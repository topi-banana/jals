//! Expression lowering: every form leaves exactly one value on the operand stack (or none, for a
//! `void` call).
//!
//! # One opcode names one type
//!
//! This is what most of the file is about. `ladd` adds two `long`s and there is no opcode that adds a
//! `long` to an `int`, so Java's numeric promotions (JLS §5.6) are not a formality here — they are
//! instructions, and leaving one out produces a class file the verifier rejects or, worse, one that
//! runs and computes something else. Three separate rules apply:
//!
//! - **Binary numeric promotion** (§5.6.2) converts both operands of an arithmetic or comparison
//!   operator to one type. Both operand types therefore have to be known *before* either is lowered,
//!   because the left one is converted while it is still alone on the stack.
//! - **Unary numeric promotion** (§5.6.1) takes anything narrower than `int` to `int`, which is what
//!   makes `-aByte` an `int` expression and what a shift applies to each side separately.
//! - **Assignment conversion** (§5.2) is [`lower_as`](Expr::lower_as): the conversion an argument, a
//!   `return` value, or an assigned value goes through on its way to a known target type.
//!
//! The narrow integral types (`byte` / `short` / `char`) are the ones a conversion gets wrong
//! silently, because all three share the `int` representation on the operand stack. That is why
//! [`Repr`] exists rather than reading a [`VerificationType`](jals_classfile::VerificationType).

use alloc::borrow::ToOwned as _;
use alloc::string::{String, ToString as _};

use jals_classfile::MethodDescriptor;
use jals_hir::{DefId, DefKind, MemberId, Primitive, Ty};
use jals_syntax::SyntaxKind::{
    AMP, AMP_AMP, BANG, BANG_EQ, CARET, EQ_EQ, GT, INSTANCEOF_KW, LSHIFT, LT, LT_EQ, MINUS,
    MINUS_MINUS, PERCENT, PIPE, PIPE_PIPE, PLUS, PLUS_PLUS, SLASH, STAR, TILDE,
};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxKind, SyntaxNode};

use crate::desc::{DescError, Descriptor};
use crate::jvm::{BinOp, Branch, Compare, Numeric};
use crate::lower::place::Place;
use crate::lower::{Context, Emit, LowerError, Result};

/// The builder a string concatenation runs through.
const STRING_BUILDER: &str = "java/lang/StringBuilder";

/// How a type is represented where a conversion has to decide what to emit.
///
/// Coarser than a [`Ty`] and finer than a
/// [`VerificationType`](jals_classfile::VerificationType). Coarser because every reference behaves
/// the same here — a widening reference conversion costs nothing. Finer because `byte`, `short`,
/// `char`, and `int` are one verification type and four different conversion targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repr {
    /// A numeric primitive, converted with one of the `x2y` opcodes.
    Number(Numeric),
    /// `boolean`. It shares the `int` representation and has no conversion to or from anything, so
    /// it is neither a number nor a reference.
    Boolean,
    /// A reference — a class, an array, `null`, or an erased type variable.
    Reference,
}

impl Repr {
    fn of(ty: &Ty) -> Result<Self> {
        Ok(match ty {
            Ty::Primitive(Primitive::Boolean) => Self::Boolean,
            Ty::Primitive(primitive) => Self::Number(match primitive {
                Primitive::Byte => Numeric::Byte,
                Primitive::Short => Numeric::Short,
                Primitive::Char => Numeric::Char,
                Primitive::Int => Numeric::Int,
                Primitive::Long => Numeric::Long,
                Primitive::Float => Numeric::Float,
                Primitive::Double => Numeric::Double,
                // Matched above; the compiler cannot see that from the outer arm.
                Primitive::Boolean => return Err(DescError::Unknown.into()),
            }),
            Ty::Class(_) | Ty::Array(_) | Ty::Null | Ty::TypeVar { .. } => Self::Reference,
            Ty::Void => return Err(DescError::Void.into()),
            Ty::Unknown => return Err(DescError::Unknown.into()),
        })
    }

    /// The numeric type this is, or `None` for a `boolean` or a reference.
    const fn number(self) -> Option<Numeric> {
        match self {
            Self::Number(numeric) => Some(numeric),
            Self::Boolean | Self::Reference => None,
        }
    }
}

/// Expression lowering.
pub(crate) struct Expr;

impl Expr {
    /// Emit `expr`, leaving its value on the stack.
    pub(crate) fn lower(
        expr: &ast::Expr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        match expr {
            ast::Expr::Literal(literal) => Self::literal(literal, context, emit),
            ast::Expr::Paren(paren) => Self::lower(&Self::inner(paren.expr())?, context, emit),
            ast::Expr::NameRef(name) => Self::name(name, context, emit),
            ast::Expr::FieldAccess(access) => Self::field(access, context, emit),
            ast::Expr::Call(call) => Self::call(call, context, emit),
            ast::Expr::Binary(binary) => Self::binary(binary, context, emit),
            ast::Expr::Assignment(assignment) => Self::assignment(assignment, context, emit),
            ast::Expr::Unary(unary) => Self::unary(unary, context, emit),
            ast::Expr::Postfix(postfix) => Self::postfix(postfix, context, emit),
            ast::Expr::Index(index) => Self::index(index, context, emit),
            ast::Expr::Cast(cast) => Self::cast(cast, context, emit),
            ast::Expr::Ternary(ternary) => Self::ternary(ternary, context, emit),
            ast::Expr::New(new) => Self::new_expr(new, context, emit),
            ast::Expr::Switch(switch) => {
                crate::lower::switch::Switch::expression(switch, context, emit)
            }
            ast::Expr::ClassLiteral(literal) => Self::class_literal(literal, context, emit),
            // An array initialiser has no type of its own — `{1, 2, 3}` is an array of whatever it is
            // assigned to — so it is normally reached through `lower_as`, which knows the target.
            // Inference does record one where it could work it out, which covers a declaration.
            ast::Expr::ArrayInit(init) => {
                let ty = Self::type_of(init.syntax(), context)?;
                Self::array_initializer(init, &ty, context, emit)
            }
            // Both need `invokedynamic` and a `BootstrapMethods` attribute, and the constant pool has
            // no `MethodHandle` / `MethodType` / `InvokeDynamic` builder yet. Each names itself rather
            // than sharing a catch-all: a report that says only "this expression form" sends a reader
            // looking for which one.
            ast::Expr::Lambda(_) => Err(LowerError::Unsupported("a lambda")),
            ast::Expr::MethodRef(_) => Err(LowerError::Unsupported("a method reference")),
        }
    }

    /// Emit `expr` and convert its value to `target`.
    ///
    /// The assignment and method-invocation conversions of JLS §5.2 / §5.3, which is where
    /// `long x = 1;` gets its `i2l` and `f(1)` gets its widening to a `double` parameter.
    pub(crate) fn lower_as(
        expr: &ast::Expr,
        target: &Ty,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // An array initialiser is the one expression whose type comes from its *target*: `{1, 2, 3}`
        // has none of its own, and `byte[] b = {1, 2, 3}` stores bytes where `int[] i = {1, 2, 3}`
        // stores ints. So the conversion runs before the value rather than after it.
        if let ast::Expr::ArrayInit(init) = expr {
            return Self::array_initializer(init, target, context, emit);
        }
        Self::lower(expr, context, emit)?;
        let target_repr = Repr::of(target)?;
        let source = Self::source_repr(expr, context, emit)?;
        // Boxing crosses the primitive / reference boundary, which no conversion opcode does — it is a
        // `valueOf` or an `xxxValue` call, and which one depends on the *names* on either side rather
        // than on the representations.
        match (source, target_repr) {
            (Repr::Number(_) | Repr::Boolean, Repr::Reference) => Self::box_value(source, emit),
            (Repr::Reference, Repr::Number(_) | Repr::Boolean) => {
                Self::unbox_value(expr, target_repr, context, emit)
            }
            _ => Self::coerce(source, target_repr, emit),
        }
    }

    /// Box the primitive on top of the stack into its own wrapper (JLS §5.1.7).
    ///
    /// *Its own* — boxing never widens on the way. `Long l = 1;` is not a Java program precisely
    /// because that would take two conversions, so the wrapper is read off the value's own type and a
    /// widening *reference* conversion (to `Object`, to `Number`) then costs nothing.
    fn box_value(source: Repr, emit: &mut Emit<'_, '_>) -> Result<()> {
        let (wrapper, descriptor) = match source {
            Repr::Boolean => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
            Repr::Number(Numeric::Byte) => ("java/lang/Byte", "(B)Ljava/lang/Byte;"),
            Repr::Number(Numeric::Short) => ("java/lang/Short", "(S)Ljava/lang/Short;"),
            Repr::Number(Numeric::Char) => ("java/lang/Character", "(C)Ljava/lang/Character;"),
            Repr::Number(Numeric::Int) => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
            Repr::Number(Numeric::Long) => ("java/lang/Long", "(J)Ljava/lang/Long;"),
            Repr::Number(Numeric::Float) => ("java/lang/Float", "(F)Ljava/lang/Float;"),
            Repr::Number(Numeric::Double) => ("java/lang/Double", "(D)Ljava/lang/Double;"),
            Repr::Reference => return Err(LowerError::Unsupported("a boxing conversion")),
        };
        Ok(emit
            .asm
            .invoke_static(wrapper, "valueOf", descriptor, false)?)
    }

    /// Unbox the wrapper on top of the stack, then widen if the target is wider (JLS §5.1.8).
    ///
    /// The accessor comes from the *source* type, because only a wrapper has one: `Object` does not
    /// unbox, and an unindexed external type is a reference whatever its name suggests.
    fn unbox_value(
        expr: &ast::Expr,
        target: Repr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let source = Self::type_of(expr.syntax(), context)?;
        let wrapper = Descriptor::class_entry(&source, context.index)?;
        let (accessor, descriptor, unboxed) = match wrapper.as_str() {
            "java/lang/Boolean" => ("booleanValue", "()Z", Repr::Boolean),
            "java/lang/Byte" => ("byteValue", "()B", Repr::Number(Numeric::Byte)),
            "java/lang/Short" => ("shortValue", "()S", Repr::Number(Numeric::Short)),
            "java/lang/Character" => ("charValue", "()C", Repr::Number(Numeric::Char)),
            "java/lang/Integer" => ("intValue", "()I", Repr::Number(Numeric::Int)),
            "java/lang/Long" => ("longValue", "()J", Repr::Number(Numeric::Long)),
            "java/lang/Float" => ("floatValue", "()F", Repr::Number(Numeric::Float)),
            "java/lang/Double" => ("doubleValue", "()D", Repr::Number(Numeric::Double)),
            _ => return Err(LowerError::Unsupported("an unboxing conversion")),
        };
        emit.asm.invoke_virtual(&wrapper, accessor, descriptor)?;
        // `long n = someInteger;` unboxes to an `int` and then widens, which is one conversion more
        // than the accessor gives.
        Self::coerce(unboxed, target, emit)
    }

    /// Emit `expr` and convert its value to the numeric type `target`.
    ///
    /// Through [`lower_as`](Self::lower_as), which is where boxing lives: an `Integer` operand of an
    /// arithmetic operator unboxes first (JLS §5.6.2), and that is one call rather than a case here.
    fn lower_to(
        expr: &ast::Expr,
        target: Numeric,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        Self::lower_as(expr, &Self::ty_of(target), context, emit)
    }

    /// The representation of the value `expr` just left on the stack.
    ///
    /// Inference's record first, and the *stack* when it has none. The fallback is coarser — every
    /// integral type narrower than `long` reads as `int` there — but it is never wrong, because it
    /// reads what is actually on the stack rather than guessing: converting a `char` value as if it
    /// were an `int` is the same instruction, and precision below `int` only matters on the target
    /// side, which is always known. That keeps a gap in inference from failing a whole method.
    fn source_repr(expr: &ast::Expr, context: &Context<'_>, emit: &Emit<'_, '_>) -> Result<Repr> {
        use jals_classfile::VerificationType as Vt;
        if let Ok(ty) = Self::type_of(expr.syntax(), context)
            && let Ok(repr) = Repr::of(&ty)
        {
            return Ok(repr);
        }
        let top = emit.asm.stack_top().ok_or(DescError::Unknown)?;
        Ok(match top {
            Vt::Integer => Repr::Number(Numeric::Int),
            Vt::Long => Repr::Number(Numeric::Long),
            Vt::Float => Repr::Number(Numeric::Float),
            Vt::Double => Repr::Number(Numeric::Double),
            _ => Repr::Reference,
        })
    }

    /// Convert the value on top of the stack between two representations.
    fn coerce(source: Repr, target: Repr, emit: &mut Emit<'_, '_>) -> Result<()> {
        match (source, target) {
            (Repr::Number(from), Repr::Number(to)) => Ok(emit.asm.convert(from, to)?),
            // Two references need nothing: a widening reference conversion is free, and a narrowing
            // one only happens through a cast, which emits its own `checkcast`.
            //
            // `boolean` against `boolean` needs nothing either, and so does `boolean` against `int`:
            // they are the same word on the stack and Java defines no conversion between them, so a
            // pair landing there is a gap in inference rather than a program, and emitting nothing is
            // the honest answer.
            (Repr::Reference, Repr::Reference)
            | (Repr::Boolean, Repr::Boolean | Repr::Number(Numeric::Int))
            | (Repr::Number(Numeric::Int), Repr::Boolean) => Ok(()),
            // Crossing the primitive / reference boundary is boxing, which is a `valueOf` or an
            // `intValue` call rather than a conversion opcode.
            (Repr::Reference, _) | (_, Repr::Reference) => {
                Err(LowerError::Unsupported("a boxing conversion"))
            }
            _ => Err(LowerError::Unsupported("a conversion between these types")),
        }
    }

    fn inner(expr: Option<ast::Expr>) -> Result<ast::Expr> {
        expr.ok_or(LowerError::Unsupported("an expression with no operand"))
    }

    /// The type inference recorded for `node`, if it worked one out.
    ///
    /// [`Ty::Unknown`] counts as no answer rather than as an answer: it is what inference records
    /// where it gave up, and treating it as a type would turn a gap into a wrong descriptor.
    pub(crate) fn type_of(node: &SyntaxNode, context: &Context<'_>) -> Result<Ty> {
        match context.inference.type_of_expr(Context::span(node)) {
            Some(Ty::Unknown) | None => Err(DescError::Unknown.into()),
            Some(ty) => Ok(ty.clone()),
        }
    }

    /// Binary numeric promotion (JLS §5.6.2): the one type both operands are converted to.
    const fn promote(left: Numeric, right: Numeric) -> Numeric {
        match (left, right) {
            (Numeric::Double, _) | (_, Numeric::Double) => Numeric::Double,
            (Numeric::Float, _) | (_, Numeric::Float) => Numeric::Float,
            (Numeric::Long, _) | (_, Numeric::Long) => Numeric::Long,
            // Everything narrower than `long` computes as `int`.
            _ => Numeric::Int,
        }
    }

    /// Unary numeric promotion (JLS §5.6.1): everything narrower than `int` becomes `int`.
    const fn promote_one(numeric: Numeric) -> Numeric {
        match numeric {
            Numeric::Byte | Numeric::Short | Numeric::Char | Numeric::Int => Numeric::Int,
            other => other,
        }
    }

    /// The numeric type `node`'s recorded type is, reported if it is not one.
    fn numeric_of(node: &SyntaxNode, context: &Context<'_>) -> Result<Numeric> {
        let ty = Self::type_of(node, context)?;
        if let Some(numeric) = Repr::of(&ty)?.number() {
            return Ok(numeric);
        }
        // Binary numeric promotion unboxes before it promotes (JLS §5.6.2), so a wrapper counts as the
        // primitive it wraps — `total += someInteger` is ordinary Java.
        Self::unboxed(&ty, context).ok_or(LowerError::Unsupported(
            "an arithmetic operand of this type",
        ))
    }

    /// The primitive a wrapper type unboxes to, if it is one.
    fn unboxed(ty: &Ty, context: &Context<'_>) -> Option<Numeric> {
        let entry = Descriptor::class_entry(ty, context.index).ok()?;
        Some(match entry.as_str() {
            "java/lang/Byte" => Numeric::Byte,
            "java/lang/Short" => Numeric::Short,
            "java/lang/Character" => Numeric::Char,
            "java/lang/Integer" => Numeric::Int,
            "java/lang/Long" => Numeric::Long,
            "java/lang/Float" => Numeric::Float,
            "java/lang/Double" => Numeric::Double,
            _ => return None,
        })
    }

    fn literal(
        literal: &ast::Literal,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
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
            TRUE_KW => emit.asm.const_int(1)?,
            FALSE_KW => emit.asm.const_int(0)?,
            NULL_KW => emit.asm.const_null()?,
            STRING_LITERAL => emit.asm.const_string(&Self::literal_text(text)?)?,
            CHAR_LITERAL => {
                let value = Self::literal_text(text)?
                    .chars()
                    .next()
                    .ok_or(LowerError::Unsupported("an empty character literal"))?;
                emit.asm.const_int(value as i32)?;
            }
            // The lexer has one integer kind and one floating kind; the `L` / `f` suffix decides
            // the width, and inference has already turned that suffix into a type. Reading the type
            // rather than re-reading the suffix keeps the two from disagreeing.
            INT_LITERAL => {
                let value = Self::integer_literal(text.trim_end_matches(['l', 'L']))?;
                if matches!(
                    Self::type_of(literal.syntax(), context),
                    Ok(Ty::Primitive(Primitive::Long))
                ) {
                    emit.asm.const_long(value)?;
                } else {
                    // Every integer literal is parsed as `i64` so that a `long` one fits; an
                    // `int`-typed literal is in range by construction, and an out-of-range one is
                    // the linter's to report. Masking to the low 32 bits makes the narrowing
                    // explicit and total rather than a truncating cast.
                    let low = u32::try_from(value.cast_unsigned() & 0xFFFF_FFFF).unwrap_or(0);
                    emit.asm.const_int(low.cast_signed())?;
                }
            }
            FLOAT_LITERAL => {
                let text = text.trim_end_matches(['f', 'F', 'd', 'D']);
                let unreadable = || {
                    LowerError::Unsupported("a floating-point literal this lowering cannot read")
                };
                if matches!(
                    Self::type_of(literal.syntax(), context),
                    Ok(Ty::Primitive(Primitive::Float))
                ) {
                    emit.asm
                        .const_float(text.parse().map_err(|_| unreadable())?)?;
                } else {
                    emit.asm
                        .const_double(text.parse().map_err(|_| unreadable())?)?;
                }
            }
            _ => return Err(LowerError::Unsupported("this literal kind")),
        }
        Ok(())
    }

    /// An integer literal's value, in whichever base its prefix names, with `_` separators removed.
    pub(crate) fn integer_literal(text: &str) -> Result<i64> {
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
    pub(crate) fn literal_text(text: &str) -> Result<String> {
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
    fn name(name: &ast::NameRef, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        // `this` is not a name that resolves to anything — it is slot 0, which is why it has no
        // identifier token to look up.
        if Self::is_this(name.syntax()) {
            return emit.load_this();
        }
        let text = name.syntax().text().to_string();
        let unresolved = || LowerError::Unresolved(text.trim().into());
        let id = context.def_at(name.syntax()).ok_or_else(unresolved)?;
        if let Some(slot) = emit.slots.slot_of(id) {
            emit.asm.load(slot)?;
            return Ok(());
        }
        // A captured local is not a local *here*: it lives in a synthetic field the constructor filled.
        if let Some(read) = Self::captured_read(id, context)? {
            emit.load_this()?;
            emit.asm.get_field(&context.this_class, &read.0, &read.1)?;
            return Ok(());
        }
        let member = Self::own_field(id, context).ok_or_else(unresolved)?;
        let (owner, field, descriptor) = Self::field_ref(member, context)?;
        if context.index.member(member).modifiers.is_static {
            emit.asm.get_static(&owner, &field, &descriptor)?;
        } else {
            emit.load_this()?;
            emit.asm.get_field(&owner, &field, &descriptor)?;
        }
        Ok(())
    }

    /// The indexed member a file-local definition declares, when that definition is a field of the
    /// enclosing type rather than a local.
    ///
    /// A name that is not a local is one of the enclosing type's own fields, written without the
    /// `this.` the JVM still requires. A field declaration is a file-local definition like any
    /// other, so its id maps straight back to the indexed member.
    /// Whether a `NAME_REF` node is the bare `this`.
    pub(crate) fn is_this(node: &SyntaxNode) -> bool {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| token.kind() == jals_syntax::SyntaxKind::THIS_KW)
    }

    /// The `(field, descriptor)` a captured local is read through, or `None` when `id` is not one of the
    /// current class's captures.
    pub(crate) fn captured_read(
        id: DefId,
        context: &Context<'_>,
    ) -> Result<Option<(String, String)>> {
        if !context.captures_local(id) {
            return Ok(None);
        }
        Ok(Some((
            alloc::format!("val${}", context.resolved.def(id).name),
            Descriptor::descriptor_of(context.inference.type_of_def(id), context.index)?
                .to_string(),
        )))
    }

    pub(crate) fn own_field(id: DefId, context: &Context<'_>) -> Option<MemberId> {
        let declaration = context.resolved.def(id);
        context
            .index
            .member_by_decl(context.file, declaration.name_range.start)
    }

    /// `receiver.name`: a field read, `static` or instance.
    fn field(
        access: &ast::FieldAccess,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // `a.length` on an array is not a field at all: the JVM answers it with an instruction, so
        // there is no member for the index to have resolved.
        if access.field().as_deref() == Some("length")
            && let Some(receiver) = access.receiver()
            && matches!(Self::type_of(receiver.syntax(), context), Ok(Ty::Array(_)))
        {
            Self::lower(&receiver, context, emit)?;
            emit.asm.array_length()?;
            return Ok(());
        }
        let member = context
            .inference
            .field_target_of(Context::span(access.syntax()))
            .ok_or_else(|| LowerError::Unresolved(access.field().unwrap_or_default()))?;
        let (owner, name, descriptor) = Self::field_ref(member, context)?;
        if context.index.member(member).modifiers.is_static {
            emit.asm.get_static(&owner, &name, &descriptor)?;
        } else {
            Self::lower(&Self::inner(access.receiver())?, context, emit)?;
            emit.asm.get_field(&owner, &name, &descriptor)?;
        }
        Ok(())
    }

    /// The `(owner, name, descriptor)` triple a `Fieldref` names.
    pub(crate) fn field_ref(
        member: MemberId,
        context: &Context<'_>,
    ) -> Result<(String, String, String)> {
        let owner = Descriptor::internal_name_of(context.index.member(member).owner, context.index);
        let descriptor = Descriptor::field_descriptor(member, context.index)?.to_string();
        Ok((owner, context.index.member(member).name.clone(), descriptor))
    }

    /// A call's arguments, each converted to its *declared* parameter type.
    ///
    /// `f(1)` against `f(long)` is an `i2l`, which JLS §5.3 calls the method-invocation conversion.
    ///
    /// A **varargs** call's trailing arguments are packed into an array first, because the JVM has no
    /// variable arity at all: `f(int...)`'s descriptor is `([I)V` and the call site builds the `int[]`.
    /// One argument that is already an array of the right type passes straight through instead — that
    /// is JLS §15.12.4.2's rule, and packing it would produce an `int[][]`.
    fn arguments(
        arguments: &[ast::Expr],
        params: &[Ty],
        varargs: bool,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        if !varargs {
            for (index, argument) in arguments.iter().enumerate() {
                let declared = params
                    .get(index)
                    .ok_or(LowerError::Unsupported("a call with too many arguments"))?;
                Self::lower_as(argument, declared, context, emit)?;
            }
            return Ok(());
        }
        let Some((last, fixed)) = params.split_last() else {
            return Err(LowerError::Unsupported("a varargs call with no parameters"));
        };
        for (index, argument) in arguments.iter().take(fixed.len()).enumerate() {
            Self::lower_as(argument, &fixed[index], context, emit)?;
        }
        let rest = &arguments[fixed.len().min(arguments.len())..];
        let Ty::Array(element) = last else {
            return Err(LowerError::Unsupported(
                "a varargs parameter that is no array",
            ));
        };
        // Exactly one argument, already an array: passed through rather than wrapped. Wrapping it
        // would hand the callee a one-element array *of arrays*.
        if rest.len() == 1
            && Self::type_of(rest[0].syntax(), context)
                .is_ok_and(|ty| matches!(ty, Ty::Array(_) | Ty::Null))
        {
            return Self::lower_as(&rest[0], last, context, emit);
        }
        let descriptor = Descriptor::descriptor_of(element, context.index)?.to_string();
        emit.asm.const_int(
            i32::try_from(rest.len())
                .map_err(|_| LowerError::Unsupported("a call with this many arguments"))?,
        )?;
        emit.asm.new_array(&descriptor)?;
        for (index, argument) in rest.iter().enumerate() {
            emit.asm.dup()?;
            emit.asm.const_int(
                i32::try_from(index)
                    .map_err(|_| LowerError::Unsupported("a call with this many arguments"))?,
            )?;
            Self::lower_as(argument, element, context, emit)?;
            emit.asm.array_store(&descriptor)?;
        }
        Ok(())
    }

    /// A call, dispatched by how the selected member is reached.
    fn call(call: &ast::CallExpr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        let member = context
            .inference
            .call_target_of(Context::span(call.syntax()))
            .ok_or_else(|| {
                LowerError::Unresolved(call.syntax().text().to_string().trim().into())
            })?;
        let info = context.index.member(member);
        let owner_item = context.index.item(info.owner);
        let owner = Descriptor::internal_name_of(info.owner, context.index);
        let interface_owner = owner_item.kind == DefKind::Interface;
        let constructor = info.kind == DefKind::Constructor;
        let descriptor = MethodDescriptor::to_string(&Descriptor::method_descriptor(
            member,
            context.index,
            constructor,
        )?);
        let is_static = info.modifiers.is_static;
        let is_private = info.modifiers.is_private;
        let varargs = info.varargs;
        // A constructor is declared under its class's name and invoked under `<init>`. The index
        // records the declaration, so the JVM's spelling is supplied here rather than read.
        let name = if constructor {
            "<init>".to_owned()
        } else {
            info.name.clone()
        };
        let params = context.index.resolved_param_tys(member);

        // The receiver comes first on the stack, below the arguments.
        if !is_static {
            match call.callee() {
                Some(ast::Expr::FieldAccess(access)) => {
                    Self::lower(&Self::inner(access.receiver())?, context, emit)?;
                }
                // A bare call in an instance method is an implicit `this`.
                _ => emit.load_this()?,
            }
        }
        let arguments: alloc::vec::Vec<ast::Expr> = call
            .args()
            .into_iter()
            .flat_map(|list| list.args())
            .collect();
        Self::arguments(&arguments, &params, varargs, context, emit)?;

        if is_static {
            emit.asm
                .invoke_static(&owner, &name, &descriptor, interface_owner)?;
        } else if is_private || constructor {
            // A `private` method is not dispatched: the call site already knows the one body it can
            // reach, and `invokevirtual` would look it up in a table it is not in.
            emit.asm
                .invoke_special(&owner, &name, &descriptor, interface_owner)?;
        } else if interface_owner {
            emit.asm.invoke_interface(&owner, &name, &descriptor)?;
        } else {
            emit.asm.invoke_virtual(&owner, &name, &descriptor)?;
        }
        Self::restore_erased(call.syntax(), member, context, emit)
    }

    /// Put back the static type a generic call's erased descriptor threw away.
    ///
    /// `List<String>.get(0)` returns `Object` at the JVM level — the descriptor is erased, and the
    /// substitution that makes it a `String` exists only in the analysis. So the stack says `Object`,
    /// and the next use of the value is verified against `Object` and rejected. javac emits the same
    /// `checkcast`, in the same place, for the same reason.
    fn restore_erased(
        node: &SyntaxNode,
        member: MemberId,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // A `void` call left nothing to cast, and a primitive one needs no cast — only a reference
        // whose descriptor was erased does.
        let Ok(actual) = Self::type_of(node, context) else {
            return Ok(());
        };
        if !matches!(Repr::of(&actual), Ok(Repr::Reference)) {
            return Ok(());
        }
        // The *declared* return type is what the descriptor erased; an un-substituted type variable
        // erases to `Object`, which is exactly the case that needs the cast.
        let declared = context.index.resolved_member_ty(member);
        let (Ok(erased), Ok(precise)) = (
            Descriptor::descriptor_of(&declared, context.index),
            Descriptor::descriptor_of(&actual, context.index),
        ) else {
            return Ok(());
        };
        if erased == precise {
            return Ok(());
        }
        Ok(emit
            .asm
            .check_cast(&Descriptor::class_entry(&actual, context.index)?)?)
    }

    /// `array[index]`.
    fn index(index: &ast::IndexExpr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        let place = Place::resolve(&ast::Expr::Index(index.clone()), context, emit)?;
        place.read(emit.asm)
    }

    /// `Foo.class`, `int.class`, `String[].class`.
    ///
    /// A reference type's is an `ldc` over the same `Class` entry a `checkcast` names. A *primitive*
    /// has no such entry — there is no `Class` constant for `int` — so `int.class` reads the
    /// `TYPE` field its wrapper carries for exactly this purpose, which is what javac emits too.
    fn class_literal(
        literal: &ast::ClassLiteral,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // `String[].class`'s dimension brackets sit on the literal itself rather than inside the type,
        // so they are counted here and wrapped around whatever the base names.
        let dimensions = literal
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == jals_syntax::SyntaxKind::LBRACK)
            .count();
        // `void.class` is the one literal whose base is not a value type at all, so it never reaches
        // `ty_of_type` — and `Void` is the wrapper that carries its `TYPE`.
        if dimensions == 0
            && literal
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == jals_syntax::SyntaxKind::VOID_KW)
        {
            return Ok(emit
                .asm
                .get_static("java/lang/Void", "TYPE", "Ljava/lang/Class;")?);
        }
        let mut named = match (literal.ty(), literal.expr()) {
            (Some(ty), _) => context.ty_of_type(&ty)?,
            // The reference form's base is parsed as an *expression* — a bare `String` is a name
            // reference — so it is resolved as a type name rather than lowered as a value.
            (None, Some(base)) => context.ty_of_name(base.syntax())?,
            (None, None) => return Err(LowerError::Unsupported("a `.class` with no type")),
        };
        for _ in 0..dimensions {
            named = Ty::Array(alloc::boxed::Box::new(named));
        }
        // A primitive *array* is still a reference, so only an undimensioned primitive takes the
        // `TYPE` route.
        if let Ty::Primitive(primitive) = &named {
            let wrapper = Self::wrapper_of(*primitive);
            return Ok(emit.asm.get_static(wrapper, "TYPE", "Ljava/lang/Class;")?);
        }
        Ok(emit
            .asm
            .const_class(&Descriptor::class_entry(&named, context.index)?)?)
    }

    /// The wrapper class carrying a primitive's `TYPE` field.
    const fn wrapper_of(primitive: Primitive) -> &'static str {
        match primitive {
            Primitive::Boolean => "java/lang/Boolean",
            Primitive::Byte => "java/lang/Byte",
            Primitive::Short => "java/lang/Short",
            Primitive::Char => "java/lang/Character",
            Primitive::Int => "java/lang/Integer",
            Primitive::Long => "java/lang/Long",
            Primitive::Float => "java/lang/Float",
            Primitive::Double => "java/lang/Double",
        }
    }

    /// `new Foo(args)`, `new T[n]`, `new T[n][m]`, or `new T[]{…}`.
    ///
    /// One node kind, two unrelated operations, told apart by whether an argument list is present:
    /// the grammar puts an object creation's `(…)` and an array creation's `[…]` in the same place.
    fn new_expr(new: &ast::NewExpr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        if new.args().is_none() {
            return Self::new_array(new, context, emit);
        }
        if new.body().is_some() {
            return Err(LowerError::Unsupported("an anonymous class"));
        }

        let unresolved = || LowerError::Unresolved(new.syntax().text().to_string().trim().into());
        let arguments: alloc::vec::Vec<ast::Expr> = new
            .args()
            .into_iter()
            .flat_map(|list| list.args())
            .collect();
        let selected = context
            .inference
            .call_target_of(Context::span(new.syntax()));
        // A class that declares no constructor has the implicit no-argument one (JLS §8.8.9), and
        // *nothing declared it* — so there is no indexed member for selection to have found, and
        // `new Foo()` on the most ordinary class in Java arrives with none. Its descriptor is fixed.
        let (owner, descriptor, params) = if let Some(member) = selected {
            (
                Descriptor::internal_name_of(context.index.member(member).owner, context.index),
                MethodDescriptor::to_string(&Descriptor::method_descriptor(
                    member,
                    context.index,
                    true,
                )?),
                context.index.resolved_param_tys(member),
            )
        } else {
            let Ty::Class(jals_hir::ClassTy::Project { id, .. }) =
                Self::type_of(new.syntax(), context)?
            else {
                return Err(unresolved());
            };
            let declares_one = context
                .index
                .own_members(id)
                .iter()
                .any(|&member| context.index.member(member).kind == DefKind::Constructor);
            if declares_one || !arguments.is_empty() {
                // Either the class has constructors and none of them accepted these arguments, or it
                // has none and was handed some anyway. Both are the linter's to report.
                return Err(unresolved());
            }
            (
                Descriptor::internal_name_of(id, context.index),
                "()V".to_owned(),
                alloc::vec::Vec::new(),
            )
        };

        // An inner class's constructor takes the enclosing instance first, and the descriptor the index
        // computed does not carry it — the declaration never wrote it. The qualifier names it when the
        // source wrote one (`outer.new Inner()`); otherwise it is `this`.
        let target = selected
            .map(|member| context.index.member(member).owner)
            .or_else(|| {
                Self::type_of(new.syntax(), context)
                    .ok()
                    .and_then(|ty| ty.project_id())
            });
        let enclosing = target.and_then(|item| context.inner.get(&item).cloned());
        // A local class's captures are trailing parameters, appended in the order the class reads them.
        let captured = target
            .and_then(|item| context.captures_of_item(item))
            .unwrap_or_default();
        let mut descriptor = match &enclosing {
            Some(name) => alloc::format!("(L{name};{}", descriptor.trim_start_matches('(')),
            None => descriptor,
        };
        if !captured.is_empty() {
            let mut trailing = String::new();
            for &id in &captured {
                trailing.push_str(
                    &Descriptor::descriptor_of(context.inference.type_of_def(id), context.index)?
                        .to_string(),
                );
            }
            descriptor = descriptor.replace(')', &alloc::format!("{trailing})"));
        }

        emit.asm.new_object(&owner)?;
        // The constructor consumes one reference and returns nothing, so the expression's own value
        // has to be a second copy — made *before* the arguments go on top of it.
        emit.asm.dup()?;
        if enclosing.is_some() {
            match new.qualifier() {
                Some(qualifier) => Self::lower(&qualifier, context, emit)?,
                None => emit.asm.load(0)?,
            }
        }
        let varargs = selected.is_some_and(|member| context.index.member(member).varargs);
        Self::arguments(&arguments, &params, varargs, context, emit)?;
        // The captured values come last, read from wherever they live *here* — a local of the enclosing
        // method, or this class's own capture field when one local class creates another.
        for &id in &captured {
            if let Some(slot) = emit.slots.slot_of(id) {
                emit.asm.load(slot)?;
            } else if let Some((field, field_descriptor)) = Self::captured_read(id, context)? {
                emit.load_this()?;
                emit.asm
                    .get_field(&context.this_class, &field, &field_descriptor)?;
            } else {
                return Err(unresolved());
            }
        }
        Ok(emit
            .asm
            .invoke_special(&owner, "<init>", &descriptor, false)?)
    }

    /// An array creation.
    ///
    /// Three shapes with three different instructions. `new T[n]` is a `newarray` or an `anewarray`;
    /// `new T[n][m]` is one `multianewarray` allocating both levels at once, and `new T[n][]` is the
    /// same instruction with only the levels it was given; `new T[]{…}` allocates and then stores.
    fn new_array(new: &ast::NewExpr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        let created = Self::type_of(new.syntax(), context)?;
        let Ty::Array(element) = &created else {
            return Err(LowerError::Unsupported("a `new` of an unindexed type"));
        };

        // An initialiser supplies the length, so it is checked for before the bracket forms.
        if let Some(init) = new.syntax().children().find_map(ast::ArrayInit::cast) {
            return Self::array_initializer(&init, &created, context, emit);
        }

        // The lengths are the bracket contents, in order; the *dimensions* are the bracket pairs,
        // which `new T[n][]` gives more of than lengths.
        let lengths: alloc::vec::Vec<ast::Expr> = new
            .syntax()
            .children()
            .filter(|child| child.kind() != jals_syntax::SyntaxKind::ARRAY_INIT)
            .filter_map(ast::Expr::cast)
            .collect();
        let dimensions = new
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == jals_syntax::SyntaxKind::LBRACK)
            .count();
        if lengths.is_empty() {
            return Err(LowerError::Unsupported("an array creation with no length"));
        }

        let int = Ty::Primitive(Primitive::Int);
        for length in &lengths {
            Self::lower_as(length, &int, context, emit)?;
        }
        if dimensions <= 1 {
            let descriptor = Descriptor::descriptor_of(element, context.index)?.to_string();
            return Ok(emit.asm.new_array(&descriptor)?);
        }
        let descriptor = Descriptor::descriptor_of(&created, context.index)?.to_string();
        let given = u8::try_from(lengths.len())
            .map_err(|_| LowerError::Unsupported("an array of this many dimensions"))?;
        Ok(emit.asm.new_multi_array(&descriptor, given)?)
    }

    /// `{a, b, c}` as an array of `target`'s element type.
    ///
    /// Allocate, then store each element through a duplicated reference — which is what makes the
    /// array itself the expression's value while every `*astore` consumed a copy of it.
    fn array_initializer(
        init: &ast::ArrayInit,
        target: &Ty,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let Ty::Array(element) = target else {
            return Err(LowerError::Unsupported(
                "an array initialiser outside an array type",
            ));
        };
        let descriptor = Descriptor::descriptor_of(element, context.index)?.to_string();
        let elements: alloc::vec::Vec<ast::Expr> = init.elements().collect();
        let length = i32::try_from(elements.len())
            .map_err(|_| LowerError::Unsupported("an array initialiser this long"))?;

        emit.asm.const_int(length)?;
        emit.asm.new_array(&descriptor)?;
        for (index, value) in elements.iter().enumerate() {
            emit.asm.dup()?;
            emit.asm.const_int(
                i32::try_from(index)
                    .map_err(|_| LowerError::Unsupported("an array initialiser this long"))?,
            )?;
            // A nested initialiser is an array of the element type, which is itself an array type —
            // so the same routine answers `{{1, 2}, {3}}`.
            Self::lower_as(value, element, context, emit)?;
            emit.asm.array_store(&descriptor)?;
        }
        Ok(())
    }

    /// `(T) e`.
    ///
    /// Two unrelated operations under one syntax. A primitive cast is a conversion opcode — and the
    /// only place a *narrowing* one appears without an assignment. A reference cast is a
    /// `checkcast`, which computes nothing and exists so the failure happens here rather than at the
    /// next `invokevirtual`.
    fn cast(cast: &ast::CastExpr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        let target = Self::type_of(cast.syntax(), context)?;
        let operand = Self::inner(cast.expr())?;
        match Repr::of(&target)? {
            Repr::Number(_) | Repr::Boolean => Self::lower_as(&operand, &target, context, emit),
            Repr::Reference => {
                Self::lower(&operand, context, emit)?;
                if !matches!(Self::source_repr(&operand, context, emit)?, Repr::Reference) {
                    return Err(LowerError::Unsupported("a boxing conversion"));
                }
                let entry = Descriptor::class_entry(&target, context.index)?;
                emit.asm.check_cast(&entry)?;
                Ok(())
            }
        }
    }

    /// `c ? a : b`.
    fn ternary(
        ternary: &ast::TernaryExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let mut parts = ternary.parts();
        let condition = Self::inner(parts.next())?;
        let then = Self::inner(parts.next())?;
        let otherwise = Self::inner(parts.next())?;
        let result = Self::conditional_ty(&then, &otherwise, ternary.syntax(), context)?;

        let else_arm = emit.asm.label();
        let done = emit.asm.label();
        Self::lower(&condition, context, emit)?;
        emit.asm.branch(Branch::IntZero(Compare::Eq), else_arm)?;
        Self::lower_as(&then, &result, context, emit)?;
        emit.asm.branch(Branch::Always, done)?;
        emit.asm.bind(else_arm)?;
        Self::lower_as(&otherwise, &result, context, emit)?;
        emit.asm.bind(done)?;

        // Two different references merge to `Object`, because that is all a frame can say without a
        // class hierarchy to walk. The static type has to be put back, or the next instruction that
        // uses the value is verified against `Object` and rejected.
        let arms_agree = Self::type_of(then.syntax(), context).ok()
            == Self::type_of(otherwise.syntax(), context).ok();
        if matches!(Repr::of(&result)?, Repr::Reference) && !arms_agree {
            let entry = Descriptor::class_entry(&result, context.index)?;
            emit.asm.check_cast(&entry)?;
        }
        Ok(())
    }

    /// The type a conditional expression produces.
    ///
    /// Inference answers this when the two arms agree exactly and deliberately not otherwise: it
    /// keeps a "never a false type" guarantee that a least-upper-bound walk over a class hierarchy
    /// would break. The numeric half of JLS §15.25 needs no such walk, though — two numeric arms
    /// produce their binary numeric promotion, the same rule every arithmetic operator follows — so
    /// that case is worked out here rather than reported.
    fn conditional_ty(
        then: &ast::Expr,
        otherwise: &ast::Expr,
        node: &SyntaxNode,
        context: &Context<'_>,
    ) -> Result<Ty> {
        if let Ok(ty) = Self::type_of(node, context) {
            return Ok(ty);
        }
        let left = Self::numeric_of(then.syntax(), context)?;
        let right = Self::numeric_of(otherwise.syntax(), context)?;
        Ok(Self::ty_of(Self::promote(left, right)))
    }

    /// The [`Ty`] a numeric type is, for handing back to a conversion that speaks in types.
    const fn ty_of(numeric: Numeric) -> Ty {
        Ty::Primitive(match numeric {
            Numeric::Byte => Primitive::Byte,
            Numeric::Short => Primitive::Short,
            Numeric::Char => Primitive::Char,
            Numeric::Int => Primitive::Int,
            Numeric::Long => Primitive::Long,
            Numeric::Float => Primitive::Float,
            Numeric::Double => Primitive::Double,
        })
    }

    fn unary(unary: &ast::UnaryExpr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        let operand = Self::inner(unary.operand())?;
        match Self::operator(unary.syntax()).as_slice() {
            // `+` is not a no-op: unary numeric promotion still applies, so `+aByte` is an `int`.
            [PLUS] => {
                let promoted = Self::promote_one(Self::numeric_of(operand.syntax(), context)?);
                Self::lower_to(&operand, promoted, context, emit)
            }
            [MINUS] => {
                let promoted = Self::promote_one(Self::numeric_of(operand.syntax(), context)?);
                Self::lower_to(&operand, promoted, context, emit)?;
                Ok(emit.asm.negate(&promoted.stack())?)
            }
            // `!b` is `b ^ 1`, which is what javac emits too. Verified code guarantees a canonical
            // 0 / 1 in a `boolean`, so the flip is exact and needs no branch.
            [BANG] => {
                Self::lower(&operand, context, emit)?;
                emit.asm.const_int(1)?;
                Ok(emit
                    .asm
                    .binary(BinOp::Xor, &jals_classfile::VerificationType::Integer)?)
            }
            // `~n` is `n ^ -1`, at the promoted width.
            [TILDE] => {
                let promoted = Self::promote_one(Self::numeric_of(operand.syntax(), context)?);
                Self::lower_to(&operand, promoted, context, emit)?;
                if promoted == Numeric::Long {
                    emit.asm.const_long(-1)?;
                } else {
                    emit.asm.const_int(-1)?;
                }
                Ok(emit.asm.binary(BinOp::Xor, &promoted.stack())?)
            }
            [PLUS_PLUS] => Self::update(&operand, 1, true, context, emit),
            [MINUS_MINUS] => Self::update(&operand, -1, true, context, emit),
            _ => Err(LowerError::Unsupported("this unary operator")),
        }
    }

    fn postfix(
        postfix: &ast::PostfixExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let operand = Self::inner(postfix.operand())?;
        match Self::operator(postfix.syntax()).as_slice() {
            [PLUS_PLUS] => Self::update(&operand, 1, false, context, emit),
            [MINUS_MINUS] => Self::update(&operand, -1, false, context, emit),
            _ => Err(LowerError::Unsupported("this postfix operator")),
        }
    }

    /// `++x` / `x++` / `--x` / `x--`.
    ///
    /// One shape for all four, because JLS §15.14 / §15.15 give them one meaning: read the place, add
    /// `delta` at the promoted type, narrow the result back to the place's own type, and write. Only
    /// *which* value the expression yields differs — the prefix form the new one, the postfix form
    /// the old.
    ///
    /// The narrowing is not optional. `byte b = 127; b++` has to wrap to -128, which is the `i2b`
    /// after the `iadd`; without it the `byte` field would hold 128.
    fn update(
        target: &ast::Expr,
        delta: i8,
        prefix: bool,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let place = Place::resolve(target, context, emit)?;
        let declared = Repr::of(place.ty())?
            .number()
            .ok_or(LowerError::Unsupported("an increment of this type"))?;

        // An `int` local is the one place the JVM updates without going through the stack. `i++` in a
        // `for` header is this one instruction; the general path below is five.
        if let Place::Local { slot, .. } = &place
            && declared == Numeric::Int
        {
            if prefix {
                emit.asm.increment(*slot, i16::from(delta))?;
                emit.asm.load(*slot)?;
            } else {
                emit.asm.load(*slot)?;
                emit.asm.increment(*slot, i16::from(delta))?;
            }
            return Ok(());
        }

        place.dup_address(emit.asm)?;
        place.read(emit.asm)?;
        if !prefix {
            // The postfix form yields the value from *before* the update, so it is re-seated under
            // the address now — the same move an assignment makes for the value it wrote.
            emit.asm.dup_below(place.words())?;
        }
        let promoted = Self::promote_one(declared);
        emit.asm.convert(declared, promoted)?;
        match promoted {
            Numeric::Long => emit.asm.const_long(i64::from(delta))?,
            Numeric::Float => emit.asm.const_float(f32::from(delta))?,
            Numeric::Double => emit.asm.const_double(f64::from(delta))?,
            _ => emit.asm.const_int(i32::from(delta))?,
        }
        emit.asm.binary(BinOp::Add, &promoted.stack())?;
        emit.asm.convert(promoted, declared)?;
        place.write(emit.asm, prefix)
    }

    fn binary(
        binary: &ast::BinaryExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let operator = Self::operator(binary.syntax());
        // `instanceof` has no right *operand*: its right-hand side is a type or a pattern, so it is
        // recognised before the two operands are taken apart.
        if operator.first() == Some(&INSTANCEOF_KW) {
            return Self::instance_of(binary, context, emit);
        }
        let (left, right) = (Self::inner(binary.lhs())?, Self::inner(binary.rhs())?);
        match operator.as_slice() {
            [AMP_AMP] => return Self::short_circuit(&left, &right, true, context, emit),
            [PIPE_PIPE] => return Self::short_circuit(&left, &right, false, context, emit),
            _ => {}
        }
        if let Some(compare) = Self::comparison(&operator) {
            return Self::compare(&left, &right, compare, context, emit);
        }

        let operation = match operator.as_slice() {
            [PLUS] => BinOp::Add,
            [MINUS] => BinOp::Sub,
            [STAR] => BinOp::Mul,
            [SLASH] => BinOp::Div,
            [PERCENT] => BinOp::Rem,
            [AMP] => BinOp::And,
            [PIPE] => BinOp::Or,
            [CARET] => BinOp::Xor,
            [LSHIFT] => BinOp::Shl,
            // `>>` and `>>>` are separate `>` tokens: the lexer never joins a `>` to what follows,
            // so that `List<List<T>>` still closes as two of them.
            [GT, GT] => BinOp::Shr,
            [GT, GT, GT] => BinOp::Ushr,
            _ => return Err(LowerError::Unsupported("this binary operator")),
        };

        // `&`, `|`, and `^` are also the *boolean* operators, evaluated without short-circuiting.
        // Both operands are already the same `int` on the stack, so there is no promotion to do.
        if matches!(operation, BinOp::And | BinOp::Or | BinOp::Xor)
            && matches!(
                Repr::of(&Self::type_of(left.syntax(), context)?)?,
                Repr::Boolean
            )
        {
            Self::lower(&left, context, emit)?;
            Self::lower(&right, context, emit)?;
            return Ok(emit
                .asm
                .binary(operation, &jals_classfile::VerificationType::Integer)?);
        }

        // A `+` whose result is a `String` is concatenation, not addition, and it shares this node
        // kind.
        // Asked of the recorded type rather than required of it: `someInteger + 1`'s result is an
        // `int` that inference leaves unknown, and an unknown result is certainly not a `String`.
        if operation == BinOp::Add
            && Self::type_of(binary.syntax(), context).is_ok_and(|ty| Self::is_string(&ty, context))
        {
            return Self::concat(binary, context, emit);
        }

        let left_numeric = Self::numeric_of(left.syntax(), context)?;
        if operation.is_shift() {
            // A shift promotes each side on its own (JLS §5.6.1 twice, not §5.6.2 once): the result
            // has the *left* operand's promoted type, and the count is always an `int` because that
            // is the only thing `lshl` takes.
            let promoted = Self::promote_one(left_numeric);
            Self::lower_to(&left, promoted, context, emit)?;
            Self::lower_to(&right, Numeric::Int, context, emit)?;
            return Ok(emit.asm.binary(operation, &promoted.stack())?);
        }

        let promoted = Self::promote(left_numeric, Self::numeric_of(right.syntax(), context)?);
        Self::lower_to(&left, promoted, context, emit)?;
        Self::lower_to(&right, promoted, context, emit)?;
        Ok(emit.asm.binary(operation, &promoted.stack())?)
    }

    /// `a + b` where the result is a `String`.
    ///
    /// A `StringBuilder` chain, which is what javac emitted before it moved to `invokedynamic`:
    /// allocate one builder for the whole expression, `append` each operand at its own static type,
    /// and `toString`.
    ///
    /// **The flattening is the point.** `a + b + c` parses as `(a + b) + c`, and lowering each `+`
    /// on its own would build the left string, hand it to a second builder, and throw it away. So the
    /// tree is walked to its leaves first, and only a `+` whose own result is a `String` continues the
    /// chain — `"x" + (1 + 2)` appends the sum 3, not the digits.
    fn concat(
        binary: &ast::BinaryExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        emit.asm.new_object(STRING_BUILDER)?;
        emit.asm.dup()?;
        emit.asm
            .invoke_special(STRING_BUILDER, "<init>", "()V", false)?;
        Self::append(&ast::Expr::Binary(binary.clone()), context, emit)?;
        Ok(emit
            .asm
            .invoke_virtual(STRING_BUILDER, "toString", "()Ljava/lang/String;")?)
    }

    /// Append `expr` to the builder on top of the stack, flattening a nested concatenation.
    fn append(expr: &ast::Expr, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        match expr {
            // Parentheses group the *tree*, not the chain: `("a" + 1) + 2` is still one chain.
            ast::Expr::Paren(paren) => {
                return Self::append(&Self::inner(paren.expr())?, context, emit);
            }
            ast::Expr::Binary(binary)
                if Self::operator(binary.syntax()).as_slice() == [PLUS]
                    && Self::is_string(&Self::type_of(binary.syntax(), context)?, context) =>
            {
                Self::append(&Self::inner(binary.lhs())?, context, emit)?;
                return Self::append(&Self::inner(binary.rhs())?, context, emit);
            }
            _ => {}
        }
        Self::lower(expr, context, emit)?;
        // Which overload the operand's own type names. Getting this wrong is not a verification
        // failure: sending a `char` to `append(int)` prints its code point.
        let descriptor = match Self::source_repr(expr, context, emit)? {
            Repr::Boolean => "(Z)Ljava/lang/StringBuilder;",
            Repr::Number(Numeric::Char) => "(C)Ljava/lang/StringBuilder;",
            Repr::Number(Numeric::Long) => "(J)Ljava/lang/StringBuilder;",
            Repr::Number(Numeric::Float) => "(F)Ljava/lang/StringBuilder;",
            Repr::Number(Numeric::Double) => "(D)Ljava/lang/StringBuilder;",
            // `byte` and `short` have no overload of their own; they are already `int`s here.
            Repr::Number(_) => "(I)Ljava/lang/StringBuilder;",
            Repr::Reference => {
                if Self::type_of(expr.syntax(), context)
                    .is_ok_and(|ty| Self::is_string(&ty, context))
                {
                    "(Ljava/lang/String;)Ljava/lang/StringBuilder;"
                } else {
                    // `append(Object)` runs `String.valueOf`, which is what turns a `null` into
                    // `"null"` rather than throwing.
                    "(Ljava/lang/Object;)Ljava/lang/StringBuilder;"
                }
            }
        };
        // `append` consumes the builder as its receiver and hands the same one back, which is what
        // makes the chain a chain: the stack has exactly one builder on it before and after.
        Ok(emit
            .asm
            .invoke_virtual(STRING_BUILDER, "append", descriptor)?)
    }

    /// Whether `ty` is `java.lang.String`, however it was named.
    ///
    /// Both spellings count. A concatenation's own type comes out of inference as an *external*
    /// `String` rather than as the indexed stub, because the operator synthesises it rather than
    /// reading it off a declaration.
    pub(crate) fn is_string(ty: &Ty, context: &Context<'_>) -> bool {
        match ty {
            Ty::Class(jals_hir::ClassTy::Project { id, .. }) => {
                context.index.item(*id).fqn.as_str() == "java.lang.String"
            }
            Ty::Class(jals_hir::ClassTy::External { name, .. }) => {
                name == "String" || name == "java.lang.String"
            }
            _ => false,
        }
    }

    /// `e instanceof T`.
    fn instance_of(
        binary: &ast::BinaryExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // A pattern binds a name that is in scope only where the test succeeded, which is a
        // flow-sensitive scoping rule this lowering does not model. A plain type test binds nothing.
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        if binary.syntax().children().any(|child| {
            matches!(
                child.kind(),
                TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
            )
        }) {
            return Err(LowerError::Unsupported("an `instanceof` pattern"));
        }
        let operand = Self::inner(binary.lhs())?;
        let ty = binary
            .syntax()
            .children()
            .find_map(ast::Type::cast)
            .ok_or(LowerError::Unsupported("an `instanceof` with no type"))?;
        let target = context.ty_of_type(&ty)?;
        Self::lower(&operand, context, emit)?;
        Ok(emit
            .asm
            .instance_of(&Descriptor::class_entry(&target, context.index)?)?)
    }

    /// `a && b` / `a || b`, which evaluate `b` only when `a` did not already decide the answer.
    ///
    /// Materialised as a `boolean` rather than folded into the enclosing branch. A `boolean` is what
    /// the expression *is* — it can be assigned, returned, or passed — and the enclosing `if` tests
    /// it with the one `ifeq` it would have emitted anyway.
    fn short_circuit(
        left: &ast::Expr,
        right: &ast::Expr,
        conjunction: bool,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // `&&` jumps out on a false operand and `||` on a true one, and the value it jumps *to* is
        // the answer that operand already settled — false for `&&`, true for `||`.
        let decided = emit.asm.label();
        let done = emit.asm.label();
        let escape = if conjunction {
            Branch::IntZero(Compare::Eq)
        } else {
            Branch::IntZero(Compare::Ne)
        };
        for operand in [left, right] {
            Self::lower(operand, context, emit)?;
            emit.asm.branch(escape, decided)?;
        }
        emit.asm.const_int(i32::from(conjunction))?;
        emit.asm.branch(Branch::Always, done)?;
        emit.asm.bind(decided)?;
        emit.asm.const_int(i32::from(!conjunction))?;
        emit.asm.bind(done)?;
        Ok(())
    }

    /// A comparison, materialised as a `boolean` on the stack: branch to a `1`, else fall into a
    /// `0` and jump over it.
    fn compare(
        left: &ast::Expr,
        right: &ast::Expr,
        compare: Compare,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let taken = emit.asm.label();
        let done = emit.asm.label();
        Self::branch_compare(left, right, compare, taken, context, emit)?;
        emit.asm.const_int(0)?;
        emit.asm.branch(Branch::Always, done)?;
        emit.asm.bind(taken)?;
        emit.asm.const_int(1)?;
        emit.asm.bind(done)?;
        Ok(())
    }

    /// Emit `left <compare> right` as a branch taken exactly when it holds.
    ///
    /// Dispatched on the operands' *representation* rather than on the operator, because that is what
    /// decides which comparison the JVM has: two numbers are promoted to one type first, two
    /// `boolean`s already share the `int` one, and two references have `if_acmp*` — for equality
    /// only, which the assembler enforces.
    fn branch_compare(
        left: &ast::Expr,
        right: &ast::Expr,
        compare: Compare,
        target: crate::jvm::Label,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        use jals_classfile::VerificationType as Vt;
        let (left_repr, right_repr) = (
            Repr::of(&Self::type_of(left.syntax(), context)?)?,
            Repr::of(&Self::type_of(right.syntax(), context)?)?,
        );
        if let (Some(a), Some(b)) = (left_repr.number(), right_repr.number()) {
            let promoted = Self::promote(a, b);
            Self::lower_to(left, promoted, context, emit)?;
            Self::lower_to(right, promoted, context, emit)?;
            return Ok(emit
                .asm
                .branch_compare(&promoted.stack(), compare, target)?);
        }
        // Not a numeric pair. Both `boolean`s compare as `int`s; anything else is a reference pair,
        // which `Null` names for the assembler's "any reference" check.
        let ty = match (left_repr, right_repr) {
            (Repr::Boolean, Repr::Boolean) => Vt::Integer,
            (Repr::Reference, Repr::Reference) => Vt::Null,
            _ => return Err(LowerError::Unsupported("a comparison of these two types")),
        };
        Self::lower(left, context, emit)?;
        Self::lower(right, context, emit)?;
        Ok(emit.asm.branch_compare(&ty, compare, target)?)
    }

    fn assignment(
        assignment: &ast::AssignmentExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let target = Self::inner(assignment.target())?;
        let value = Self::inner(assignment.value())?;
        if assignment.is_simple() {
            let place = Place::resolve(&target, context, emit)?;
            let ty = place.ty().clone();
            Self::lower_as(&value, &ty, context, emit)?;
            return place.write(emit.asm, true);
        }
        let operation = Self::compound_operator(assignment.syntax())?;
        Self::compound(&target, &value, operation, context, emit)
    }

    /// The operator a compound assignment fuses in.
    ///
    /// Most arrive as one token (`PLUS_EQ`), but the right shifts do not: the lexer never joins a `>`
    /// to what follows, so `>>=` is `GT GT EQ` and `>>>=` is `GT GT GT EQ`.
    fn compound_operator(node: &SyntaxNode) -> Result<BinOp> {
        use jals_syntax::SyntaxKind::{
            AMP_EQ, CARET_EQ, EQ, LSHIFT_EQ, MINUS_EQ, PERCENT_EQ, PIPE_EQ, PLUS_EQ, SLASH_EQ,
            STAR_EQ,
        };
        let operator: alloc::vec::Vec<_> = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect();
        Ok(match operator.as_slice() {
            [PLUS_EQ] => BinOp::Add,
            [MINUS_EQ] => BinOp::Sub,
            [STAR_EQ] => BinOp::Mul,
            [SLASH_EQ] => BinOp::Div,
            [PERCENT_EQ] => BinOp::Rem,
            [AMP_EQ] => BinOp::And,
            [PIPE_EQ] => BinOp::Or,
            [CARET_EQ] => BinOp::Xor,
            [LSHIFT_EQ] => BinOp::Shl,
            [GT, GT, EQ] => BinOp::Shr,
            [GT, GT, GT, EQ] => BinOp::Ushr,
            _ => return Err(LowerError::Unsupported("this compound assignment operator")),
        })
    }

    /// `E1 op= E2`.
    ///
    /// JLS §15.26.2 defines it as `E1 = (T)((E1) op (E2))`, where `T` is `E1`'s type — so the operator
    /// runs at the *promoted* type and the result is narrowed back. Both narrowings are load-bearing:
    /// `int i; i += 1L` is `i2l`, `ladd`, `l2i`, and `byte b; b += 1` is `iadd`, `i2b`. Dropping
    /// either stores a value outside the variable's range, in a class file that verifies.
    fn compound(
        target: &ast::Expr,
        value: &ast::Expr,
        operation: BinOp,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let place = Place::resolve(target, context, emit)?;
        let declared = Repr::of(place.ty())?;

        // `boolean &= …` is the one compound assignment with no numeric type in it. Both sides are
        // already the same `int`, so there is nothing to promote or narrow.
        if declared == Repr::Boolean {
            if !matches!(operation, BinOp::And | BinOp::Or | BinOp::Xor) {
                return Err(LowerError::Unsupported("this operator on a `boolean`"));
            }
            place.dup_address(emit.asm)?;
            place.read(emit.asm)?;
            Self::lower(value, context, emit)?;
            emit.asm
                .binary(operation, &jals_classfile::VerificationType::Integer)?;
            return place.write(emit.asm, true);
        }

        // `s += x` on a `String` is a concatenation, and the only compound assignment whose operator
        // is not an arithmetic one.
        if operation == BinOp::Add && Self::is_string(place.ty(), context) {
            place.dup_address(emit.asm)?;
            place.read(emit.asm)?;
            emit.asm.new_object(STRING_BUILDER)?;
            emit.asm.dup()?;
            emit.asm
                .invoke_special(STRING_BUILDER, "<init>", "()V", false)?;
            // The old value was read before the builder existed, so the two are the wrong way round.
            emit.asm.swap()?;
            emit.asm.invoke_virtual(
                STRING_BUILDER,
                "append",
                "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            )?;
            Self::append(value, context, emit)?;
            emit.asm
                .invoke_virtual(STRING_BUILDER, "toString", "()Ljava/lang/String;")?;
            return place.write(emit.asm, true);
        }

        let Some(declared) = declared.number() else {
            return Err(LowerError::Unsupported(
                "a compound assignment of this type",
            ));
        };
        let promoted = if operation.is_shift() {
            Self::promote_one(declared)
        } else {
            Self::promote(declared, Self::numeric_of(value.syntax(), context)?)
        };

        place.dup_address(emit.asm)?;
        place.read(emit.asm)?;
        emit.asm.convert(declared, promoted)?;
        let right = if operation.is_shift() {
            Numeric::Int
        } else {
            promoted
        };
        Self::lower_to(value, right, context, emit)?;
        emit.asm.binary(operation, &promoted.stack())?;
        // The implicit cast back to `E1`'s type, which is the half of §15.26.2 that is easy to lose.
        emit.asm.convert(promoted, declared)?;
        place.write(emit.asm, true)
    }

    /// The comparison a token sequence spells, if it is one.
    fn comparison(operator: &[SyntaxKind]) -> Option<Compare> {
        Some(match operator {
            [EQ_EQ] => Compare::Eq,
            [BANG_EQ] => Compare::Ne,
            [LT] => Compare::Lt,
            [LT_EQ] => Compare::Le,
            [GT] => Compare::Gt,
            // `>=` is two tokens. The lexer never joins a `>` to what follows, so that `List<List<T>>`
            // closes as two `>` rather than one shift operator.
            [GT, jals_syntax::SyntaxKind::EQ] => Compare::Ge,
            _ => return None,
        })
    }

    /// The operator tokens of a unary or binary expression, in order.
    ///
    /// The CST holds them as plain tokens between the operands rather than in a labelled slot, and
    /// there can be more than one — see [`comparison`](Self::comparison).
    fn operator(node: &SyntaxNode) -> alloc::vec::Vec<SyntaxKind> {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect()
    }
}
