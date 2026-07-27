//! Expression lowering: every form leaves exactly one value on the operand stack (or none, for a
//! `void` call).

use alloc::borrow::ToOwned as _;
use alloc::string::{String, ToString as _};

use jals_classfile::MethodDescriptor;
use jals_hir::{DefKind, MemberId, Ty};
use jals_syntax::SyntaxKind::{
    AMP, BANG_EQ, CARET, EQ, EQ_EQ, GT, LT, LT_EQ, MINUS, PERCENT, PIPE, PLUS, SLASH, STAR,
};
use jals_syntax::ast::{self, AstNode as _};

use crate::desc::Descriptor;
use crate::jvm::{Assembler, BinOp, Branch, Compare};
use crate::lower::slots::Slots;
use crate::lower::{Context, LowerError, Result};

/// Expression lowering.
pub(crate) struct Expr;

impl Expr {
    /// Emit `expr`, leaving its value on the stack.
    pub(crate) fn lower(
        expr: &ast::Expr,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        match expr {
            ast::Expr::Literal(literal) => Self::literal(literal, context, asm),
            ast::Expr::Paren(paren) => {
                Self::lower(&Self::inner(paren.expr())?, context, asm, slots)
            }
            ast::Expr::NameRef(name) => Self::name(name, context, asm, slots),
            ast::Expr::FieldAccess(access) => Self::field(access, context, asm, slots),
            ast::Expr::Call(call) => Self::call(call, context, asm, slots),
            ast::Expr::Binary(binary) => Self::binary(binary, context, asm, slots),
            ast::Expr::Assignment(assignment) => Self::assignment(assignment, context, asm, slots),
            ast::Expr::Unary(unary) => Self::unary(unary, context, asm, slots),
            _ => Err(LowerError::Unsupported("this expression form")),
        }
    }

    fn inner(expr: Option<ast::Expr>) -> Result<ast::Expr> {
        expr.ok_or(LowerError::Unsupported("an expression with no operand"))
    }

    /// The type inference recorded for `node`.
    fn ty<'a>(context: &'a Context<'_>, node: &jals_syntax::SyntaxNode) -> Result<&'a Ty> {
        context
            .inference
            .type_of_expr(Context::span(node))
            .ok_or(LowerError::Descriptor(crate::desc::DescError::Unknown))
    }

    fn literal(
        literal: &ast::Literal,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
    ) -> Result<()> {
        use jals_hir::Primitive;
        use jals_syntax::SyntaxKind::{
            CHAR_LITERAL, FALSE_KW, FLOAT_LITERAL, INT_LITERAL, NULL_KW, STRING_LITERAL, TRUE_KW,
        };
        let token = literal
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
            .ok_or(LowerError::Unsupported("an empty literal"))?;
        let text = token.text();
        match token.kind() {
            TRUE_KW => asm.const_int(1)?,
            FALSE_KW => asm.const_int(0)?,
            NULL_KW => asm.const_null()?,
            STRING_LITERAL => asm.const_string(&Self::string_value(text)?)?,
            CHAR_LITERAL => {
                let value = Self::string_value(text)?
                    .chars()
                    .next()
                    .ok_or(LowerError::Unsupported("an empty character literal"))?;
                asm.const_int(value as i32)?;
            }
            // The lexer has one integer kind and one floating kind; the `L` / `f` suffix decides
            // the width, and inference has already turned that suffix into a type. Reading the type
            // rather than re-reading the suffix keeps the two from disagreeing.
            INT_LITERAL => {
                let value = Self::integer(text.trim_end_matches(['l', 'L']))?;
                if matches!(
                    Self::ty(context, literal.syntax())?,
                    Ty::Primitive(Primitive::Long)
                ) {
                    asm.const_long(value)?;
                } else {
                    // Every integer literal is parsed as `i64` so that a `long` one fits; an
                    // `int`-typed literal is in range by construction, and an out-of-range one is
                    // the linter's to report. Masking to the low 32 bits makes the narrowing
                    // explicit and total rather than a truncating cast.
                    let low = u32::try_from(value.cast_unsigned() & 0xFFFF_FFFF).unwrap_or(0);
                    asm.const_int(low.cast_signed())?;
                }
            }
            FLOAT_LITERAL => {
                let text = text.trim_end_matches(['f', 'F', 'd', 'D']);
                let unreadable = || {
                    LowerError::Unsupported("a floating-point literal this lowering cannot read")
                };
                match Self::ty(context, literal.syntax())? {
                    Ty::Primitive(Primitive::Float) => {
                        asm.const_float(text.parse().map_err(|_| unreadable())?)?;
                    }
                    _ => asm.const_double(text.parse().map_err(|_| unreadable())?)?,
                }
            }
            _ => return Err(LowerError::Unsupported("this literal kind")),
        }
        Ok(())
    }

    /// An integer literal's value, in whichever base its prefix names, with `_` separators removed.
    fn integer(text: &str) -> Result<i64> {
        let cleaned = text.replace('_', "");
        let (digits, radix) = match cleaned.get(..2).map(str::to_ascii_lowercase).as_deref() {
            Some("0x") => (&cleaned[2..], 16),
            Some("0b") => (&cleaned[2..], 2),
            _ if cleaned.len() > 1 && cleaned.starts_with('0') => (&cleaned[1..], 8),
            _ => (cleaned.as_str(), 10),
        };
        // Parsing as unsigned first accepts `0x8000_0000_0000_0000`, which is a legal `long`
        // literal whose value is negative — the source spells the bit pattern, not the number.
        i64::from_str_radix(digits, radix)
            .or_else(|_| u64::from_str_radix(digits, radix).map(u64::cast_signed))
            .map_err(|_| LowerError::Unsupported("an integer literal this lowering cannot read"))
    }

    /// The text between a literal's delimiters: exactly **one** quote comes off each end.
    ///
    /// `trim_end_matches` took every trailing quote, so `"a\""` — whose last two characters are an
    /// escaped quote and the closing one — lost both and compiled to `a`. An unterminated literal
    /// the lexer recovered still yields its text rather than nothing.
    fn unquote(text: &str) -> &str {
        let open = text
            .strip_prefix('"')
            .or_else(|| text.strip_prefix('\''))
            .unwrap_or(text);
        open.strip_suffix('"')
            .or_else(|| open.strip_suffix('\''))
            .unwrap_or(open)
    }

    /// A string / char literal's value, with its quotes stripped and escapes resolved.
    ///
    /// An escape this does not know is reported rather than approximated. Pushing the character
    /// after the backslash — the old fallback — turned `A` into `u0041` and `\101` into `101`,
    /// which is a string constant that is simply wrong, in a class file nothing downstream checks.
    fn string_value(text: &str) -> Result<String> {
        let inner = Self::unquote(text);
        let unknown = || LowerError::Unsupported("an escape sequence this lowering cannot read");
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '\\' {
                out.push(character);
                continue;
            }
            match chars.next().ok_or_else(unknown)? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                's' => out.push(' '),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\\' => out.push('\\'),
                // JLS §3.3: a unicode escape may carry any number of `u`s, and the four hex digits
                // after the last one name one UTF-16 code unit. A lone surrogate is a code unit
                // Rust's `char` cannot hold, so it is reported rather than silently replaced.
                'u' => {
                    while chars.peek() == Some(&'u') {
                        chars.next();
                    }
                    let mut digits = String::with_capacity(4);
                    for _ in 0..4 {
                        digits.push(chars.next().ok_or_else(unknown)?);
                    }
                    let unit = u32::from_str_radix(&digits, 16).map_err(|_| unknown())?;
                    out.push(char::from_u32(unit).ok_or_else(unknown)?);
                }
                // JLS §3.10.7: one to three octal digits, and at most `\377` — so a leading digit
                // above `3` takes only one more.
                first @ '0'..='7' => {
                    let mut value = u32::from(first as u8 - b'0');
                    let remaining = if first <= '3' { 2 } else { 1 };
                    for _ in 0..remaining {
                        let Some(&digit @ '0'..='7') = chars.peek() else {
                            break;
                        };
                        chars.next();
                        value = value * 8 + u32::from(digit as u8 - b'0');
                    }
                    out.push(char::from_u32(value).ok_or_else(unknown)?);
                }
                _ => return Err(unknown()),
            }
        }
        Ok(out)
    }

    /// A bare name: a local, a parameter, or an unqualified field of the enclosing type.
    fn name(
        name: &ast::NameRef,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        let text = name.syntax().text().to_string();
        let unresolved = || LowerError::Unresolved(text.trim().into());
        let id = context.def_at(name.syntax()).ok_or_else(unresolved)?;
        if let Some(slot) = slots.slot_of(id) {
            asm.load(slot)?;
            return Ok(());
        }
        // Not a local, so the name is one of the enclosing type's own fields, written without the
        // `this.` the JVM still requires. A field declaration is a file-local definition like any
        // other, so its id maps straight back to the indexed member.
        let declaration = context.resolved.def(id);
        let member = context
            .index
            .member_by_decl(context.file, declaration.name_range.start)
            .ok_or_else(unresolved)?;
        let (owner, field, descriptor) = Self::field_ref(member, context)?;
        if context.index.member(member).modifiers.is_static {
            asm.get_static(&owner, &field, &descriptor)?;
        } else {
            asm.load(0)?;
            asm.get_field(&owner, &field, &descriptor)?;
        }
        Ok(())
    }

    /// `receiver.name`: a field read, `static` or instance.
    fn field(
        access: &ast::FieldAccess,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        let member = context
            .inference
            .field_target_of(Context::span(access.syntax()))
            .ok_or_else(|| LowerError::Unresolved(access.field().unwrap_or_default()))?;
        let (owner, name, descriptor) = Self::field_ref(member, context)?;
        if context.index.member(member).modifiers.is_static {
            asm.get_static(&owner, &name, &descriptor)?;
        } else {
            Self::lower(&Self::inner(access.receiver())?, context, asm, slots)?;
            asm.get_field(&owner, &name, &descriptor)?;
        }
        Ok(())
    }

    /// The `(owner, name, descriptor)` triple a `Fieldref` names.
    fn field_ref(member: MemberId, context: &Context<'_>) -> Result<(String, String, String)> {
        let owner = Descriptor::internal_name(
            context
                .index
                .item(context.index.member(member).owner)
                .fqn
                .as_str(),
        );
        let descriptor = Descriptor::field_descriptor(member, context.index)?.to_string();
        Ok((owner, context.index.member(member).name.clone(), descriptor))
    }

    /// A call, dispatched by how the selected member is reached.
    fn call(
        call: &ast::CallExpr,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        let member = context
            .inference
            .call_target_of(Context::span(call.syntax()))
            .ok_or_else(|| {
                LowerError::Unresolved(call.syntax().text().to_string().trim().into())
            })?;
        let info = context.index.member(member);
        let owner_item = context.index.item(info.owner);
        let owner = Descriptor::internal_name(owner_item.fqn.as_str());
        let interface_owner = owner_item.kind == DefKind::Interface;
        let constructor = info.kind == DefKind::Constructor;
        let descriptor = MethodDescriptor::to_string(&Descriptor::method_descriptor(
            member,
            context.index,
            constructor,
        )?);
        let is_static = info.modifiers.is_static;
        let is_private = info.modifiers.is_private;
        // A constructor is declared under its class's name and invoked under `<init>`. The index
        // records the declaration, so the JVM's spelling is supplied here rather than read.
        let name = if constructor {
            "<init>".to_owned()
        } else {
            info.name.clone()
        };

        // The receiver comes first on the stack, below the arguments.
        if !is_static {
            match call.callee() {
                Some(ast::Expr::FieldAccess(access)) => {
                    Self::lower(&Self::inner(access.receiver())?, context, asm, slots)?;
                }
                // A bare call in an instance method is an implicit `this`.
                _ => asm.load(0)?,
            }
        }
        for argument in call.args().into_iter().flat_map(|list| list.args()) {
            Self::lower(&argument, context, asm, slots)?;
        }

        if is_static {
            asm.invoke_static(&owner, &name, &descriptor, interface_owner)?;
        } else if is_private || constructor {
            // A `private` method is not dispatched: the call site already knows the one body it can
            // reach, and `invokevirtual` would look it up in a table it is not in.
            asm.invoke_special(&owner, &name, &descriptor, interface_owner)?;
        } else if interface_owner {
            asm.invoke_interface(&owner, &name, &descriptor)?;
        } else {
            asm.invoke_virtual(&owner, &name, &descriptor)?;
        }
        Ok(())
    }

    fn unary(
        unary: &ast::UnaryExpr,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        let operand = Self::inner(unary.operand())?;
        match Self::operator(unary.syntax()).as_slice() {
            // Unary `+` is a no-op past numeric promotion, which the operand's own type already is.
            [PLUS] => Self::lower(&operand, context, asm, slots),
            _ => Err(LowerError::Unsupported("this unary operator")),
        }
    }

    fn binary(
        binary: &ast::BinaryExpr,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        let (left, right) = (Self::inner(binary.lhs())?, Self::inner(binary.rhs())?);
        let operator = Self::operator(binary.syntax());
        if let Some(compare) = Self::comparison(&operator) {
            return Self::compare(&left, &right, compare, context, asm, slots);
        }
        let arithmetic = match operator.as_slice() {
            [PLUS] => BinOp::Add,
            [MINUS] => BinOp::Sub,
            [STAR] => BinOp::Mul,
            [SLASH] => BinOp::Div,
            [PERCENT] => BinOp::Rem,
            [AMP | PIPE | CARET] => return Err(LowerError::Unsupported("a bitwise operator")),
            _ => return Err(LowerError::Unsupported("this binary operator")),
        };
        let result = Self::ty(context, binary.syntax())?.clone();
        if matches!(result, Ty::Class(_)) {
            // String concatenation lowers to a `StringBuilder` chain, which arrives with the
            // milestone that models the chain rather than a single `+`.
            return Err(LowerError::Unsupported("string concatenation"));
        }
        // One opcode family, one operand type: `ladd` takes two `long`s. Java's binary numeric
        // promotion would widen the narrower side first, and until that is lowered a mixed pair is
        // reported here rather than left to the assembler's `TypeMismatch`, which names no
        // construct.
        let verification = Self::verification_type(&result)?;
        for operand in [&left, &right] {
            if Self::verification_type(Self::ty(context, operand.syntax())?)? != verification {
                return Err(LowerError::Unsupported(
                    "a binary operator over two different numeric types",
                ));
            }
        }
        Self::lower(&left, context, asm, slots)?;
        Self::lower(&right, context, asm, slots)?;
        asm.binary(arithmetic, &verification)?;
        Ok(())
    }

    /// A comparison, materialised as a `boolean` on the stack: branch to a `1`, else fall into a
    /// `0` and jump over it.
    fn compare(
        left: &ast::Expr,
        right: &ast::Expr,
        compare: Compare,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        // `if_icmp*` compares two `int`s. A `long` / `float` / `double` needs an `lcmp` / `fcmp` /
        // `dcmp` first and a reference needs `if_acmp*`; neither is lowered yet. Checking here
        // rather than leaving it to the assembler names the construct instead of the opcode.
        for operand in [left, right] {
            if !Self::is_int_like(Self::ty(context, operand.syntax())?) {
                return Err(LowerError::Unsupported("a comparison of this type"));
            }
        }
        Self::lower(left, context, asm, slots)?;
        Self::lower(right, context, asm, slots)?;
        let taken = asm.label();
        let done = asm.label();
        asm.branch(Branch::IntCmp(compare), taken)?;
        asm.const_int(0)?;
        asm.branch(Branch::Always, done)?;
        asm.bind(taken)?;
        asm.const_int(1)?;
        asm.bind(done)?;
        Ok(())
    }

    fn assignment(
        assignment: &ast::AssignmentExpr,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        // `x = v` and `x += v` are the same node kind, so the operator has to be read: a compound
        // assignment reads the target, applies an operator, and narrows the result back to the
        // target's type, none of which this lowers. Emitting it as a plain store would be a silent
        // miscompile — the one outcome this crate reports rather than produces.
        if !assignment.is_simple() {
            return Err(LowerError::Unsupported("a compound assignment"));
        }
        let target = Self::inner(assignment.target())?;
        let value = Self::inner(assignment.value())?;
        let ast::Expr::NameRef(name) = &target else {
            return Err(LowerError::Unsupported("assignment to this target"));
        };
        let id = context
            .def_at(name.syntax())
            .ok_or_else(|| LowerError::Unresolved(name.syntax().text().to_string()))?;
        let slot = slots
            .slot_of(id)
            .ok_or_else(|| LowerError::Unresolved(name.syntax().text().to_string()))?;

        Self::lower(&value, context, asm, slots)?;
        // An assignment is an expression whose value is the assigned one, so the value has to
        // survive the store. `dup` before storing is how javac does it too.
        asm.dup()?;
        asm.store(slot)?;
        Ok(())
    }

    /// The comparison a token sequence spells, if it is one.
    fn comparison(operator: &[jals_syntax::SyntaxKind]) -> Option<Compare> {
        Some(match operator {
            [EQ_EQ] => Compare::Eq,
            [BANG_EQ] => Compare::Ne,
            [LT] => Compare::Lt,
            [LT_EQ] => Compare::Le,
            [GT] => Compare::Gt,
            // `>=` is two tokens. The lexer never joins a `>` to what follows, so that `List<List<T>>`
            // closes as two `>` rather than one shift operator.
            [GT, EQ] => Compare::Ge,
            _ => return None,
        })
    }

    /// The operator tokens of a unary or binary expression, in order.
    ///
    /// The CST holds them as plain tokens between the operands rather than in a labelled slot, and
    /// there can be more than one — see [`comparison`](Self::comparison).
    fn operator(node: &jals_syntax::SyntaxNode) -> alloc::vec::Vec<jals_syntax::SyntaxKind> {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect()
    }

    /// Whether a value of `ty` occupies one stack word as an `int` — the representation every
    /// integral type narrower than `long` shares, and the only one `if_icmp*` accepts.
    const fn is_int_like(ty: &Ty) -> bool {
        use jals_hir::Primitive;
        matches!(
            ty,
            Ty::Primitive(
                Primitive::Boolean
                    | Primitive::Byte
                    | Primitive::Short
                    | Primitive::Char
                    | Primitive::Int
            )
        )
    }

    /// The verification type an arithmetic result has, which is what selects the opcode family.
    const fn verification_type(ty: &Ty) -> Result<jals_classfile::VerificationType> {
        use jals_classfile::VerificationType as Vt;
        use jals_hir::Primitive;
        Ok(match ty {
            Ty::Primitive(Primitive::Long) => Vt::Long,
            Ty::Primitive(Primitive::Float) => Vt::Float,
            Ty::Primitive(Primitive::Double) => Vt::Double,
            // Every narrower integral type computes as `int` on the operand stack.
            Ty::Primitive(_) => Vt::Integer,
            _ => return Err(LowerError::Unsupported("arithmetic on a reference type")),
        })
    }
}
