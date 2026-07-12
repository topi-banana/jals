//! Method-body decompilation: reconstructing a method body from its bytecode.
//!
//! Two layers. The value layer ([`Sim`]) is a per-block symbolic execution: the operand stack is
//! simulated as a stack of [`Expr`] trees, and each instruction either rewrites the stack or emits a
//! [`Stmt`]. The control layer ([`Structurer`]) builds a CFG ([`crate::cfg`]) and recovers structured
//! Java from it — a straight-line method is one block, and forward conditional branches become
//! `if` / `if`-`else`. Both layers are deliberately conservative: anything not modelled (a loop /
//! back-edge, `switch`, a `try`/`catch`, a local store, `invokedynamic`, an exotic stack shuffle, or
//! a control-flow shape that is not a clean tree) makes the whole method fall back to the caller's
//! safe body — so the output is always valid Java, never a half-built or mis-structured body.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use jals_classfile::{
    AttributeBody, ClassFile, CodeAttribute, ConstantPool, ConstantPoolEntry, Instruction,
    MethodDescriptor, MethodInfo, ReturnType, WideInstruction,
};

use crate::attrs::Attrs;
use crate::cfg::{Cfg, Term};
use crate::expr::{Expr, Stmt};
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
    /// parameter.
    pub fn decompile(
        method: &MethodInfo,
        cf: &ClassFile,
        param_names: &[String],
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
        let is_static = method.access_flags.is_static();
        let mut locals = Self::local_slots(method, pool, is_static, param_names)?;
        // Hoist a typed declaration for every non-parameter local the method stores into, registering
        // each in `locals` so the body can name it — bailing if any local cannot be resolved from the
        // `LocalVariableTable` (no `-g`, a synthetic temporary, a reused slot, or a name collision).
        let decls = Self::local_declarations(code, pool, is_static, &mut locals)?;
        let cfg = Cfg::build(&code.code)?;
        let structurer = Structurer {
            code: &code.code,
            cfg: &cfg,
            pool,
            owner,
            is_static,
            locals,
        };
        let mut stmts = decls;
        stmts.extend(structurer.structure()?);
        Some(Self::render_body(&stmts))
    }

    /// The parameter slot → source-name map (slot 0 is `this` for an instance method and is not
    /// listed), naming each slot from `param_names`. Returns `None` when the descriptor's parameter
    /// count differs from `param_names`, so the body cannot name a slot the signature does not
    /// declare.
    fn local_slots(
        method: &MethodInfo,
        pool: &ConstantPool,
        is_static: bool,
        param_names: &[String],
    ) -> Option<BTreeMap<u16, String>> {
        let descriptor = pool.utf8(method.descriptor_index)?;
        let params = MethodDescriptor::parse(&descriptor).ok()?.params;
        if params.len() != param_names.len() {
            return None;
        }
        let map = Attrs::parameter_slots(&params, is_static)
            .zip(param_names)
            .map(|((slot, _param), name)| (slot, name.clone()))
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
        locals: &mut BTreeMap<u16, String>,
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
            if locals.values().any(|n| *n == name) {
                return None;
            }
            locals.insert(slot, name.clone());
            decls.push(Stmt::Declare { ty, name });
        }
        Some(decls)
    }

    /// The local slot a *store* instruction writes (a store form, its numbered shorthand, or the
    /// `wide` form), or `None` for a non-store. The single source of truth for the store opcode set,
    /// shared by declaration discovery ([`MethodBody::stored_slot`]) and the simulator
    /// ([`Sim::step`]) so the two never drift. `iinc` is deliberately excluded — it read-modify-
    /// writes and carries a delta, handled separately.
    fn store_slot(ins: &Instruction) -> Option<u16> {
        use Instruction as I;
        Some(match ins {
            I::Istore(s) | I::Lstore(s) | I::Fstore(s) | I::Dstore(s) | I::Astore(s) => {
                u16::from(*s)
            }
            I::Istore0 | I::Lstore0 | I::Fstore0 | I::Dstore0 | I::Astore0 => 0,
            I::Istore1 | I::Lstore1 | I::Fstore1 | I::Dstore1 | I::Astore1 => 1,
            I::Istore2 | I::Lstore2 | I::Fstore2 | I::Dstore2 | I::Astore2 => 2,
            I::Istore3 | I::Lstore3 | I::Fstore3 | I::Dstore3 | I::Astore3 => 3,
            I::Wide(
                WideInstruction::Istore(s)
                | WideInstruction::Lstore(s)
                | WideInstruction::Fstore(s)
                | WideInstruction::Dstore(s)
                | WideInstruction::Astore(s),
            ) => *s,
            _ => return None,
        })
    }

    /// The local slot an instruction writes — a store (via [`MethodBody::store_slot`]) or an `iinc`
    /// (and its `wide` form) — or `None` if it writes no local. Drives declaration discovery.
    fn stored_slot(ins: &Instruction) -> Option<u16> {
        use Instruction as I;
        Self::store_slot(ins).or_else(|| match ins {
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

/// The straight-line symbolic-execution state for one basic block.
struct Sim<'a> {
    pool: &'a ConstantPool,
    /// Internal binary name of the class being decompiled (for `this`-call vs object-creation).
    owner: &'a str,
    is_static: bool,
    locals: &'a BTreeMap<u16, String>,
    stack: Vec<Expr>,
    stmts: Vec<Stmt>,
}

impl Sim<'_> {
    fn pop(&mut self) -> Option<Expr> {
        self.stack.pop()
    }

    /// Pop `n` operands and return them in source (left-to-right) order.
    fn pop_args(&mut self, n: usize) -> Option<Vec<Expr>> {
        let mut args = Vec::with_capacity(n);
        for _ in 0..n {
            args.push(self.pop()?);
        }
        args.reverse();
        Some(args)
    }

    /// Push the value of a local slot (`this` for slot 0 of an instance method).
    fn load(&mut self, slot: u16) -> Option<()> {
        if !self.is_static && slot == 0 {
            self.stack.push(Expr::This);
        } else {
            let name = self.locals.get(&slot)?;
            self.stack.push(Expr::Local(name.clone()));
        }
        Some(())
    }

    /// Store the top of stack into a local: `name = value;`. The slot's name comes from the map
    /// built by [`local_declarations`] (parameters plus hoisted locals), so an unmapped slot bails.
    fn store(&mut self, slot: u16) -> Option<()> {
        let name = self.locals.get(&slot)?.clone();
        let value = self.pop()?;
        self.stmts.push(Stmt::Assign {
            target: Expr::Local(name),
            value,
        });
        Some(())
    }

    /// `iinc`: `name = name + by;` (or `name - |by|;` when negative). Reads and writes the local in
    /// place — the operand stack is untouched.
    fn iinc(&mut self, slot: u16, by: i32) -> Option<()> {
        let name = self.locals.get(&slot)?.clone();
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

    fn binary(&mut self, op: &'static str) -> Option<()> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.stack.push(Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        });
        Some(())
    }

    fn unary(&mut self, op: &'static str) -> Option<()> {
        let expr = self.pop()?;
        self.stack.push(Expr::Unary {
            op,
            expr: Box::new(expr),
        });
        Some(())
    }

    fn cast(&mut self, ty: String) -> Option<()> {
        let expr = self.pop()?;
        self.stack.push(Expr::Cast {
            ty,
            expr: Box::new(expr),
        });
        Some(())
    }

    /// Emit an invocation whose receiver is already known (`recv` = `None` for a static call, which
    /// carries its owner type as its receiver expression instead).
    fn invoke(&mut self, index: u16, is_static: bool) -> Option<()> {
        let (owner, name, descriptor) = self.method_ref(index)?;
        let md = MethodDescriptor::parse(&descriptor).ok()?;
        let args = self.pop_args(md.params.len())?;
        let recv = if is_static {
            Box::new(Expr::Type(JavaType::internal_to_java(&owner)))
        } else {
            Box::new(self.pop()?)
        };
        self.emit_call(
            Expr::Call {
                recv: Some(recv),
                name,
                args,
            },
            matches!(md.return_type, ReturnType::Void),
        );
        Some(())
    }

    /// Handle `invokespecial`: a constructor chain (`super(...)` / `this(...)`), object creation
    /// (`new X(...)`), or a non-virtual instance call (a `private` / `super.m()` method).
    fn invoke_special(&mut self, index: u16) -> Option<()> {
        let (owner, name, descriptor) = self.method_ref(index)?;
        let md = MethodDescriptor::parse(&descriptor).ok()?;
        let args = self.pop_args(md.params.len())?;
        if name != "<init>" {
            let recv = self.pop()?;
            self.emit_call(
                Expr::Call {
                    recv: Some(Box::new(recv)),
                    name,
                    args,
                },
                matches!(md.return_type, ReturnType::Void),
            );
            return Some(());
        }
        match self.pop()? {
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
                if matches!(self.stack.last(), Some(Expr::Uninitialized(t)) if *t == ty) {
                    self.stack.pop();
                }
                self.stack.push(Expr::New { ty, args });
            }
            _ => return None,
        }
        Some(())
    }

    /// Push a call (value result) or emit it as a statement (`void` result).
    fn emit_call(&mut self, call: Expr, is_void: bool) {
        if is_void {
            self.stmts.push(Stmt::Expr(call));
        } else {
            self.stack.push(call);
        }
    }

    /// The receiver of a field access / store: the owner type for a `static` field, else the object
    /// reference popped from the stack.
    fn field_receiver(&mut self, owner: &str, is_static: bool) -> Option<Expr> {
        if is_static {
            Some(Expr::Type(JavaType::internal_to_java(owner)))
        } else {
            self.pop()
        }
    }

    fn field_access(&mut self, index: u16, is_static: bool) -> Option<()> {
        let (owner, name, _) = self.field_ref(index)?;
        let recv = self.field_receiver(&owner, is_static)?;
        self.stack.push(Expr::Field {
            recv: Box::new(recv),
            name,
        });
        Some(())
    }

    fn field_store(&mut self, index: u16, is_static: bool) -> Option<()> {
        let (owner, name, _) = self.field_ref(index)?;
        let value = self.pop()?;
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

    fn step(&mut self, ins: &Instruction) -> Option<()> {
        use Instruction as I;
        // Local stores (all forms) — the opcode set lives in `store_slot`, shared with declaration
        // discovery so the simulator and the pre-pass can never drift. Handled before the `match`
        // (a guard can't bind the slot there) so `store_slot` is computed once.
        if let Some(slot) = MethodBody::store_slot(ins) {
            return self.store(slot);
        }
        match ins {
            I::Nop => {}

            // Constants.
            I::AconstNull => self.stack.push(Expr::lit("null")),
            I::IconstM1 => self.stack.push(Expr::lit("-1")),
            I::Iconst0 => self.stack.push(Expr::lit("0")),
            I::Iconst1 => self.stack.push(Expr::lit("1")),
            I::Iconst2 => self.stack.push(Expr::lit("2")),
            I::Iconst3 => self.stack.push(Expr::lit("3")),
            I::Iconst4 => self.stack.push(Expr::lit("4")),
            I::Iconst5 => self.stack.push(Expr::lit("5")),
            I::Lconst0 => self.stack.push(Expr::lit("0L")),
            I::Lconst1 => self.stack.push(Expr::lit("1L")),
            I::Fconst0 => self.stack.push(Expr::lit(Literal::float_literal(0.0))),
            I::Fconst1 => self.stack.push(Expr::lit(Literal::float_literal(1.0))),
            I::Fconst2 => self.stack.push(Expr::lit(Literal::float_literal(2.0))),
            I::Dconst0 => self.stack.push(Expr::lit(Literal::double_literal(0.0))),
            I::Dconst1 => self.stack.push(Expr::lit(Literal::double_literal(1.0))),
            I::Bipush(v) => self.stack.push(Expr::lit(v.to_string())),
            I::Sipush(v) => self.stack.push(Expr::lit(v.to_string())),
            I::Ldc(i) => {
                let e = self.constant(u16::from(*i))?;
                self.stack.push(e);
            }
            I::LdcW(i) | I::Ldc2W(i) => {
                let e = self.constant(*i)?;
                self.stack.push(e);
            }

            // Loads (slot forms and the numbered shorthands).
            I::Iload(s) | I::Lload(s) | I::Fload(s) | I::Dload(s) | I::Aload(s) => {
                self.load(u16::from(*s))?;
            }
            I::Iload0 | I::Lload0 | I::Fload0 | I::Dload0 | I::Aload0 => self.load(0)?,
            I::Iload1 | I::Lload1 | I::Fload1 | I::Dload1 | I::Aload1 => self.load(1)?,
            I::Iload2 | I::Lload2 | I::Fload2 | I::Dload2 | I::Aload2 => self.load(2)?,
            I::Iload3 | I::Lload3 | I::Fload3 | I::Dload3 | I::Aload3 => self.load(3)?,

            // `iinc` (and its wide form): a read-modify-write of a local, stack untouched.
            I::Iinc { index, value } => self.iinc(u16::from(*index), i32::from(*value))?,
            I::Wide(WideInstruction::Iinc { index, value }) => {
                self.iinc(*index, i32::from(*value))?;
            }

            // Arithmetic and bitwise.
            I::Iadd | I::Ladd | I::Fadd | I::Dadd => self.binary("+")?,
            I::Isub | I::Lsub | I::Fsub | I::Dsub => self.binary("-")?,
            I::Imul | I::Lmul | I::Fmul | I::Dmul => self.binary("*")?,
            I::Idiv | I::Ldiv | I::Fdiv | I::Ddiv => self.binary("/")?,
            I::Irem | I::Lrem | I::Frem | I::Drem => self.binary("%")?,
            I::Ineg | I::Lneg | I::Fneg | I::Dneg => self.unary("-")?,
            I::Ishl | I::Lshl => self.binary("<<")?,
            I::Ishr | I::Lshr => self.binary(">>")?,
            I::Iushr | I::Lushr => self.binary(">>>")?,
            I::Iand | I::Land => self.binary("&")?,
            I::Ior | I::Lor => self.binary("|")?,
            I::Ixor | I::Lxor => self.binary("^")?,

            // Numeric conversions.
            I::I2l | I::F2l | I::D2l => self.cast("long".into())?,
            I::I2f | I::L2f | I::D2f => self.cast("float".into())?,
            I::I2d | I::L2d | I::F2d => self.cast("double".into())?,
            I::L2i | I::F2i | I::D2i => self.cast("int".into())?,
            I::I2b => self.cast("byte".into())?,
            I::I2c => self.cast("char".into())?,
            I::I2s => self.cast("short".into())?,

            // Field access.
            I::GetField(i) => self.field_access(*i, false)?,
            I::GetStatic(i) => self.field_access(*i, true)?,
            I::PutField(i) => self.field_store(*i, false)?,
            I::PutStatic(i) => self.field_store(*i, true)?,

            // Invocations.
            I::InvokeVirtual(i) | I::InvokeInterface { index: i, .. } => self.invoke(*i, false)?,
            I::InvokeStatic(i) => self.invoke(*i, true)?,
            I::InvokeSpecial(i) => self.invoke_special(*i)?,

            // Object creation.
            I::New(i) => {
                let ty = JavaType::internal_to_java(&self.class_ref(*i)?);
                self.stack.push(Expr::Uninitialized(ty));
            }
            I::Dup => match self.stack.last() {
                // Only the object-creation `new; dup; …; invokespecial` shape is modelled; a `dup`
                // of any real value would duplicate a side effect, so bail.
                Some(Expr::Uninitialized(ty)) => {
                    let ty = ty.clone();
                    self.stack.push(Expr::Uninitialized(ty));
                }
                _ => return None,
            },
            I::CheckCast(i) => {
                let internal = self.class_ref(*i)?;
                // An array-typed cast (`[L…;`) is not a plain name — leave it to a later milestone.
                if internal.starts_with('[') {
                    return None;
                }
                self.cast(JavaType::internal_to_java(&internal))?;
            }
            I::ArrayLength => {
                let array = self.pop()?;
                self.stack.push(Expr::ArrayLength(Box::new(array)));
            }

            // Returns and throw.
            I::Ireturn | I::Lreturn | I::Freturn | I::Dreturn | I::Areturn => {
                let value = self.pop()?;
                self.stmts.push(Stmt::Return(Some(value)));
            }
            I::Return => self.stmts.push(Stmt::Return(None)),
            I::Athrow => {
                let value = self.pop()?;
                self.stmts.push(Stmt::Throw(value));
            }

            // Discard: a call whose result is unused becomes an expression statement.
            I::Pop => match self.stack.last() {
                Some(Expr::Call { .. }) => {
                    let call = self.pop()?;
                    self.stmts.push(Stmt::Expr(call));
                }
                _ => {
                    self.pop()?;
                }
            },

            // Everything else — branches, switches, `jsr`/`ret`, array ops, comparisons, monitors,
            // `invokedynamic`, `wide` loads, exotic stack shuffles — is not yet modelled. Bail so the
            // caller keeps its safe body.
            _ => return None,
        }
        Some(())
    }

    /// Resolve a constant-pool constant (for `ldc` / `ldc_w` / `ldc2_w`) to a literal expression.
    fn constant(&self, index: u16) -> Option<Expr> {
        Some(match self.pool.get(index)? {
            ConstantPoolEntry::Integer(v) => Expr::lit(v.to_string()),
            ConstantPoolEntry::Long(v) => Expr::lit(format!("{v}L")),
            ConstantPoolEntry::Float(v) => Expr::lit(Literal::float_literal(*v)),
            ConstantPoolEntry::Double(v) => Expr::lit(Literal::double_literal(*v)),
            ConstantPoolEntry::String { string_index } => {
                Expr::lit(Literal::string_literal(&self.pool.utf8(*string_index)?))
            }
            ConstantPoolEntry::Class { name_index } => {
                Expr::lit(Literal::class_literal(&self.pool.utf8(*name_index)?))
            }
            _ => return None,
        })
    }

    /// The `(owner-internal, name, descriptor)` a `FieldRef` points to.
    fn field_ref(&self, index: u16) -> Option<(String, String, String)> {
        match self.pool.get(index)? {
            ConstantPoolEntry::FieldRef {
                class_index,
                name_and_type_index,
            } => {
                let owner = self.pool.class_name(*class_index)?.into_owned();
                let (name, descriptor) = self.name_and_type(*name_and_type_index)?;
                Some((owner, name, descriptor))
            }
            _ => None,
        }
    }

    /// The `(owner-internal, name, descriptor)` a `MethodRef` / `InterfaceMethodRef` points to.
    fn method_ref(&self, index: u16) -> Option<(String, String, String)> {
        let (class_index, nat) = match self.pool.get(index)? {
            ConstantPoolEntry::MethodRef {
                class_index,
                name_and_type_index,
            }
            | ConstantPoolEntry::InterfaceMethodRef {
                class_index,
                name_and_type_index,
            } => (*class_index, *name_and_type_index),
            _ => return None,
        };
        let owner = self.pool.class_name(class_index)?.into_owned();
        let (name, descriptor) = self.name_and_type(nat)?;
        Some((owner, name, descriptor))
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
}

/// Recovers structured statements from a method's [`Cfg`], running each block through [`Sim`] and
/// folding forward conditional branches into `if` / `if`-`else`.
struct Structurer<'a> {
    code: &'a [Instruction],
    cfg: &'a Cfg,
    pool: &'a ConstantPool,
    owner: String,
    is_static: bool,
    locals: BTreeMap<u16, String>,
}

impl Structurer<'_> {
    /// Structure the whole method, requiring every block to be emitted exactly once — a strong guard
    /// that the recovered tree matches the actual control flow (any mismatch bails to a safe body).
    fn structure(&self) -> Option<Vec<Stmt>> {
        let n = self.cfg.blocks.len();
        let mut visited = vec![false; n];
        let stmts = self.emit_region(0, n, n, &mut visited)?;
        if visited.iter().any(|&seen| !seen) {
            return None;
        }
        Some(stmts)
    }

    /// Structure the single-entry region of blocks `[lo, hi)` (entered at `lo`), whose normal exit is
    /// block `exit` (reached by a fall-through or `goto`). Returns the statements, or `None` on any
    /// shape that is not a clean acyclic tree.
    fn emit_region(
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
                let (loop_stmt, cont) = self.structure_loop(b, latch, hi, visited)?;
                out.push(loop_stmt);
                b = cont;
                continue;
            }
            visited[b] = true;
            let (mut stmts, cond_stack) = self.run_block(b)?;
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
                    let cond = Self::branch_condition(&self.code[instr], true, cond_stack)?;
                    // Acyclic only: the fall-through is the next block and the taken edge is forward
                    // and within this region. A back-edge (loop) or a jump out is not yet structured.
                    if fallthrough != b + 1 || taken <= b || taken > hi {
                        return None;
                    }
                    // If the block just before `taken` jumps forward past it, that is the `else`'s
                    // trailing skip: `taken..e` is the `else` and `e` the join; otherwise no `else`.
                    let (then, els, join) = match self.cfg.blocks[taken - 1].term {
                        Term::Goto(e) if e > taken && e <= hi => {
                            let then = self.emit_region(fallthrough, taken, e, visited)?;
                            let els = self.emit_region(taken, e, e, visited)?;
                            (then, els, e)
                        }
                        _ => {
                            let then = self.emit_region(fallthrough, taken, taken, visited)?;
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

    /// Replay one block's value-level effects, returning its statements and any operand(s) left on the
    /// stack (the condition of a conditional-branch block; empty for every other block).
    fn run_block(&self, b: usize) -> Option<(Vec<Stmt>, Vec<Expr>)> {
        let mut sim = Sim {
            pool: self.pool,
            owner: &self.owner,
            is_static: self.is_static,
            locals: &self.locals,
            stack: Vec::new(),
            stmts: Vec::new(),
        };
        for ins in &self.code[self.cfg.blocks[b].body()] {
            sim.step(ins)?;
        }
        Some((sim.stmts, sim.stack))
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
    fn structure_loop(
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
                let mut body = self.emit_region(header, latch, latch, visited)?;
                Self::claim(visited, latch)?;
                let (mut tail, cond_stack) = self.run_block(latch)?;
                body.append(&mut tail);
                let cond = Self::branch_condition(&self.code[instr], false, cond_stack)?;
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
                let (head_stmts, cond_stack) = self.run_block(header)?;
                if !head_stmts.is_empty() {
                    return None;
                }
                let cond = Self::branch_condition(&self.code[instr], true, cond_stack)?;
                // The body `[body_start, latch]` exits back to the header (the latch's goto-back).
                let body = self.emit_region(body_start, latch + 1, header, visited)?;
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
    fn branch_condition(branch: &Instruction, negate: bool, mut stack: Vec<Expr>) -> Option<Expr> {
        use Instruction as I;
        // Pick the branch-taken operator, or its negation for the fall-through condition.
        let op = |taken, negated| if negate { negated } else { taken };
        let cond = match branch {
            I::IfIcmpeq(_) | I::IfAcmpeq(_) => Self::compare(op("==", "!="), &mut stack)?,
            I::IfIcmpne(_) | I::IfAcmpne(_) => Self::compare(op("!=", "=="), &mut stack)?,
            I::IfIcmplt(_) => Self::compare(op("<", ">="), &mut stack)?,
            I::IfIcmpge(_) => Self::compare(op(">=", "<"), &mut stack)?,
            I::IfIcmpgt(_) => Self::compare(op(">", "<="), &mut stack)?,
            I::IfIcmple(_) => Self::compare(op("<=", ">"), &mut stack)?,
            I::Iflt(_) => Self::compare_lit(op("<", ">="), "0", &mut stack)?,
            I::Ifge(_) => Self::compare_lit(op(">=", "<"), "0", &mut stack)?,
            I::Ifgt(_) => Self::compare_lit(op(">", "<="), "0", &mut stack)?,
            I::Ifle(_) => Self::compare_lit(op("<=", ">"), "0", &mut stack)?,
            I::IfNull(_) => Self::compare_lit(op("==", "!="), "null", &mut stack)?,
            I::IfNonNull(_) => Self::compare_lit(op("!=", "=="), "null", &mut stack)?,
            // A bare `ifne` is taken when the value is truthy, `ifeq` when it is falsy; negating the
            // taken test gives the fall-through condition.
            I::Ifne(_) | I::Ifeq(_) => {
                let value = stack.pop()?;
                if matches!(branch, I::Ifne(_)) == negate {
                    Expr::Unary {
                        op: "!",
                        expr: Box::new(value),
                    }
                } else {
                    value
                }
            }
            _ => return None,
        };
        // The condition must consume exactly the operands the block left; a leftover means we
        // mis-read it.
        if stack.is_empty() { Some(cond) } else { None }
    }

    /// Pop two operands into `lhs op rhs`.
    fn compare(op: &'static str, stack: &mut Vec<Expr>) -> Option<Expr> {
        let rhs = stack.pop()?;
        let lhs = stack.pop()?;
        Some(Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        })
    }

    /// Pop one operand into `lhs op <literal>` (a comparison against `0` or `null`).
    fn compare_lit(op: &'static str, literal: &str, stack: &mut Vec<Expr>) -> Option<Expr> {
        let lhs = stack.pop()?;
        Some(Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(Expr::lit(literal)),
        })
    }
}
