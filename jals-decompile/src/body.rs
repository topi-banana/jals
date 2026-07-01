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

use std::collections::HashMap;

use jals_classfile::{
    AttributeBody, ClassFile, ConstantPool, ConstantPoolEntry, Instruction, MethodInfo, ReturnType,
    parse_method_descriptor,
};

use crate::attrs::parameter_slots;
use crate::cfg::{self, Cfg, Term};
use crate::expr::{Expr, Stmt, render_block};
use crate::literal::{class_literal, double_literal, float_literal, string_literal};
use crate::types::internal_to_java;

/// Reconstruct a method's body as indented Java statement lines, or `None` if it cannot be decompiled
/// confidently (a control-flow shape not yet modelled, an exception handler, or any unsupported
/// instruction). The caller wraps the lines in a block and falls back to a safe placeholder on `None`.
///
/// `param_names` are the exact parameter names the caller renders in the signature, in order; the
/// body reuses them (never a name the signature doesn't declare), and a mismatch between them and the
/// descriptor's parameters (a generic signature that hides synthetic parameters, e.g. an `enum`
/// constructor's `String, int`) makes this bail so the body can never reference a phantom parameter.
pub fn decompile_method_body(
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
    let locals = local_slots(method, pool, is_static, param_names)?;
    let cfg = cfg::build(&code.code)?;
    let structurer = Structurer {
        code: &code.code,
        cfg: &cfg,
        pool,
        owner,
        is_static,
        locals,
    };
    let stmts = structurer.structure()?;
    Some(render_body(&stmts))
}

/// The parameter slot → source-name map (slot 0 is `this` for an instance method and is not listed),
/// naming each slot from `param_names`. Returns `None` when the descriptor's parameter count differs
/// from `param_names`, so the body cannot name a slot the signature does not declare.
fn local_slots(
    method: &MethodInfo,
    pool: &ConstantPool,
    is_static: bool,
    param_names: &[String],
) -> Option<HashMap<u16, String>> {
    let descriptor = pool.utf8(method.descriptor_index)?;
    let params = parse_method_descriptor(&descriptor).ok()?.params;
    if params.len() != param_names.len() {
        return None;
    }
    let map = parameter_slots(&params, is_static)
        .zip(param_names)
        .map(|((slot, _param), name)| (slot, name.clone()))
        .collect();
    Some(map)
}

/// Trim a trailing implicit `return;` (a `void` method's fall-off return) and render the rest.
fn render_body(stmts: &[Stmt]) -> Vec<String> {
    let end = if matches!(stmts.last(), Some(Stmt::Return(None))) {
        stmts.len() - 1
    } else {
        stmts.len()
    };
    render_block(&stmts[..end])
}

/// The straight-line symbolic-execution state for one basic block.
struct Sim<'a> {
    pool: &'a ConstantPool,
    /// Internal binary name of the class being decompiled (for `this`-call vs object-creation).
    owner: &'a str,
    is_static: bool,
    locals: &'a HashMap<u16, String>,
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
        let md = parse_method_descriptor(&descriptor).ok()?;
        let args = self.pop_args(md.params.len())?;
        let recv = if is_static {
            Box::new(Expr::Type(internal_to_java(&owner)))
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
        let md = parse_method_descriptor(&descriptor).ok()?;
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
            Some(Expr::Type(internal_to_java(owner)))
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
        match ins {
            I::Nop => {}

            // Constants.
            I::AconstNull => self.stack.push(lit("null")),
            I::IconstM1 => self.stack.push(lit("-1")),
            I::Iconst0 => self.stack.push(lit("0")),
            I::Iconst1 => self.stack.push(lit("1")),
            I::Iconst2 => self.stack.push(lit("2")),
            I::Iconst3 => self.stack.push(lit("3")),
            I::Iconst4 => self.stack.push(lit("4")),
            I::Iconst5 => self.stack.push(lit("5")),
            I::Lconst0 => self.stack.push(lit("0L")),
            I::Lconst1 => self.stack.push(lit("1L")),
            I::Fconst0 => self.stack.push(lit(float_literal(0.0))),
            I::Fconst1 => self.stack.push(lit(float_literal(1.0))),
            I::Fconst2 => self.stack.push(lit(float_literal(2.0))),
            I::Dconst0 => self.stack.push(lit(double_literal(0.0))),
            I::Dconst1 => self.stack.push(lit(double_literal(1.0))),
            I::Bipush(v) => self.stack.push(lit(v.to_string())),
            I::Sipush(v) => self.stack.push(lit(v.to_string())),
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
                self.load(u16::from(*s))?
            }
            I::Iload0 | I::Lload0 | I::Fload0 | I::Dload0 | I::Aload0 => self.load(0)?,
            I::Iload1 | I::Lload1 | I::Fload1 | I::Dload1 | I::Aload1 => self.load(1)?,
            I::Iload2 | I::Lload2 | I::Fload2 | I::Dload2 | I::Aload2 => self.load(2)?,
            I::Iload3 | I::Lload3 | I::Fload3 | I::Dload3 | I::Aload3 => self.load(3)?,

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
                let ty = internal_to_java(&self.class_ref(*i)?);
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
                self.cast(internal_to_java(&internal))?;
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

            // Everything else — branches, switches, `jsr`/`ret`, local stores, array ops, `iinc`,
            // comparisons, monitors, `invokedynamic`, `wide`, exotic stack shuffles — is out of M1's
            // straight-line scope. Bail so the caller keeps its safe body.
            _ => return None,
        }
        Some(())
    }

    /// Resolve a constant-pool constant (for `ldc` / `ldc_w` / `ldc2_w`) to a literal expression.
    fn constant(&self, index: u16) -> Option<Expr> {
        Some(match self.pool.get(index)? {
            ConstantPoolEntry::Integer(v) => lit(v.to_string()),
            ConstantPoolEntry::Long(v) => lit(format!("{v}L")),
            ConstantPoolEntry::Float(v) => lit(float_literal(*v)),
            ConstantPoolEntry::Double(v) => lit(double_literal(*v)),
            ConstantPoolEntry::String { string_index } => {
                lit(string_literal(&self.pool.utf8(*string_index)?))
            }
            ConstantPoolEntry::Class { name_index } => {
                lit(class_literal(&self.pool.utf8(*name_index)?))
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
        self.pool.class_name(index).map(|c| c.into_owned())
    }
}

/// A literal expression from already-rendered Java source text.
fn lit(text: impl Into<String>) -> Expr {
    Expr::Literal(text.into())
}

/// Recovers structured statements from a method's [`Cfg`], running each block through [`Sim`] and
/// folding forward conditional branches into `if` / `if`-`else`.
struct Structurer<'a> {
    code: &'a [Instruction],
    cfg: &'a Cfg,
    pool: &'a ConstantPool,
    owner: String,
    is_static: bool,
    locals: HashMap<u16, String>,
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
                    let cond = branch_condition(&self.code[instr], cond_stack)?;
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
}

/// Recover the source `if` condition from a conditional branch and the operand(s) it tested. The
/// condition is the one under which the branch is *not* taken (control falls into the `then` body),
/// so it is the negation of the branch's jump condition.
fn branch_condition(branch: &Instruction, mut stack: Vec<Expr>) -> Option<Expr> {
    use Instruction as I;
    let cond = match branch {
        I::IfIcmpeq(_) | I::IfAcmpeq(_) => compare("!=", &mut stack)?,
        I::IfIcmpne(_) | I::IfAcmpne(_) => compare("==", &mut stack)?,
        I::IfIcmplt(_) => compare(">=", &mut stack)?,
        I::IfIcmpge(_) => compare("<", &mut stack)?,
        I::IfIcmpgt(_) => compare("<=", &mut stack)?,
        I::IfIcmple(_) => compare(">", &mut stack)?,
        I::Iflt(_) => compare_lit(">=", "0", &mut stack)?,
        I::Ifge(_) => compare_lit("<", "0", &mut stack)?,
        I::Ifgt(_) => compare_lit("<=", "0", &mut stack)?,
        I::Ifle(_) => compare_lit(">", "0", &mut stack)?,
        I::IfNull(_) => compare_lit("!=", "null", &mut stack)?,
        I::IfNonNull(_) => compare_lit("==", "null", &mut stack)?,
        // A bare `ifeq` / `ifne` tests a boolean value: `ifeq` skips when false, so the `then` runs
        // when it is truthy; `ifne` skips when true, so the `then` runs when it is negated.
        I::Ifeq(_) => stack.pop()?,
        I::Ifne(_) => Expr::Unary {
            op: "!",
            expr: Box::new(stack.pop()?),
        },
        _ => return None,
    };
    // The condition must consume exactly the operands the block left; a leftover means we mis-read it.
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
        rhs: Box::new(lit(literal)),
    })
}
