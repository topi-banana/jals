//! Method-body decompilation: reconstructing a method body from its bytecode.
//!
//! Two layers. The value layer ([`Sim`]) is a per-block symbolic execution: the operand stack is
//! simulated as typed [`Expr`] trees, and each instruction either rewrites the stack or emits a
//! [`Stmt`]. The control layer ([`Structurer`]) builds a CFG ([`crate::cfg`]) and recovers structured
//! Java from it — a straight-line method is one block, forward conditional branches become
//! `if` / `if`-`else`, and back-edges become `while` / `do`-`while` loops. Both layers are
//! deliberately conservative: anything not modelled (a `switch`, a `try`/`catch`, a
//! `break`/`continue` or nested/irreducible loop, a non-string-concat `invokedynamic`, an exotic
//! stack shuffle, or a control-flow shape that is not a clean tree) makes the whole method fall
//! back to the caller's safe body — so the output is always valid Java, never a half-built or
//! mis-structured body.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use jals_classfile::{
    AttributeBody, BaseType, BootstrapMethod, ClassFile, CodeAttribute, ConstantPool,
    ConstantPoolEntry, FieldType, Instruction, MethodDescriptor, MethodInfo, ReturnType,
    WideInstruction,
};
use jals_exec::{LocalBoxFuture, Yielder};

use crate::attrs::Attrs;
use crate::cfg::{Cfg, Term};
use crate::expr::{ArrayForm, ConcatPart, Expr, Stmt};
use crate::hierarchy::ClassHierarchy;
use crate::literal::Literal;
use crate::types::JavaType;

/// Namespace for method-body decompilation: the entry point and its slot / declaration pre-passes.
pub struct MethodBody;

impl MethodBody {
    /// Reconstruct a method's body as indented Java statement lines, or `None` if it cannot be
    /// decompiled confidently.
    ///
    /// It returns `None` on a control-flow shape not yet modelled, an exception handler, or any
    /// unsupported instruction; the caller wraps the lines in a block and falls back to a safe
    /// placeholder on `None`.
    ///
    /// `param_names` are the exact parameter names the caller renders in the signature, in order; the
    /// body reuses them (never a name the signature doesn't declare), and a mismatch between them and
    /// the descriptor's parameters (a generic signature that hides synthetic parameters, e.g. an
    /// `enum` constructor's `String, int`) makes this bail so the body can never reference a phantom
    /// parameter. `hierarchy` contains the class files available to prove hierarchy-sensitive source
    /// forms; incomplete information only prevents those forms and does not disable unrelated body
    /// reconstruction.
    pub async fn decompile(
        method: &MethodInfo,
        cf: &ClassFile,
        param_names: &[String],
        hierarchy: &ClassHierarchy<'_>,
    ) -> Option<Vec<String>> {
        let pool = &cf.constant_pool;
        let code = method.attributes.iter().find_map(|a| match &a.body {
            AttributeBody::Code(code) => Some(code),
            _ => None,
        })?;
        // A non-empty exception table means try/catch/finally — not yet structured.
        if !code.exception_table.is_empty() {
            return None;
        }
        let owner = pool.class_name(cf.this_class)?.into_owned();
        let owner_is_interface = cf.access_flags.is_interface();
        let direct_superclass = if cf.super_class == 0 {
            None
        } else {
            Some(pool.class_name(cf.super_class)?.into_owned())
        };
        let is_static = method.access_flags.is_static();
        let descriptor = pool.utf8(method.descriptor_index)?;
        let method_descriptor = MethodDescriptor::parse(&descriptor).ok()?;
        // The class-level `BootstrapMethods` table, which an `invokedynamic` string-concat call
        // site resolves its recipe through (absent when the class has no dynamic call sites).
        let bootstrap = cf
            .attributes
            .iter()
            .find_map(|a| match &a.body {
                AttributeBody::BootstrapMethods(b) => Some(b.as_slice()),
                _ => None,
            })
            .unwrap_or(&[]);
        let mut locals = Self::local_slots(&method_descriptor.params, is_static, param_names)?;
        // Hoist a typed declaration for every non-parameter local the method stores into, registering
        // each in `locals` so the body can name it — bailing if any local cannot be resolved from the
        // `LocalVariableTable` (no `-g`, a synthetic temporary, a reused slot, or a name collision).
        let decls = Self::local_declarations(code, pool, is_static, &mut locals)?;
        let cfg = Cfg::build(&code.code).await?;
        let structurer = Structurer {
            code: &code.code,
            cfg: &cfg,
            pool,
            bootstrap,
            class: cf,
            hierarchy,
            owner,
            owner_is_interface,
            direct_superclass,
            is_static,
            locals,
            return_type: method_descriptor.return_type,
        };
        let mut stmts = decls;
        stmts.extend(structurer.structure().await?);
        Some(Self::render_body(&stmts))
    }

    /// The parameter slot → source-name map (slot 0 is `this` for an instance method and is not
    /// listed), naming each slot from `param_names`. Returns `None` when the descriptor's parameter
    /// count differs from `param_names`, so the body cannot name a slot the signature does not
    /// declare.
    fn local_slots(
        params: &[FieldType],
        is_static: bool,
        param_names: &[String],
    ) -> Option<BTreeMap<u16, Local>> {
        if params.len() != param_names.len() {
            return None;
        }
        let map = Attrs::parameter_slots(params, is_static)
            .zip(param_names)
            .map(|((slot, param), name)| {
                (
                    slot,
                    Local {
                        name: name.clone(),
                        ty: param.clone(),
                    },
                )
            })
            .collect();
        Some(map)
    }

    /// Plan the hoisted local declarations for a method: scan its bytecode for stored slots, drop
    /// `this` and the parameters (already named), and resolve each remaining slot to a typed
    /// declaration from the `LocalVariableTable`, registering its name in `locals` for the body to
    /// reference. Returns the declarations in slot order, or `None` — bailing the whole method — when
    /// a stored local has no usable LVT entry (no `-g` build, a synthetic temporary, or a reused
    /// slot) or its name collides with a parameter or another local.
    fn local_declarations(
        code: &CodeAttribute,
        pool: &ConstantPool,
        is_static: bool,
        locals: &mut BTreeMap<u16, Local>,
    ) -> Option<Vec<Stmt>> {
        // Slots written by a store / `iinc`, minus `this` (slot 0, instance) and the parameters —
        // `locals` holds exactly those slots here, before any hoisted local is registered.
        let mut stored: BTreeSet<u16> = code.code.iter().filter_map(Self::stored_slot).collect();
        if !is_static {
            stored.remove(&0);
        }
        stored.retain(|slot| !locals.contains_key(slot));
        if stored.is_empty() {
            return Some(Vec::new());
        }
        // Locals need types (a bare `var x;` is illegal), so the `LocalVariableTable` is required.
        let table = Attrs::local_variable_table(code)?;
        let mut decls = Vec::with_capacity(stored.len());
        for slot in stored {
            let (name, ty) = Attrs::local_variable(table, pool, slot)?;
            // A hoisted declaration must never shadow a parameter or an already-hoisted local.
            if locals.values().any(|local| local.name == name) {
                return None;
            }
            let rendered_ty = JavaType::render_field_type(&ty);
            locals.insert(
                slot,
                Local {
                    name: name.clone(),
                    ty,
                },
            );
            decls.push(Stmt::Declare {
                ty: rendered_ty,
                name,
            });
        }
        Some(decls)
    }

    /// The local slot and JVM kind a *store* instruction writes (a store form, its numbered
    /// shorthand, or the `wide` form), or `None` for a non-store. Shared by declaration discovery
    /// ([`MethodBody::stored_slot`]) and the simulator ([`Sim::step`]) so the two never drift.
    /// `iinc` is deliberately excluded — it read-modify-writes and carries a delta, handled
    /// separately.
    fn store_info(ins: &Instruction) -> Option<(u16, JvmKind)> {
        use Instruction as I;
        Some(match ins {
            I::Istore(slot) => (u16::from(*slot), JvmKind::Int),
            I::Lstore(slot) => (u16::from(*slot), JvmKind::Long),
            I::Fstore(slot) => (u16::from(*slot), JvmKind::Float),
            I::Dstore(slot) => (u16::from(*slot), JvmKind::Double),
            I::Astore(slot) => (u16::from(*slot), JvmKind::Reference),
            I::Istore0 => (0, JvmKind::Int),
            I::Lstore0 => (0, JvmKind::Long),
            I::Fstore0 => (0, JvmKind::Float),
            I::Dstore0 => (0, JvmKind::Double),
            I::Astore0 => (0, JvmKind::Reference),
            I::Istore1 => (1, JvmKind::Int),
            I::Lstore1 => (1, JvmKind::Long),
            I::Fstore1 => (1, JvmKind::Float),
            I::Dstore1 => (1, JvmKind::Double),
            I::Astore1 => (1, JvmKind::Reference),
            I::Istore2 => (2, JvmKind::Int),
            I::Lstore2 => (2, JvmKind::Long),
            I::Fstore2 => (2, JvmKind::Float),
            I::Dstore2 => (2, JvmKind::Double),
            I::Astore2 => (2, JvmKind::Reference),
            I::Istore3 => (3, JvmKind::Int),
            I::Lstore3 => (3, JvmKind::Long),
            I::Fstore3 => (3, JvmKind::Float),
            I::Dstore3 => (3, JvmKind::Double),
            I::Astore3 => (3, JvmKind::Reference),
            I::Wide(WideInstruction::Istore(slot)) => (*slot, JvmKind::Int),
            I::Wide(WideInstruction::Lstore(slot)) => (*slot, JvmKind::Long),
            I::Wide(WideInstruction::Fstore(slot)) => (*slot, JvmKind::Float),
            I::Wide(WideInstruction::Dstore(slot)) => (*slot, JvmKind::Double),
            I::Wide(WideInstruction::Astore(slot)) => (*slot, JvmKind::Reference),
            _ => return None,
        })
    }

    /// The local slot an instruction writes — a store (via [`MethodBody::store_info`]) or an `iinc`
    /// (and its `wide` form) — or `None` if it writes no local. Drives declaration discovery.
    fn stored_slot(ins: &Instruction) -> Option<u16> {
        use Instruction as I;
        Self::store_info(ins)
            .map(|(slot, _)| slot)
            .or_else(|| match ins {
                I::Iinc { index, .. } => Some(u16::from(*index)),
                I::Wide(WideInstruction::Iinc { index, .. }) => Some(*index),
                _ => None,
            })
    }

    /// Trim a trailing implicit `return;` (a `void` method's fall-off return) and render the rest.
    fn render_body(stmts: &[Stmt]) -> Vec<String> {
        let end = if matches!(stmts.last(), Some(Stmt::Return(None))) {
            stmts.len() - 1
        } else {
            stmts.len()
        };
        Stmt::render_block(&stmts[..end])
    }
}

#[derive(Clone)]
struct Local {
    name: String,
    ty: FieldType,
}

/// The Java source type carried alongside an operand-stack expression. `null` has no single field
/// descriptor, so it remains distinct until a reference-typed consumer accepts it.
#[derive(Clone, PartialEq, Eq)]
enum StackType {
    Field(FieldType),
    Null,
}

impl StackType {
    const fn is_reference_compatible(&self) -> bool {
        matches!(
            self,
            Self::Null | Self::Field(FieldType::Object(_) | FieldType::Array(_))
        )
    }
}

#[derive(Clone)]
struct StackValue {
    expr: Expr,
    ty: StackType,
}

impl StackValue {
    const fn field(expr: Expr, ty: FieldType) -> Self {
        Self {
            expr,
            ty: StackType::Field(ty),
        }
    }

    const fn base(expr: Expr, ty: BaseType) -> Self {
        Self::field(expr, FieldType::Base(ty))
    }

    fn object(expr: Expr, internal: impl Into<String>) -> Self {
        Self::field(expr, FieldType::Object(internal.into()))
    }

    fn int_literal(value: i32) -> Self {
        Self::base(Expr::lit(value.to_string()), BaseType::Int)
    }

    fn int_constant(&self) -> Option<i64> {
        if !matches!(self.ty, StackType::Field(FieldType::Base(BaseType::Int))) {
            return None;
        }
        self.expr.as_int_const()
    }
}

#[derive(Clone, Copy)]
enum ArrayKind {
    Int,
    Long,
    Float,
    Double,
    Reference,
    ByteOrBoolean,
    Char,
    Short,
}

#[derive(Clone, Copy)]
enum JvmKind {
    Int,
    Long,
    Float,
    Double,
    Reference,
}

impl JvmKind {
    const fn accepts(self, ty: &FieldType) -> bool {
        match self {
            Self::Int => matches!(
                ty,
                FieldType::Base(
                    BaseType::Byte
                        | BaseType::Char
                        | BaseType::Int
                        | BaseType::Short
                        | BaseType::Boolean
                )
            ),
            Self::Long => matches!(ty, FieldType::Base(BaseType::Long)),
            Self::Float => matches!(ty, FieldType::Base(BaseType::Float)),
            Self::Double => matches!(ty, FieldType::Base(BaseType::Double)),
            Self::Reference => Sim::is_reference(ty),
        }
    }
}

impl ArrayKind {
    const fn accepts(self, component: &FieldType) -> bool {
        match self {
            Self::Int => matches!(component, FieldType::Base(BaseType::Int)),
            Self::Long => matches!(component, FieldType::Base(BaseType::Long)),
            Self::Float => matches!(component, FieldType::Base(BaseType::Float)),
            Self::Double => matches!(component, FieldType::Base(BaseType::Double)),
            Self::Reference => Sim::is_reference(component),
            Self::ByteOrBoolean => matches!(
                component,
                FieldType::Base(BaseType::Byte | BaseType::Boolean)
            ),
            Self::Char => matches!(component, FieldType::Base(BaseType::Char)),
            Self::Short => matches!(component, FieldType::Base(BaseType::Short)),
        }
    }
}

enum MethodRefKind {
    Class,
    Interface,
}

/// The straight-line symbolic-execution state for one basic block.
struct Sim<'a, 'classes> {
    pool: &'a ConstantPool,
    /// The class's `BootstrapMethods` entries (empty when absent), resolving `invokedynamic`.
    bootstrap: &'a [BootstrapMethod],
    class: &'a ClassFile,
    hierarchy: &'a ClassHierarchy<'classes>,
    /// Internal binary name of the class being decompiled (for `this`-call vs object-creation).
    owner: &'a str,
    owner_is_interface: bool,
    direct_superclass: Option<&'a str>,
    is_static: bool,
    locals: &'a BTreeMap<u16, Local>,
    return_type: &'a ReturnType,
    stack: Vec<StackValue>,
    stmts: Vec<Stmt>,
}

impl Sim<'_, '_> {
    fn pop(&mut self) -> Option<StackValue> {
        Self::finalize(self.stack.pop()?)
    }

    /// Finalize a value leaving the stack: a collecting array creation becomes its final form —
    /// untouched → a plain sized creation, completely filled → a `new T[]{…}` initializer — and a
    /// partial collection or a leaked initializer-store marker bails; a collecting `StringBuilder`
    /// chain consumed by anything but its `toString()` becomes the original append call chain
    /// again. The single gate that keeps the folding sentinels ([`Expr::PendingArray`] /
    /// [`Expr::PendingArrayDup`] / [`Expr::PendingBuilder`]) out of rendered output: every
    /// consumption funnels through here (via [`Sim::pop`] or the block-end sweep).
    fn finalize(mut value: StackValue) -> Option<StackValue> {
        value.expr = match value.expr {
            Expr::PendingArrayDup => None,
            Expr::PendingBuilder(parts) => Some(Expr::builder_chain(parts)),
            Expr::PendingArray {
                elem,
                empty_dims,
                len,
                elems,
            } => {
                if elems.is_empty() {
                    Some(Expr::NewArray {
                        elem,
                        empty_dims,
                        form: ArrayForm::Sized(vec![Expr::lit(len.to_string())]),
                    })
                } else if elems.len() == len {
                    Some(Expr::NewArray {
                        elem,
                        empty_dims,
                        form: ArrayForm::Init(elems),
                    })
                } else {
                    // A partial fill (a compiler that skips default-valued element stores, e.g.
                    // ECJ) — rendering `{…}` would change the array's length, so bail.
                    None
                }
            }
            e => Some(e),
        }?;
        Some(value)
    }

    /// Pop `n` operands and return them in source (left-to-right) order.
    fn pop_values(&mut self, n: usize) -> Option<Vec<StackValue>> {
        let mut args = Vec::with_capacity(n);
        for _ in 0..n {
            args.push(self.pop()?);
        }
        args.reverse();
        Some(args)
    }

    /// Pop invocation operands and adapt each expression to its descriptor parameter type.
    fn pop_call_args(&mut self, params: &[FieldType]) -> Option<Vec<Expr>> {
        self.pop_values(params.len())?
            .into_iter()
            .zip(params)
            .map(|(value, param)| Self::consume_as(value, param))
            .collect()
    }

    const fn is_reference(ty: &FieldType) -> bool {
        matches!(ty, FieldType::Object(_) | FieldType::Array(_))
    }

    const fn is_int_numeric(base: BaseType) -> bool {
        matches!(
            base,
            BaseType::Byte | BaseType::Char | BaseType::Int | BaseType::Short
        )
    }

    const fn widens(actual: BaseType, expected: BaseType) -> bool {
        match actual {
            BaseType::Byte => matches!(
                expected,
                BaseType::Short
                    | BaseType::Int
                    | BaseType::Long
                    | BaseType::Float
                    | BaseType::Double
            ),
            BaseType::Short | BaseType::Char => matches!(
                expected,
                BaseType::Int | BaseType::Long | BaseType::Float | BaseType::Double
            ),
            BaseType::Int => matches!(
                expected,
                BaseType::Long | BaseType::Float | BaseType::Double
            ),
            BaseType::Long => matches!(expected, BaseType::Float | BaseType::Double),
            BaseType::Float => matches!(expected, BaseType::Double),
            BaseType::Double | BaseType::Boolean => false,
        }
    }

    /// Consume a stack value in a descriptor-typed Java context. Constants carried as JVM `int`s
    /// regain their narrow source spelling. Other primitive widening and reference adaptation use
    /// explicit casts so overload selection and string conversion retain the descriptor's static
    /// type; incompatible primitive conversions bail.
    fn consume_as(value: StackValue, expected: &FieldType) -> Option<Expr> {
        match (&value.ty, expected) {
            (StackType::Field(actual), expected) if actual == expected => Some(value.expr),
            (StackType::Null, expected) if Self::is_reference(expected) => Some(Expr::Cast {
                ty: JavaType::render_field_type(expected),
                expr: Box::new(value.expr),
            }),
            (StackType::Field(actual), expected)
                if Self::is_reference(actual) && Self::is_reference(expected) =>
            {
                Some(Expr::Cast {
                    ty: JavaType::render_field_type(expected),
                    expr: Box::new(value.expr),
                })
            }
            (StackType::Field(FieldType::Base(actual)), FieldType::Base(expected))
                if Self::widens(*actual, *expected) =>
            {
                Some(Expr::Cast {
                    ty: expected.keyword().into(),
                    expr: Box::new(value.expr),
                })
            }
            (StackType::Field(FieldType::Base(BaseType::Int)), FieldType::Base(expected)) => {
                let constant = value.int_constant()?;
                match expected {
                    BaseType::Boolean => match constant {
                        0 => Some(Expr::lit("false")),
                        1 => Some(Expr::lit("true")),
                        _ => None,
                    },
                    BaseType::Char => Some(Expr::lit(Literal::char_code_unit(constant)?)),
                    BaseType::Byte if i8::try_from(constant).is_ok() => Some(Expr::Cast {
                        ty: "byte".into(),
                        expr: Box::new(value.expr),
                    }),
                    BaseType::Short if i16::try_from(constant).is_ok() => Some(Expr::Cast {
                        ty: "short".into(),
                        expr: Box::new(value.expr),
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn reference_expr(value: StackValue) -> Option<Expr> {
        match value.ty {
            StackType::Field(ref ty) if Self::is_reference(ty) => Some(value.expr),
            _ => None,
        }
    }

    fn int_numeric_expr(value: StackValue) -> Option<Expr> {
        match value.ty {
            StackType::Field(FieldType::Base(base)) if Self::is_int_numeric(base) => {
                Some(value.expr)
            }
            _ => None,
        }
    }

    fn exact_base_expr(value: StackValue, expected: BaseType) -> Option<Expr> {
        matches!(value.ty, StackType::Field(FieldType::Base(actual)) if actual == expected)
            .then_some(value.expr)
    }

    /// Push the value of a local slot (`this` for slot 0 of an instance method).
    fn load(&mut self, slot: u16, kind: JvmKind) -> Option<()> {
        if !self.is_static && slot == 0 {
            if !matches!(kind, JvmKind::Reference) {
                return None;
            }
            self.stack.push(StackValue::object(Expr::This, self.owner));
        } else {
            let local = self.locals.get(&slot)?;
            if !kind.accepts(&local.ty) {
                return None;
            }
            self.stack.push(StackValue::field(
                Expr::Local(local.name.clone()),
                local.ty.clone(),
            ));
        }
        Some(())
    }

    /// Store the top of stack into a local: `name = value;`. The slot's name comes from the map
    /// built by [`local_declarations`] (parameters plus hoisted locals), so an unmapped slot bails.
    fn store(&mut self, slot: u16, kind: JvmKind) -> Option<()> {
        let local = self.locals.get(&slot)?.clone();
        if !kind.accepts(&local.ty) {
            return None;
        }
        let value = Self::consume_as(self.pop()?, &local.ty)?;
        self.stmts.push(Stmt::Assign {
            target: Expr::Local(local.name),
            value,
        });
        Some(())
    }

    /// `iinc`: `name = name + by;` (or `name - |by|;` when negative). Reads and writes the local in
    /// place — the operand stack is untouched.
    fn iinc(&mut self, slot: u16, by: i32) -> Option<()> {
        let local = self.locals.get(&slot)?;
        if local.ty != FieldType::Base(BaseType::Int) {
            return None;
        }
        let name = local.name.clone();
        let op = if by < 0 { "-" } else { "+" };
        let mag = by.unsigned_abs();
        self.stmts.push(Stmt::Assign {
            target: Expr::Local(name.clone()),
            value: Expr::Binary {
                op,
                lhs: Box::new(Expr::Local(name)),
                rhs: Box::new(Expr::lit(mag.to_string())),
            },
        });
        Some(())
    }

    fn int_binary(&mut self, op: &'static str) -> Option<()> {
        let rhs = Self::int_numeric_expr(self.pop()?)?;
        let lhs = Self::int_numeric_expr(self.pop()?)?;
        self.stack.push(StackValue::base(
            Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            BaseType::Int,
        ));
        Some(())
    }

    fn base_binary(&mut self, op: &'static str, base: BaseType) -> Option<()> {
        let rhs = Self::exact_base_expr(self.pop()?, base)?;
        let lhs = Self::exact_base_expr(self.pop()?, base)?;
        self.stack.push(StackValue::base(
            Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            base,
        ));
        Some(())
    }

    fn int_bitwise(&mut self, op: &'static str) -> Option<()> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        let boolean = matches!(
            (&lhs.ty, &rhs.ty),
            (
                StackType::Field(FieldType::Base(BaseType::Boolean)),
                StackType::Field(FieldType::Base(BaseType::Boolean))
            )
        );
        let (lhs, rhs, result) = if boolean {
            (lhs.expr, rhs.expr, BaseType::Boolean)
        } else {
            (
                Self::int_numeric_expr(lhs)?,
                Self::int_numeric_expr(rhs)?,
                BaseType::Int,
            )
        };
        self.stack.push(StackValue::base(
            Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            result,
        ));
        Some(())
    }

    fn shift(&mut self, op: &'static str, lhs_type: BaseType) -> Option<()> {
        let rhs = Self::int_numeric_expr(self.pop()?)?;
        let lhs = if lhs_type == BaseType::Int {
            Self::int_numeric_expr(self.pop()?)?
        } else {
            Self::exact_base_expr(self.pop()?, lhs_type)?
        };
        self.stack.push(StackValue::base(
            Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            lhs_type,
        ));
        Some(())
    }

    fn int_unary(&mut self, op: &'static str) -> Option<()> {
        let expr = Self::int_numeric_expr(self.pop()?)?;
        self.stack.push(StackValue::base(
            Expr::Unary {
                op,
                expr: Box::new(expr),
            },
            BaseType::Int,
        ));
        Some(())
    }

    fn base_unary(&mut self, op: &'static str, base: BaseType) -> Option<()> {
        let expr = Self::exact_base_expr(self.pop()?, base)?;
        self.stack.push(StackValue::base(
            Expr::Unary {
                op,
                expr: Box::new(expr),
            },
            base,
        ));
        Some(())
    }

    fn numeric_cast(&mut self, target: BaseType) -> Option<()> {
        let value = self.pop()?;
        let StackType::Field(FieldType::Base(source)) = value.ty else {
            return None;
        };
        if source == BaseType::Boolean || target == BaseType::Boolean {
            return None;
        }
        self.stack.push(StackValue::base(
            Expr::Cast {
                ty: target.keyword().into(),
                expr: Box::new(value.expr),
            },
            target,
        ));
        Some(())
    }

    fn check_cast(&mut self, target: FieldType) -> Option<()> {
        let value = self.pop()?;
        if !value.ty.is_reference_compatible() {
            return None;
        }
        self.stack.push(StackValue::field(
            Expr::Cast {
                ty: JavaType::render_field_type(&target),
                expr: Box::new(value.expr),
            },
            target,
        ));
        Some(())
    }

    /// Emit an invocation whose receiver is already known (`recv` = `None` for a static call, which
    /// carries its owner type as its receiver expression instead).
    fn invoke(&mut self, index: u16, is_static: bool) -> Option<()> {
        let (_, owner, name, descriptor) = self.method_ref(index)?;
        let md = MethodDescriptor::parse(&descriptor).ok()?;
        if !is_static && owner == "java/lang/StringBuilder" && self.fold_builder(&name, &md)? {
            return Some(());
        }
        let args = self.pop_call_args(&md.params)?;
        let recv = if is_static {
            Box::new(Expr::Type(JavaType::internal_to_java(&owner)))
        } else {
            Box::new(Self::consume_as(
                self.pop()?,
                &Self::internal_type(&owner)?,
            )?)
        };
        self.emit_call(
            Expr::Call {
                recv: Some(recv),
                name,
                args,
            },
            md.return_type,
        );
        Some(())
    }

    /// Handle `invokespecial`: a constructor chain (`super(...)` / `this(...)`), object creation
    /// (`new X(...)`), or a non-virtual instance call (a `private` / `super.m()` method).
    fn invoke_special(&mut self, index: u16) -> Option<()> {
        let (kind, owner, name, descriptor) = self.method_ref(index)?;
        let md = MethodDescriptor::parse(&descriptor).ok()?;
        let args = self.pop_call_args(&md.params)?;
        if name != "<init>" {
            let receiver = Self::reference_expr(self.pop()?)?;
            let receiver = if owner == self.owner {
                receiver
            } else {
                match (kind, receiver) {
                    (MethodRefKind::Class, Expr::This)
                        if !self.owner_is_interface
                            && self.direct_superclass == Some(owner.as_str()) =>
                    {
                        Expr::Super
                    }
                    (MethodRefKind::Interface, Expr::This)
                        if !self.is_static
                            && self.hierarchy.allows_interface_super(
                                self.class,
                                &owner,
                                &name,
                                &descriptor,
                            ) =>
                    {
                        Expr::QualifiedSuper(JavaType::internal_to_java(&owner))
                    }
                    _ => return None,
                }
            };
            self.emit_call(
                Expr::Call {
                    recv: Some(Box::new(receiver)),
                    name,
                    args,
                },
                md.return_type,
            );
            return Some(());
        }
        if !matches!(md.return_type, ReturnType::Void) {
            return None;
        }
        let receiver = self.pop()?;
        match receiver.expr {
            // `this.<init>` — a `super(...)` / `this(...)` constructor delegation.
            Expr::This => {
                if owner == self.owner {
                    self.stmts.push(Stmt::ThisCall(args));
                } else if !args.is_empty() {
                    self.stmts.push(Stmt::SuperCall(args));
                }
                // else: an implicit no-arg `super()` — Java inserts it, so omit it.
            }
            // `new X(...)` — the `dup`'d copy left on the stack becomes the constructed value.
            Expr::Uninitialized(ty) => {
                if !matches!(
                    receiver.ty,
                    StackType::Field(FieldType::Object(ref internal)) if *internal == owner
                ) {
                    return None;
                }
                if matches!(
                    self.stack.last(),
                    Some(StackValue {
                        expr: Expr::Uninitialized(top),
                        ..
                    }) if *top == ty
                ) {
                    self.stack.pop();
                }
                // A fresh no-arg `StringBuilder` is recognized here, where the internal name is
                // authoritative, and pushed as the collecting sentinel: its concat-safe `append`
                // chain may fold into a `+` concatenation, and any other consumption finalizes it
                // back into the original calls ([`Sim::finalize`]), so an unfolded one renders
                // exactly as the `new` it was.
                if owner == "java/lang/StringBuilder" && args.is_empty() {
                    self.stack
                        .push(StackValue::object(Expr::PendingBuilder(Vec::new()), owner));
                } else {
                    self.stack
                        .push(StackValue::object(Expr::New { ty, args }, owner));
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// Push a call (value result) or emit it as a statement (`void` result).
    fn emit_call(&mut self, call: Expr, return_type: ReturnType) {
        match return_type {
            ReturnType::Void => self.stmts.push(Stmt::Expr(call)),
            ReturnType::Type(ty) => self.stack.push(StackValue::field(call, ty)),
        }
    }

    /// Try to fold a `StringBuilder` call into a collecting concatenation: an `append` of a
    /// concat-safe operand onto a collecting chain (the [`Expr::PendingBuilder`] a fresh
    /// `new StringBuilder()` pushes) extends it, and a `toString()` on a non-empty chain
    /// finalizes it into the `+` concatenation. Returns `Some(false)` when the call is not part
    /// of that pattern, so the caller renders it as an ordinary call — a chain consumed any
    /// other way re-renders as the original `new StringBuilder().append(…)` calls via
    /// [`Sim::finalize`].
    fn fold_builder(&mut self, name: &str, md: &MethodDescriptor) -> Option<bool> {
        match name {
            "toString" if md.params.is_empty() => match self.stack.last_mut() {
                // An empty chain stays an ordinary `new StringBuilder().toString()` call.
                Some(StackValue {
                    expr: Expr::PendingBuilder(parts),
                    ..
                }) if !parts.is_empty() => {
                    let parts = core::mem::take(parts);
                    self.stack.pop();
                    self.stack
                        .push(StackValue::object(Expr::concat(parts), "java/lang/String"));
                    Some(true)
                }
                _ => Some(false),
            },
            "append" if md.params.len() == 1 && Self::concat_safe(&md.params[0]) => {
                // The stack is `[…, receiver, operand]` — commit only when the receiver is a
                // collecting chain (a builder that came from a local, parameter, or field keeps
                // its real `append` calls).
                let receiver = self.stack.len().checked_sub(2).map(|i| &self.stack[i]);
                if !matches!(
                    receiver,
                    Some(StackValue {
                        expr: Expr::PendingBuilder(_),
                        ..
                    })
                ) {
                    return Some(false);
                }
                let arg = self.pop()?;
                let part = ConcatPart {
                    expr: Self::consume_as(arg, &md.params[0])?,
                    stringy: Self::is_string(&md.params[0]),
                };
                let Some(StackValue {
                    expr: Expr::PendingBuilder(parts),
                    ..
                }) = self.stack.last_mut()
                else {
                    return None;
                };
                parts.push(part);
                Some(true)
            }
            _ => Some(false),
        }
    }

    /// Whether a `StringBuilder.append` overload of this operand type appends exactly the
    /// operand's *string conversion* — the condition under which the append equals one `+`
    /// operand. Every primitive and `String`/`Object`/`CharSequence` qualify; `char[]` does not
    /// (it appends the array's *characters*, where `+` would render its `toString`), so it and
    /// anything else stay unfolded.
    fn concat_safe(param: &FieldType) -> bool {
        match param {
            FieldType::Base(_) => true,
            FieldType::Object(internal) => matches!(
                internal.as_str(),
                "java/lang/String" | "java/lang/Object" | "java/lang/CharSequence"
            ),
            FieldType::Array(_) => false,
        }
    }

    /// Whether a descriptor type is exactly `java.lang.String` (the operand type that anchors a
    /// rendered `+` chain in string context).
    fn is_string(ft: &FieldType) -> bool {
        matches!(ft, FieldType::Object(internal) if internal == "java/lang/String")
    }

    /// `invokedynamic`: only the two `java.lang.invoke.StringConcatFactory` bootstraps `javac`
    /// compiles string concatenation to are modelled — `makeConcatWithConstants`, whose recipe
    /// interleaves literal chunks with the stacked operands (`\u{1}`) and trailing constants
    /// (`\u{2}`), and the recipe-free `makeConcat`. The call site folds back into the `+`
    /// concatenation it came from; any other bootstrap (a lambda, a method reference, …) bails.
    fn invoke_dynamic(&mut self, index: u16) -> Option<()> {
        let ConstantPoolEntry::InvokeDynamic {
            bootstrap_method_attr_index,
            name_and_type_index,
        } = self.pool.get(index)?
        else {
            return None;
        };
        let (_, descriptor) = self.name_and_type(*name_and_type_index)?;
        let md = MethodDescriptor::parse(&descriptor).ok()?;
        // A string concatenation always produces a `String`; anything else is a foreign bootstrap.
        if !matches!(&md.return_type, ReturnType::Type(ft) if Self::is_string(ft)) {
            return None;
        }
        let bsm = self
            .bootstrap
            .get(usize::from(*bootstrap_method_attr_index))?;
        let (recipe, consts) = self.concat_shape(bsm, md.params.len())?;
        let mut args = self.pop_values(md.params.len())?.into_iter();
        let mut params = md.params.iter();
        let mut consts = consts.into_iter();
        let mut parts: Vec<ConcatPart> = Vec::new();
        let mut chunk = String::new();
        let flush = |chunk: &mut String, parts: &mut Vec<ConcatPart>| {
            if !chunk.is_empty() {
                parts.push(ConcatPart {
                    expr: Expr::lit(Literal::string_literal(chunk)),
                    stringy: true,
                });
                chunk.clear();
            }
        };
        for c in recipe.chars() {
            match c {
                '\u{1}' => {
                    flush(&mut chunk, &mut parts);
                    let (arg, param) = (args.next()?, params.next()?);
                    parts.push(ConcatPart {
                        expr: Self::consume_as(arg, param)?,
                        stringy: Self::is_string(param),
                    });
                }
                '\u{2}' => {
                    flush(&mut chunk, &mut parts);
                    parts.push(consts.next()?);
                }
                c => chunk.push(c),
            }
        }
        flush(&mut chunk, &mut parts);
        // Every stacked operand and constant must be placed by the recipe, or a value would be
        // silently dropped from the rendered concatenation.
        if args.next().is_some() || consts.next().is_some() {
            return None;
        }
        self.stack
            .push(StackValue::object(Expr::concat(parts), "java/lang/String"));
        Some(())
    }

    /// Resolve an `invokedynamic` bootstrap to the string-concat shape it encodes: the recipe
    /// (each `\u{1}` a stacked operand, each `\u{2}` a constant, anything else a literal chunk)
    /// and the rendered `\u{2}` constants, or `None` when the bootstrap is not one of
    /// `StringConcatFactory`'s two factories.
    fn concat_shape(
        &self,
        bsm: &BootstrapMethod,
        argc: usize,
    ) -> Option<(String, Vec<ConcatPart>)> {
        let ConstantPoolEntry::MethodHandle {
            reference_kind,
            reference_index,
        } = self.pool.get(bsm.bootstrap_method_ref)?
        else {
            return None;
        };
        // Both concat factories are `REF_invokeStatic` bootstraps (JVMS Table 5.4.3.5-A).
        if *reference_kind != 6 {
            return None;
        }
        let (_, owner, name, _) = self.method_ref(*reference_index)?;
        if owner != "java/lang/invoke/StringConcatFactory" {
            return None;
        }
        match name.as_str() {
            // No recipe: the concatenation is exactly the stacked operands, in order.
            "makeConcat" if bsm.bootstrap_arguments.is_empty() => {
                Some(("\u{1}".repeat(argc), Vec::new()))
            }
            "makeConcatWithConstants" => {
                let (recipe_index, rest) = bsm.bootstrap_arguments.split_first()?;
                let ConstantPoolEntry::String { string_index } = self.pool.get(*recipe_index)?
                else {
                    return None;
                };
                let recipe = self.pool.utf8(*string_index)?.into_owned();
                // The trailing constants a `\u{2}` marker pulls in: `javac` only ever passes
                // strings (a constant whose text contains a marker char); anything else bails.
                let consts = rest
                    .iter()
                    .map(|&i| match self.pool.get(i)? {
                        ConstantPoolEntry::String { string_index } => Some(ConcatPart {
                            expr: Expr::lit(Literal::string_literal(
                                &self.pool.utf8(*string_index)?,
                            )),
                            stringy: true,
                        }),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some((recipe, consts))
            }
            _ => None,
        }
    }

    /// The receiver of a field access / store: the owner type for a `static` field, else the object
    /// reference popped from the stack.
    fn field_receiver(&mut self, owner: &str, is_static: bool) -> Option<Expr> {
        if is_static {
            Some(Expr::Type(JavaType::internal_to_java(owner)))
        } else {
            Self::consume_as(self.pop()?, &Self::internal_type(owner)?)
        }
    }

    fn field_access(&mut self, index: u16, is_static: bool) -> Option<()> {
        let (owner, name, descriptor) = self.field_ref(index)?;
        let ty = FieldType::parse(&descriptor).ok()?;
        let recv = self.field_receiver(&owner, is_static)?;
        self.stack.push(StackValue::field(
            Expr::Field {
                recv: Box::new(recv),
                name,
            },
            ty,
        ));
        Some(())
    }

    fn field_store(&mut self, index: u16, is_static: bool) -> Option<()> {
        let (owner, name, descriptor) = self.field_ref(index)?;
        let ty = FieldType::parse(&descriptor).ok()?;
        let value = Self::consume_as(self.pop()?, &ty)?;
        let recv = self.field_receiver(&owner, is_static)?;
        self.stmts.push(Stmt::Assign {
            target: Expr::Field {
                recv: Box::new(recv),
                name,
            },
            value,
        });
        Some(())
    }

    /// Push a one-sized-dimension array creation (`newarray` / `anewarray`), popping its length.
    /// A constant length starts a collecting [`Expr::PendingArray`] (a following `dup; <index>;
    /// <value>; Xastore` run folds into a `new T[]{…}` initializer; consumption finalizes it) — a
    /// dynamic length can never take an initializer in source, so it is final immediately.
    fn new_array(&mut self, element_type: FieldType) -> Option<()> {
        let len = self.pop()?;
        let constant_len = len
            .int_constant()
            .and_then(|value| usize::try_from(value).ok());
        let len_expr = Self::int_numeric_expr(len)?;
        let (elem, empty_dims) = JavaType::array_base(&element_type);
        let array_type = FieldType::Array(Box::new(element_type));
        let expr = match constant_len {
            Some(len) => Expr::PendingArray {
                elem,
                empty_dims,
                len,
                elems: Vec::new(),
            },
            None => Expr::NewArray {
                elem,
                empty_dims,
                form: ArrayForm::Sized(vec![len_expr]),
            },
        };
        self.stack.push(StackValue::field(expr, array_type));
        Some(())
    }

    /// `anewarray`: the pool entry names the *element* class — itself an array type (`[I`) for a
    /// `new int[n][]`-shaped creation.
    fn anew_array(&mut self, index: u16) -> Option<()> {
        self.new_array(self.class_ref_type(index)?)
    }

    /// `multianewarray`: the pool entry is the full array descriptor (`[[I`); `dimensions` counts
    /// are popped as the sized dimensions, any remaining depth rendering as empty `[]` pairs
    /// (`new int[a][b]`, `new int[a][b][]`). Never collecting — no compiler runs the initializer
    /// store pattern on one (a following `dup` bails).
    fn multi_new_array(&mut self, index: u16, dimensions: u8) -> Option<()> {
        let array_type = self.class_ref_type(index)?;
        let (elem, depth) = JavaType::array_base(&array_type);
        let dimensions = usize::from(dimensions);
        if dimensions == 0 || dimensions > depth {
            return None;
        }
        let dims = self
            .pop_values(dimensions)?
            .into_iter()
            .map(Self::int_numeric_expr)
            .collect::<Option<Vec<_>>>()?;
        self.stack.push(StackValue::field(
            Expr::NewArray {
                elem,
                empty_dims: depth - dimensions,
                form: ArrayForm::Sized(dims),
            },
            array_type,
        ));
        Some(())
    }

    /// An array element read (`*aload`): the descriptor component disambiguates `baload` and keeps
    /// the source type for later assignments, calls, returns, and branches.
    fn array_load(&mut self, kind: ArrayKind) -> Option<()> {
        let index = Self::int_numeric_expr(self.pop()?)?;
        let array = self.pop()?;
        let StackType::Field(FieldType::Array(component)) = &array.ty else {
            return None;
        };
        if !kind.accepts(component) {
            return None;
        }
        self.stack.push(StackValue::field(
            Expr::Index {
                array: Box::new(array.expr),
                index: Box::new(index),
            },
            (**component).clone(),
        ));
        Some(())
    }

    /// An array element write (`*astore`, all eight flavors): fold into a collecting array
    /// initializer when the array operand is the `dup`'d creation marker, else a plain
    /// `array[index] = value;` (mirroring [`Sim::field_store`]).
    fn array_store(&mut self, kind: ArrayKind) -> Option<()> {
        let value = self.pop()?;
        let index = self.pop()?;
        // The array operand is popped raw: the initializer-store marker must reach `push_elem`,
        // not the finalizing `pop` (which rejects it).
        let array = self.stack.pop()?;
        if matches!(array.expr, Expr::PendingArrayDup) {
            self.push_elem(&index, value, kind, &array.ty)
        } else {
            let array = Self::finalize(array)?;
            let StackType::Field(FieldType::Array(component)) = &array.ty else {
                return None;
            };
            if !kind.accepts(component) {
                return None;
            }
            let value = Self::consume_as(value, component)?;
            let index = Self::int_numeric_expr(index)?;
            self.stmts.push(Stmt::Assign {
                target: Expr::Index {
                    array: Box::new(array.expr),
                    index: Box::new(index),
                },
                value,
            });
            Some(())
        }
    }

    /// Fold one `dup; <index>; <value>; Xastore` element store into the collecting
    /// [`Expr::PendingArray`] beneath the popped marker. Only the exact `javac` initializer shape
    /// folds — the index must be the next sequential constant from 0 and in bounds — so a partial
    /// or out-of-order fill (a default-skipping compiler) can never render a wrong-length
    /// `new T[]{…}`; anything else bails.
    fn push_elem(
        &mut self,
        index: &StackValue,
        value: StackValue,
        kind: ArrayKind,
        marker_type: &StackType,
    ) -> Option<()> {
        let position = usize::try_from(index.int_constant()?).ok()?;
        let pending = self.stack.last_mut()?;
        if pending.ty != *marker_type {
            return None;
        }
        let StackType::Field(FieldType::Array(component)) = &pending.ty else {
            return None;
        };
        if !kind.accepts(component) {
            return None;
        }
        let value = Self::consume_as(value, component)?;
        let Expr::PendingArray { len, elems, .. } = &mut pending.expr else {
            return None;
        };
        if position != elems.len() || position >= *len {
            return None;
        }
        elems.push(value);
        Some(())
    }

    fn return_value(&mut self, kind: JvmKind) -> Option<()> {
        let ReturnType::Type(expected) = self.return_type else {
            return None;
        };
        if !kind.accepts(expected) {
            return None;
        }
        let value = Self::consume_as(self.pop()?, expected)?;
        self.stmts.push(Stmt::Return(Some(value)));
        Some(())
    }

    fn step(&mut self, ins: &Instruction) -> Option<()> {
        use Instruction as I;
        // Local stores (all forms) are decoded once for both declaration discovery and simulation.
        // Handle them before the `match` because a guard cannot bind the slot and kind there.
        if let Some((slot, kind)) = MethodBody::store_info(ins) {
            return self.store(slot, kind);
        }
        match ins {
            I::Nop => {}

            // Constants.
            I::AconstNull => self.stack.push(StackValue {
                expr: Expr::lit("null"),
                ty: StackType::Null,
            }),
            I::IconstM1 => self.stack.push(StackValue::int_literal(-1)),
            I::Iconst0 => self.stack.push(StackValue::int_literal(0)),
            I::Iconst1 => self.stack.push(StackValue::int_literal(1)),
            I::Iconst2 => self.stack.push(StackValue::int_literal(2)),
            I::Iconst3 => self.stack.push(StackValue::int_literal(3)),
            I::Iconst4 => self.stack.push(StackValue::int_literal(4)),
            I::Iconst5 => self.stack.push(StackValue::int_literal(5)),
            I::Lconst0 => self
                .stack
                .push(StackValue::base(Expr::lit("0L"), BaseType::Long)),
            I::Lconst1 => self
                .stack
                .push(StackValue::base(Expr::lit("1L"), BaseType::Long)),
            I::Fconst0 => self.stack.push(StackValue::base(
                Expr::lit(Literal::float_literal(0.0)),
                BaseType::Float,
            )),
            I::Fconst1 => self.stack.push(StackValue::base(
                Expr::lit(Literal::float_literal(1.0)),
                BaseType::Float,
            )),
            I::Fconst2 => self.stack.push(StackValue::base(
                Expr::lit(Literal::float_literal(2.0)),
                BaseType::Float,
            )),
            I::Dconst0 => self.stack.push(StackValue::base(
                Expr::lit(Literal::double_literal(0.0)),
                BaseType::Double,
            )),
            I::Dconst1 => self.stack.push(StackValue::base(
                Expr::lit(Literal::double_literal(1.0)),
                BaseType::Double,
            )),
            I::Bipush(v) => self.stack.push(StackValue::int_literal(i32::from(*v))),
            I::Sipush(v) => self.stack.push(StackValue::int_literal(i32::from(*v))),
            I::Ldc(i) => {
                let e = self.constant(u16::from(*i))?;
                self.stack.push(e);
            }
            I::LdcW(i) | I::Ldc2W(i) => {
                let e = self.constant(*i)?;
                self.stack.push(e);
            }

            // Loads (slot forms and the numbered shorthands).
            I::Iload(s) => self.load(u16::from(*s), JvmKind::Int)?,
            I::Lload(s) => self.load(u16::from(*s), JvmKind::Long)?,
            I::Fload(s) => self.load(u16::from(*s), JvmKind::Float)?,
            I::Dload(s) => self.load(u16::from(*s), JvmKind::Double)?,
            I::Aload(s) => self.load(u16::from(*s), JvmKind::Reference)?,
            I::Iload0 => self.load(0, JvmKind::Int)?,
            I::Lload0 => self.load(0, JvmKind::Long)?,
            I::Fload0 => self.load(0, JvmKind::Float)?,
            I::Dload0 => self.load(0, JvmKind::Double)?,
            I::Aload0 => self.load(0, JvmKind::Reference)?,
            I::Iload1 => self.load(1, JvmKind::Int)?,
            I::Lload1 => self.load(1, JvmKind::Long)?,
            I::Fload1 => self.load(1, JvmKind::Float)?,
            I::Dload1 => self.load(1, JvmKind::Double)?,
            I::Aload1 => self.load(1, JvmKind::Reference)?,
            I::Iload2 => self.load(2, JvmKind::Int)?,
            I::Lload2 => self.load(2, JvmKind::Long)?,
            I::Fload2 => self.load(2, JvmKind::Float)?,
            I::Dload2 => self.load(2, JvmKind::Double)?,
            I::Aload2 => self.load(2, JvmKind::Reference)?,
            I::Iload3 => self.load(3, JvmKind::Int)?,
            I::Lload3 => self.load(3, JvmKind::Long)?,
            I::Fload3 => self.load(3, JvmKind::Float)?,
            I::Dload3 => self.load(3, JvmKind::Double)?,
            I::Aload3 => self.load(3, JvmKind::Reference)?,

            // `iinc` (and its wide form): a read-modify-write of a local, stack untouched.
            I::Iinc { index, value } => self.iinc(u16::from(*index), i32::from(*value))?,
            I::Wide(WideInstruction::Iinc { index, value }) => {
                self.iinc(*index, i32::from(*value))?;
            }

            // Arithmetic and bitwise.
            I::Iadd => self.int_binary("+")?,
            I::Ladd => self.base_binary("+", BaseType::Long)?,
            I::Fadd => self.base_binary("+", BaseType::Float)?,
            I::Dadd => self.base_binary("+", BaseType::Double)?,
            I::Isub => self.int_binary("-")?,
            I::Lsub => self.base_binary("-", BaseType::Long)?,
            I::Fsub => self.base_binary("-", BaseType::Float)?,
            I::Dsub => self.base_binary("-", BaseType::Double)?,
            I::Imul => self.int_binary("*")?,
            I::Lmul => self.base_binary("*", BaseType::Long)?,
            I::Fmul => self.base_binary("*", BaseType::Float)?,
            I::Dmul => self.base_binary("*", BaseType::Double)?,
            I::Idiv => self.int_binary("/")?,
            I::Ldiv => self.base_binary("/", BaseType::Long)?,
            I::Fdiv => self.base_binary("/", BaseType::Float)?,
            I::Ddiv => self.base_binary("/", BaseType::Double)?,
            I::Irem => self.int_binary("%")?,
            I::Lrem => self.base_binary("%", BaseType::Long)?,
            I::Frem => self.base_binary("%", BaseType::Float)?,
            I::Drem => self.base_binary("%", BaseType::Double)?,
            I::Ineg => self.int_unary("-")?,
            I::Lneg => self.base_unary("-", BaseType::Long)?,
            I::Fneg => self.base_unary("-", BaseType::Float)?,
            I::Dneg => self.base_unary("-", BaseType::Double)?,
            I::Ishl => self.shift("<<", BaseType::Int)?,
            I::Lshl => self.shift("<<", BaseType::Long)?,
            I::Ishr => self.shift(">>", BaseType::Int)?,
            I::Lshr => self.shift(">>", BaseType::Long)?,
            I::Iushr => self.shift(">>>", BaseType::Int)?,
            I::Lushr => self.shift(">>>", BaseType::Long)?,
            I::Iand => self.int_bitwise("&")?,
            I::Land => self.base_binary("&", BaseType::Long)?,
            I::Ior => self.int_bitwise("|")?,
            I::Lor => self.base_binary("|", BaseType::Long)?,
            I::Ixor => self.int_bitwise("^")?,
            I::Lxor => self.base_binary("^", BaseType::Long)?,

            // Numeric conversions.
            I::I2l | I::F2l | I::D2l => self.numeric_cast(BaseType::Long)?,
            I::I2f | I::L2f | I::D2f => self.numeric_cast(BaseType::Float)?,
            I::I2d | I::L2d | I::F2d => self.numeric_cast(BaseType::Double)?,
            I::L2i | I::F2i | I::D2i => self.numeric_cast(BaseType::Int)?,
            I::I2b => self.numeric_cast(BaseType::Byte)?,
            I::I2c => self.numeric_cast(BaseType::Char)?,
            I::I2s => self.numeric_cast(BaseType::Short)?,

            // Field access.
            I::GetField(i) => self.field_access(*i, false)?,
            I::GetStatic(i) => self.field_access(*i, true)?,
            I::PutField(i) => self.field_store(*i, false)?,
            I::PutStatic(i) => self.field_store(*i, true)?,

            // Invocations.
            I::InvokeVirtual(i) | I::InvokeInterface { index: i, .. } => self.invoke(*i, false)?,
            I::InvokeStatic(i) => self.invoke(*i, true)?,
            I::InvokeSpecial(i) => self.invoke_special(*i)?,
            I::InvokeDynamic { index } => self.invoke_dynamic(*index)?,

            // Object creation.
            I::New(i) => {
                let internal = self.class_ref(*i)?;
                let ty = JavaType::internal_to_java(&internal);
                self.stack
                    .push(StackValue::object(Expr::Uninitialized(ty), internal));
            }
            I::Dup => {
                // Only two shapes are modelled — the object-creation `new; dup; …; invokespecial`
                // and the array-initializer `newarray/anewarray; (dup; <index>; <value>;
                // Xastore)*` — since a `dup` of any real value would duplicate a side effect;
                // everything else bails.
                let duplicate = match self.stack.last()? {
                    value @ StackValue {
                        expr: Expr::Uninitialized(_),
                        ..
                    } => value.clone(),
                    StackValue {
                        expr: Expr::PendingArray { .. },
                        ty,
                    } => StackValue {
                        expr: Expr::PendingArrayDup,
                        ty: ty.clone(),
                    },
                    _ => return None,
                };
                self.stack.push(duplicate);
            }
            I::CheckCast(i) => {
                self.check_cast(self.class_ref_type(*i)?)?;
            }
            I::ArrayLength => {
                let array = self.pop()?;
                if !matches!(array.ty, StackType::Field(FieldType::Array(_))) {
                    return None;
                }
                self.stack.push(StackValue::base(
                    Expr::ArrayLength(Box::new(array.expr)),
                    BaseType::Int,
                ));
            }

            // Arrays: element reads / writes and creation.
            I::Iaload => self.array_load(ArrayKind::Int)?,
            I::Laload => self.array_load(ArrayKind::Long)?,
            I::Faload => self.array_load(ArrayKind::Float)?,
            I::Daload => self.array_load(ArrayKind::Double)?,
            I::Aaload => self.array_load(ArrayKind::Reference)?,
            I::Baload => self.array_load(ArrayKind::ByteOrBoolean)?,
            I::Caload => self.array_load(ArrayKind::Char)?,
            I::Saload => self.array_load(ArrayKind::Short)?,
            I::Iastore => self.array_store(ArrayKind::Int)?,
            I::Lastore => self.array_store(ArrayKind::Long)?,
            I::Fastore => self.array_store(ArrayKind::Float)?,
            I::Dastore => self.array_store(ArrayKind::Double)?,
            I::Aastore => self.array_store(ArrayKind::Reference)?,
            I::Bastore => self.array_store(ArrayKind::ByteOrBoolean)?,
            I::Castore => self.array_store(ArrayKind::Char)?,
            I::Sastore => self.array_store(ArrayKind::Short)?,
            I::NewArray(atype) => {
                let elem = BaseType::from_atype(*atype)?;
                self.new_array(FieldType::Base(elem))?;
            }
            I::ANewArray(i) => self.anew_array(*i)?,
            I::MultiANewArray { index, dimensions } => {
                self.multi_new_array(*index, *dimensions)?;
            }

            // Returns and throw.
            I::Ireturn => self.return_value(JvmKind::Int)?,
            I::Lreturn => self.return_value(JvmKind::Long)?,
            I::Freturn => self.return_value(JvmKind::Float)?,
            I::Dreturn => self.return_value(JvmKind::Double)?,
            I::Areturn => self.return_value(JvmKind::Reference)?,
            I::Return => {
                if !matches!(self.return_type, ReturnType::Void) {
                    return None;
                }
                self.stmts.push(Stmt::Return(None));
            }
            I::Athrow => {
                let value = self.pop()?;
                if !value.ty.is_reference_compatible() {
                    return None;
                }
                self.stmts.push(Stmt::Throw(value.expr));
            }

            // Discard: a call (or a discarded object creation / builder chain) whose result is
            // unused becomes an expression statement.
            I::Pop => match self.stack.last().map(|value| &value.expr) {
                Some(Expr::Call { .. } | Expr::New { .. } | Expr::PendingBuilder(_)) => {
                    let call = self.pop()?;
                    self.stmts.push(Stmt::Expr(call.expr));
                }
                _ => {
                    self.pop()?;
                }
            },

            // Everything else — branches, switches, `jsr`/`ret`, monitors, `wide` loads, a
            // `*cmp` not fused into its block's conditional branch (the fused form is read by
            // `Structurer::branch_condition`, never stepped), exotic stack shuffles
            // (`dup2`/`dup_x*`/`swap`, so compound element assignment like `arr[i]++`) — is not
            // yet modelled. Bail so the caller keeps its safe body. (A non-string-concat
            // `invokedynamic` bails inside `invoke_dynamic`.)
            _ => return None,
        }
        Some(())
    }

    /// Resolve a constant-pool constant (for `ldc` / `ldc_w` / `ldc2_w`) to a typed literal.
    fn constant(&self, index: u16) -> Option<StackValue> {
        Some(match self.pool.get(index)? {
            ConstantPoolEntry::Integer(v) => StackValue::int_literal(*v),
            ConstantPoolEntry::Long(v) => {
                StackValue::base(Expr::lit(format!("{v}L")), BaseType::Long)
            }
            ConstantPoolEntry::Float(v) => {
                StackValue::base(Expr::lit(Literal::float_literal(*v)), BaseType::Float)
            }
            ConstantPoolEntry::Double(v) => {
                StackValue::base(Expr::lit(Literal::double_literal(*v)), BaseType::Double)
            }
            ConstantPoolEntry::String { string_index } => StackValue::object(
                Expr::lit(Literal::string_literal(&self.pool.utf8(*string_index)?)),
                "java/lang/String",
            ),
            ConstantPoolEntry::Class { .. } => {
                let class_type = self.class_ref_type(index)?;
                StackValue::object(
                    Expr::lit(Literal::class_literal(&class_type)),
                    "java/lang/Class",
                )
            }
            _ => return None,
        })
    }

    /// The `(owner-internal, name, descriptor)` a `FieldRef` points to. A field name that is not a
    /// valid Java identifier (e.g. a JVM-legal but Java-reserved keyword) bails so the body falls
    /// back to a safe skeleton instead of emitting an unparsable `recv.class` access.
    fn field_ref(&self, index: u16) -> Option<(String, String, String)> {
        match self.pool.get(index)? {
            ConstantPoolEntry::FieldRef {
                class_index,
                name_and_type_index,
            } => {
                let owner = self.pool.class_name(*class_index)?.into_owned();
                let (name, descriptor) = self.name_and_type(*name_and_type_index)?;
                if !Attrs::is_java_identifier(&name) {
                    return None;
                }
                Some((owner, name, descriptor))
            }
            _ => None,
        }
    }

    /// The `(kind, owner-internal, name, descriptor)` a method reference points to. A non-constructor
    /// method name that is not a valid Java identifier bails so the body falls back to a safe
    /// skeleton instead of emitting an unparsable `recv.class(args)` call.
    fn method_ref(&self, index: u16) -> Option<(MethodRefKind, String, String, String)> {
        let (kind, class_index, name_and_type_index) = match self.pool.get(index)? {
            ConstantPoolEntry::MethodRef {
                class_index,
                name_and_type_index,
            } => (MethodRefKind::Class, *class_index, *name_and_type_index),
            ConstantPoolEntry::InterfaceMethodRef {
                class_index,
                name_and_type_index,
            } => (MethodRefKind::Interface, *class_index, *name_and_type_index),
            _ => return None,
        };
        let owner = self.pool.class_name(class_index)?.into_owned();
        let (name, descriptor) = self.name_and_type(name_and_type_index)?;
        if name != "<init>" && !Attrs::is_java_identifier(&name) {
            return None;
        }
        Some((kind, owner, name, descriptor))
    }

    /// The `(name, descriptor)` of a `NameAndType` entry.
    fn name_and_type(&self, index: u16) -> Option<(String, String)> {
        match self.pool.get(index)? {
            ConstantPoolEntry::NameAndType {
                name_index,
                descriptor_index,
            } => Some((
                self.pool.utf8(*name_index)?.into_owned(),
                self.pool.utf8(*descriptor_index)?.into_owned(),
            )),
            _ => None,
        }
    }

    /// The internal name a `Class` entry points to.
    fn class_ref(&self, index: u16) -> Option<String> {
        self.pool
            .class_name(index)
            .map(alloc::borrow::Cow::into_owned)
    }

    /// The type a `Class` entry points to, as a [`FieldType`]: an array class entry holds the full
    /// field descriptor (`[I`, `[Ljava/lang/String;`), any other a plain internal name (JVMS
    /// §4.4.1) — the ambiguity is resolved once here for every instruction that uses a class entry
    /// (`ldc`, `checkcast`, `anewarray`, `multianewarray`).
    fn class_ref_type(&self, index: u16) -> Option<FieldType> {
        let internal = self.class_ref(index)?;
        Self::internal_type(&internal)
    }

    /// A constant-pool class name as a field type. Array owners use their full descriptor while
    /// ordinary classes use an internal binary name.
    fn internal_type(internal: &str) -> Option<FieldType> {
        if internal.starts_with('[') {
            FieldType::parse(internal).ok()
        } else {
            Some(FieldType::Object(internal.into()))
        }
    }
}

/// A `lcmp`/`fcmpl`/`fcmpg`/`dcmpl`/`dcmpg` fused into the following `if<cond>` branch: the pair
/// pushes -1/0/1 and immediately tests it against 0, reading back a source-level `long`/`float`/
/// `double` comparison. The flavor records what either operand being NaN pushes (`*cmpl` -1,
/// `*cmpg` +1; `lcmp` compares totally), which decides whether a rendered operator is faithful.
#[derive(Clone, Copy)]
enum Cmp {
    Long,
    FloatNanNeg,
    FloatNanPos,
    DoubleNanNeg,
    DoubleNanPos,
}

impl Cmp {
    /// Classify a comparison instruction, or `None` for any other instruction.
    const fn of(ins: &Instruction) -> Option<Self> {
        match ins {
            Instruction::Lcmp => Some(Self::Long),
            Instruction::Fcmpl => Some(Self::FloatNanNeg),
            Instruction::Fcmpg => Some(Self::FloatNanPos),
            Instruction::Dcmpl => Some(Self::DoubleNanNeg),
            Instruction::Dcmpg => Some(Self::DoubleNanPos),
            _ => None,
        }
    }

    const fn base(self) -> BaseType {
        match self {
            Self::Long => BaseType::Long,
            Self::FloatNanNeg | Self::FloatNanPos => BaseType::Float,
            Self::DoubleNanNeg | Self::DoubleNanPos => BaseType::Double,
        }
    }

    /// Whether rendering the fused pair as `lhs <op> rhs` is exact — equivalent to the branch's
    /// `cmp(lhs, rhs) <op> 0` for *every* input, NaN included. `==`/`!=` are exact under either
    /// flavor (NaN's ±1 is never 0), but an ordering operator whose true side would capture NaN
    /// is not: `javac` always picks the flavor that drops NaN on the false side (`<`/`<=` compile
    /// to `*cmpg`, `>`/`>=` to `*cmpl`), so its output passes; the mismatched pairings (e.g.
    /// `!(a < b)`, which is *true* on NaN and has no single-operator rendering) must bail.
    fn exact(self, op: &str) -> bool {
        match self {
            Self::Long => true,
            Self::FloatNanNeg | Self::DoubleNanNeg => !matches!(op, "<" | "<="),
            Self::FloatNanPos | Self::DoubleNanPos => !matches!(op, ">" | ">="),
        }
    }
}

/// Recovers structured statements from a method's [`Cfg`], running each block through [`Sim`] and
/// folding forward conditional branches into `if` / `if`-`else`.
struct Structurer<'a, 'classes> {
    code: &'a [Instruction],
    cfg: &'a Cfg,
    pool: &'a ConstantPool,
    bootstrap: &'a [BootstrapMethod],
    class: &'a ClassFile,
    hierarchy: &'a ClassHierarchy<'classes>,
    owner: String,
    owner_is_interface: bool,
    direct_superclass: Option<String>,
    is_static: bool,
    locals: BTreeMap<u16, Local>,
    return_type: ReturnType,
}

impl Structurer<'_, '_> {
    /// Structure the whole method, requiring every block to be emitted exactly once — a strong guard
    /// that the recovered tree matches the actual control flow (any mismatch bails to a safe body).
    async fn structure(&self) -> Option<Vec<Stmt>> {
        let n = self.cfg.blocks.len();
        let mut visited = vec![false; n];
        let stmts = self.emit_region(0, n, n, &mut visited).await?;
        if visited.iter().any(|&seen| !seen) {
            return None;
        }
        Some(stmts)
    }

    /// The one boxed shim of the region recursion: `emit_region` and `structure_loop` recurse into
    /// nested regions through here, so the async cycle has a single `Box::pin` choke point.
    fn emit_region_boxed<'a>(
        &'a self,
        lo: usize,
        hi: usize,
        exit: usize,
        visited: &'a mut [bool],
    ) -> LocalBoxFuture<'a, Option<Vec<Stmt>>> {
        Box::pin(self.emit_region(lo, hi, exit, visited))
    }

    /// Structure the single-entry region of blocks `[lo, hi)` (entered at `lo`), whose normal exit is
    /// block `exit` (reached by a fall-through or `goto`). Returns the statements, or `None` on any
    /// shape that is not a clean acyclic tree.
    async fn emit_region(
        &self,
        lo: usize,
        hi: usize,
        exit: usize,
        visited: &mut [bool],
    ) -> Option<Vec<Stmt>> {
        let mut out = Vec::new();
        let mut b = lo;
        while b < hi {
            if visited[b] {
                return None;
            }
            // A block that is the target of a back-edge is a loop header — structure the loop and
            // resume past its exit. (Checked before marking `b` visited; the loop's blocks are
            // visited inside `structure_loop`.)
            if let Some(latch) = self.loop_latch(b) {
                let (loop_stmt, cont) = self.structure_loop(b, latch, hi, visited).await?;
                out.push(loop_stmt);
                b = cont;
                continue;
            }
            visited[b] = true;
            let (mut stmts, cond_stack, cmp) = self.run_block(b).await?;
            out.append(&mut stmts);
            // Only a conditional-branch block may leave operands (its condition) on the stack; a
            // leftover on any other terminator means we mis-read the block, so bail.
            if !cond_stack.is_empty() && !matches!(self.cfg.blocks[b].term, Term::Branch { .. }) {
                return None;
            }
            match &self.cfg.blocks[b].term {
                Term::Fall(next) => {
                    let next = *next;
                    if next == b + 1 && next < hi {
                        b = next;
                    } else if next == exit {
                        break;
                    } else {
                        return None;
                    }
                }
                Term::Ret | Term::Throw => {
                    b += 1;
                }
                Term::Goto(target) => {
                    if *target == exit {
                        break;
                    }
                    return None;
                }
                Term::Branch {
                    instr,
                    taken,
                    fallthrough,
                } => {
                    let (instr, taken, fallthrough) = (*instr, *taken, *fallthrough);
                    let cond = Self::branch_condition(&self.code[instr], true, cmp, cond_stack)?;
                    // Acyclic only: the fall-through is the next block and the taken edge is forward
                    // and within this region. A back-edge (loop) or a jump out is not yet structured.
                    if fallthrough != b + 1 || taken <= b || taken > hi {
                        return None;
                    }
                    // If the block just before `taken` jumps forward past it, that is the `else`'s
                    // trailing skip: `taken..e` is the `else` and `e` the join; otherwise no `else`.
                    let (then, els, join) = match self.cfg.blocks[taken - 1].term {
                        Term::Goto(e) if e > taken && e <= hi => {
                            let then = self
                                .emit_region_boxed(fallthrough, taken, e, visited)
                                .await?;
                            let els = self.emit_region_boxed(taken, e, e, visited).await?;
                            (then, els, e)
                        }
                        _ => {
                            let then = self
                                .emit_region_boxed(fallthrough, taken, taken, visited)
                                .await?;
                            (then, Vec::new(), taken)
                        }
                    };
                    out.push(Stmt::If { cond, then, els });
                    b = join;
                }
            }
        }
        Some(out)
    }

    /// Replay one block's value-level effects, returning its statements, any operand(s) left on the
    /// stack (the condition of a conditional-branch block; empty for every other block), and the
    /// flavor of a trailing `*cmp` fused into the block's conditional branch (its two operands are
    /// then the leftover stack, for [`Self::branch_condition`] to read back).
    async fn run_block(&self, b: usize) -> Option<(Vec<Stmt>, Vec<StackValue>, Option<Cmp>)> {
        let mut sim = Sim {
            pool: self.pool,
            bootstrap: self.bootstrap,
            class: self.class,
            hierarchy: self.hierarchy,
            owner: &self.owner,
            owner_is_interface: self.owner_is_interface,
            direct_superclass: self.direct_superclass.as_deref(),
            is_static: self.is_static,
            locals: &self.locals,
            return_type: &self.return_type,
            stack: Vec::new(),
            stmts: Vec::new(),
        };
        // A `*cmp` directly feeding the block's conditional branch is interpreted alongside that
        // branch (`Sim` has no encoding for its -1/0/1 result), so leave it — and its two operands,
        // which stay on the stack — to `branch_condition`.
        let mut body = self.cfg.blocks[b].body();
        let cmp = match self.cfg.blocks[b].term {
            Term::Branch { .. } if !body.is_empty() => Cmp::of(&self.code[body.end - 1]),
            _ => None,
        };
        if cmp.is_some() {
            body.end -= 1;
        }
        let mut yielder = Yielder::new();
        for ins in &self.code[body] {
            yielder.tick().await;
            sim.step(ins)?;
        }
        // Finalize anything left on the stack — a still-collecting array initializer or a leaked
        // fold marker (e.g. an initializer whose element expression spans blocks) must never
        // escape the block, and the leftover condition operands must be renderable.
        let stack = sim
            .stack
            .into_iter()
            .map(Sim::finalize)
            .collect::<Option<Vec<_>>>()?;
        Some((sim.stmts, stack, cmp))
    }

    /// If block `header` is the target of exactly one back-edge — a `goto` / conditional branch from a
    /// block at or after it — return that latch block; `None` if `header` is not a single-back-edge
    /// loop header. More than one back-edge (a multi-latch / irreducible loop) yields `None`, and the
    /// unstructured back-edge then bails through the normal terminator handling.
    fn loop_latch(&self, header: usize) -> Option<usize> {
        let mut latch = None;
        for (b, block) in self.cfg.blocks.iter().enumerate().skip(header) {
            let targets_header = match block.term {
                Term::Goto(t) => t == header,
                Term::Branch { taken, .. } => taken == header,
                _ => false,
            };
            if targets_header {
                if latch.is_some() {
                    return None;
                }
                latch = Some(b);
            }
        }
        latch
    }

    /// Structure the natural loop with this `header` and `latch` into a `while` / `do`-`while`,
    /// returning the statement and the block to resume at (the loop exit). Handles the two shapes
    /// `javac` emits — a top-test `while` (the header's branch exits the loop, the latch's `goto`
    /// jumps back) and a `do`-`while` (the latch's conditional branch is itself the back-edge) — and
    /// bails on anything else (a `break`/`continue` edge, an irregular exit, a side-effecting header).
    async fn structure_loop(
        &self,
        header: usize,
        latch: usize,
        hi: usize,
        visited: &mut [bool],
    ) -> Option<(Stmt, usize)> {
        match &self.cfg.blocks[latch].term {
            // do-while: the latch's conditional branch jumps back to the header to repeat the loop.
            Term::Branch {
                instr,
                taken,
                fallthrough,
            } if *taken == header => {
                let (instr, exit) = (*instr, *fallthrough);
                // The loop exit is the fall-through — right after the latch, within the region.
                if exit != latch + 1 || exit > hi {
                    return None;
                }
                // Body: the forward region `[header, latch)` that flows into the latch, then the
                // latch's own statements finish the body (its leftover operands are the condition).
                let mut body = self
                    .emit_region_boxed(header, latch, latch, visited)
                    .await?;
                Self::claim(visited, latch)?;
                let (mut tail, cond_stack, cmp) = self.run_block(latch).await?;
                body.append(&mut tail);
                let cond = Self::branch_condition(&self.code[instr], false, cmp, cond_stack)?;
                Some((Stmt::DoWhile { body, cond }, exit))
            }
            // while (top-test): the latch's `goto` is the back-edge; the header's branch exits.
            Term::Goto(t) if *t == header => {
                let (instr, exit, body_start) = match &self.cfg.blocks[header].term {
                    Term::Branch {
                        instr,
                        taken,
                        fallthrough,
                    } => (*instr, *taken, *fallthrough),
                    _ => return None,
                };
                // The body immediately follows the header, and the exit is forward past the latch.
                if body_start != header + 1 || exit <= latch || exit > hi {
                    return None;
                }
                Self::claim(visited, header)?;
                // The header carries only the loop condition — a side effect there would repeat.
                let (head_stmts, cond_stack, cmp) = self.run_block(header).await?;
                if !head_stmts.is_empty() {
                    return None;
                }
                let cond = Self::branch_condition(&self.code[instr], true, cmp, cond_stack)?;
                // The body `[body_start, latch]` exits back to the header (the latch's goto-back).
                let body = self
                    .emit_region_boxed(body_start, latch + 1, header, visited)
                    .await?;
                Some((Stmt::While { cond, body }, exit))
            }
            _ => None,
        }
    }

    /// Claim block `b` as emitted exactly once — the "emitted exactly once" invariant `structure`
    /// asserts — bailing if it was already visited.
    fn claim(visited: &mut [bool], b: usize) -> Option<()> {
        if visited[b] {
            return None;
        }
        visited[b] = true;
        Some(())
    }

    /// Recover the source condition from a conditional branch and the operand(s) it tested. With
    /// `negate` the *fall-through* condition is returned — the branch is taken to skip a body, so
    /// control falls through under the negation of its jump test (a forward `if`, or a top-test
    /// `while` whose branch exits the loop). Without it the branch's own (positive) jump test is
    /// returned — the branch is taken to *continue* the loop (the `while (cond)` of a `do`-`while`).
    /// With `cmp` the branch tests a fused `*cmp` result against 0 and the two operands are the
    /// stack: `lhs <op> rhs` reads back the source `long`/`float`/`double` comparison.
    fn branch_condition(
        branch: &Instruction,
        negate: bool,
        cmp: Option<Cmp>,
        mut stack: Vec<StackValue>,
    ) -> Option<Expr> {
        use Instruction as I;
        // Pick the branch-taken operator, or its negation for the fall-through condition.
        let op = |taken, negated| if negate { negated } else { taken };
        let cond = if let Some(cmp) = cmp {
            // Only the six int-zero tests can follow a fused `*cmp`, and the rendered operator
            // must agree with the flavor on NaN (see `Cmp::exact`).
            let (taken, negated) = Self::zero_test(branch)?;
            let operator = op(taken, negated);
            if !cmp.exact(operator) {
                return None;
            }
            Self::compare_base(operator, cmp.base(), &mut stack)?
        } else {
            match branch {
                I::IfIcmpeq(_) => Self::compare_int_eq(op("==", "!="), &mut stack)?,
                I::IfIcmpne(_) => Self::compare_int_eq(op("!=", "=="), &mut stack)?,
                I::IfAcmpeq(_) => Self::compare_ref(op("==", "!="), &mut stack)?,
                I::IfAcmpne(_) => Self::compare_ref(op("!=", "=="), &mut stack)?,
                I::IfIcmplt(_) => Self::compare_int(op("<", ">="), &mut stack)?,
                I::IfIcmpge(_) => Self::compare_int(op(">=", "<"), &mut stack)?,
                I::IfIcmpgt(_) => Self::compare_int(op(">", "<="), &mut stack)?,
                I::IfIcmple(_) => Self::compare_int(op("<=", ">"), &mut stack)?,
                I::Iflt(_) | I::Ifge(_) | I::Ifgt(_) | I::Ifle(_) => {
                    let (taken, negated) = Self::zero_test(branch)?;
                    Self::compare_int_zero(op(taken, negated), &mut stack)?
                }
                I::IfNull(_) => Self::compare_null(op("==", "!="), &mut stack)?,
                I::IfNonNull(_) => Self::compare_null(op("!=", "=="), &mut stack)?,
                // `ifeq`/`ifne` carry both Java booleans and int-family zero tests. The retained
                // descriptor type decides whether to render truth/not or an explicit comparison.
                I::Ifne(_) | I::Ifeq(_) => {
                    let value = stack.pop()?;
                    if matches!(
                        value.ty,
                        StackType::Field(FieldType::Base(BaseType::Boolean))
                    ) {
                        if matches!(branch, I::Ifne(_)) == negate {
                            Expr::Unary {
                                op: "!",
                                expr: Box::new(value.expr),
                            }
                        } else {
                            value.expr
                        }
                    } else {
                        let value = Sim::int_numeric_expr(value)?;
                        let (taken, negated) = Self::zero_test(branch)?;
                        Self::binary_compare(op(taken, negated), value, Expr::lit("0"))
                    }
                }
                _ => return None,
            }
        };
        // The condition must consume exactly the operands the block left; a leftover means we
        // mis-read it.
        if stack.is_empty() { Some(cond) } else { None }
    }

    /// The `(taken, negated)` source operator of an `if<cond>` integer zero test — one table
    /// shared by the plain `x <op> 0` rendering and a fused `*cmp` comparison (whose -1/0/1
    /// result the branch tests against 0).
    const fn zero_test(branch: &Instruction) -> Option<(&'static str, &'static str)> {
        use Instruction as I;
        Some(match branch {
            I::Ifeq(_) => ("==", "!="),
            I::Ifne(_) => ("!=", "=="),
            I::Iflt(_) => ("<", ">="),
            I::Ifge(_) => (">=", "<"),
            I::Ifgt(_) => (">", "<="),
            I::Ifle(_) => ("<=", ">"),
            _ => return None,
        })
    }

    fn binary_compare(op: &'static str, lhs: Expr, rhs: Expr) -> Expr {
        Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    fn compare_base(op: &'static str, base: BaseType, stack: &mut Vec<StackValue>) -> Option<Expr> {
        let rhs = stack.pop()?;
        let lhs = stack.pop()?;
        Some(Self::binary_compare(
            op,
            Sim::exact_base_expr(lhs, base)?,
            Sim::exact_base_expr(rhs, base)?,
        ))
    }

    fn compare_int(op: &'static str, stack: &mut Vec<StackValue>) -> Option<Expr> {
        let rhs = Sim::int_numeric_expr(stack.pop()?)?;
        let lhs = Sim::int_numeric_expr(stack.pop()?)?;
        Some(Self::binary_compare(op, lhs, rhs))
    }

    fn compare_int_eq(op: &'static str, stack: &mut Vec<StackValue>) -> Option<Expr> {
        let rhs = stack.pop()?;
        let lhs = stack.pop()?;
        let lhs_boolean = matches!(lhs.ty, StackType::Field(FieldType::Base(BaseType::Boolean)));
        let rhs_boolean = matches!(rhs.ty, StackType::Field(FieldType::Base(BaseType::Boolean)));
        let (lhs, rhs) = if lhs_boolean || rhs_boolean {
            let boolean = FieldType::Base(BaseType::Boolean);
            (
                Sim::consume_as(lhs, &boolean)?,
                Sim::consume_as(rhs, &boolean)?,
            )
        } else {
            (Sim::int_numeric_expr(lhs)?, Sim::int_numeric_expr(rhs)?)
        };
        Some(Self::binary_compare(op, lhs, rhs))
    }

    fn compare_ref(op: &'static str, stack: &mut Vec<StackValue>) -> Option<Expr> {
        let rhs = stack.pop()?;
        let lhs = stack.pop()?;
        if !lhs.ty.is_reference_compatible() || !rhs.ty.is_reference_compatible() {
            return None;
        }
        let directly_comparable = matches!(lhs.ty, StackType::Null)
            || matches!(rhs.ty, StackType::Null)
            || lhs.ty == rhs.ty;
        let (lhs, rhs) = if directly_comparable {
            (lhs.expr, rhs.expr)
        } else {
            let cast = |expr| Expr::Cast {
                ty: "java.lang.Object".into(),
                expr: Box::new(expr),
            };
            (cast(lhs.expr), cast(rhs.expr))
        };
        Some(Self::binary_compare(op, lhs, rhs))
    }

    fn compare_int_zero(op: &'static str, stack: &mut Vec<StackValue>) -> Option<Expr> {
        let lhs = Sim::int_numeric_expr(stack.pop()?)?;
        Some(Self::binary_compare(op, lhs, Expr::lit("0")))
    }

    fn compare_null(op: &'static str, stack: &mut Vec<StackValue>) -> Option<Expr> {
        let lhs = stack.pop()?;
        if lhs.ty.is_reference_compatible() {
            Some(Self::binary_compare(op, lhs.expr, Expr::lit("null")))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseType, Cmp, Expr, FieldType, Sim, StackValue};

    fn consume_int(value: i32, expected: BaseType) -> Option<String> {
        Sim::consume_as(StackValue::int_literal(value), &FieldType::Base(expected))
            .map(|expr| expr.render())
    }

    #[test]
    fn int_constants_regain_narrow_source_types() {
        assert_eq!(consume_int(0, BaseType::Boolean).as_deref(), Some("false"));
        assert_eq!(consume_int(1, BaseType::Boolean).as_deref(), Some("true"));
        assert_eq!(consume_int(2, BaseType::Boolean), None);
        assert_eq!(consume_int(65, BaseType::Char).as_deref(), Some("'A'"));
        assert_eq!(
            consume_int(0xD800, BaseType::Char).as_deref(),
            Some("(char) 55296")
        );
        assert_eq!(consume_int(-1, BaseType::Char), None);
        assert_eq!(consume_int(0x1_0000, BaseType::Char), None);
        assert_eq!(consume_int(1, BaseType::Byte).as_deref(), Some("(byte) 1"));
        assert_eq!(
            consume_int(1, BaseType::Short).as_deref(),
            Some("(short) 1")
        );
    }

    #[test]
    fn descriptor_adaptation_preserves_reference_static_types() {
        let object = Sim::consume_as(
            StackValue::object(Expr::Local("s".into()), "java/lang/String"),
            &FieldType::Object("java/lang/Object".into()),
        )
        .expect("reference cast");
        let null = Sim::consume_as(
            StackValue {
                expr: Expr::lit("null"),
                ty: super::StackType::Null,
            },
            &FieldType::Object("java/lang/String".into()),
        )
        .expect("typed null");
        assert_eq!(object.render(), "(java.lang.Object) s");
        assert_eq!(null.render(), "(java.lang.String) null");
    }

    #[test]
    fn class_constants_distinguish_names_from_array_descriptors() {
        assert_eq!(Sim::internal_type("I"), Some(FieldType::Object("I".into())));
        assert_eq!(Sim::internal_type("[I"), FieldType::parse("[I").ok());
        for malformed in ["[", "[V", "[Ljava/lang/String", "[I;"] {
            assert_eq!(Sim::internal_type(malformed), None, "{malformed}");
        }
    }

    // The fixtures only cover the flavor/operator pairings `javac` emits, so the full NaN
    // faithfulness table is asserted here: an ordering operator whose true side would capture
    // NaN (the flavor's sign) is inexact, everything else — and all of `lcmp` — is exact.
    #[test]
    fn cmp_exactness_table() {
        for (cmp, expected) in [
            (Cmp::Long, [true, true, true, true, true, true]),
            (Cmp::FloatNanNeg, [true, true, false, false, true, true]),
            (Cmp::DoubleNanPos, [true, true, true, true, false, false]),
        ] {
            for (op, exact) in ["==", "!=", "<", "<=", ">", ">="].into_iter().zip(expected) {
                assert_eq!(cmp.exact(op), exact, "{op}");
            }
        }
        assert_eq!(Cmp::Long.base(), BaseType::Long);
        assert_eq!(Cmp::FloatNanNeg.base(), BaseType::Float);
        assert_eq!(Cmp::DoubleNanPos.base(), BaseType::Double);
    }
}
