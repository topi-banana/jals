//! Method-body decompilation: reconstructing a method body from its bytecode.
//!
//! Two layers. The value layer ([`Sim`]) is a per-block symbolic execution: the operand stack is
//! simulated as a stack of [`Expr`] trees, and each instruction either rewrites the stack or emits a
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

use crate::attrs::Attrs;
use crate::cfg::{Cfg, Term};
use crate::expr::{ArrayForm, ConcatPart, Expr, Stmt};
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
            bootstrap,
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
    /// The class's `BootstrapMethods` entries (empty when absent), resolving `invokedynamic`.
    bootstrap: &'a [BootstrapMethod],
    /// Internal binary name of the class being decompiled (for `this`-call vs object-creation).
    owner: &'a str,
    is_static: bool,
    locals: &'a BTreeMap<u16, String>,
    stack: Vec<Expr>,
    stmts: Vec<Stmt>,
}

impl Sim<'_> {
    fn pop(&mut self) -> Option<Expr> {
        Self::finalize(self.stack.pop()?)
    }

    /// Finalize a value leaving the stack: a collecting array creation becomes its final form —
    /// untouched → a plain sized creation, completely filled → a `new T[]{…}` initializer — and a
    /// partial collection or a leaked initializer-store marker bails; a collecting `StringBuilder`
    /// chain consumed by anything but its `toString()` becomes the original append call chain
    /// again. The single gate that keeps the folding sentinels ([`Expr::PendingArray`] /
    /// [`Expr::PendingArrayDup`] / [`Expr::PendingBuilder`]) out of rendered output: every
    /// consumption funnels through here (via [`Sim::pop`] or the block-end sweep).
    fn finalize(expr: Expr) -> Option<Expr> {
        match expr {
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
        }
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
        if !is_static && owner == "java/lang/StringBuilder" && self.fold_builder(&name, &md)? {
            return Some(());
        }
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
                // A fresh no-arg `StringBuilder` is recognized here, where the internal name is
                // authoritative, and pushed as the collecting sentinel: its concat-safe `append`
                // chain may fold into a `+` concatenation, and any other consumption finalizes it
                // back into the original calls ([`Sim::finalize`]), so an unfolded one renders
                // exactly as the `new` it was.
                if owner == "java/lang/StringBuilder" && args.is_empty() {
                    self.stack.push(Expr::PendingBuilder(Vec::new()));
                } else {
                    self.stack.push(Expr::New { ty, args });
                }
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
                Some(Expr::PendingBuilder(parts)) if !parts.is_empty() => {
                    let parts = core::mem::take(parts);
                    self.stack.pop();
                    self.stack.push(Expr::concat(parts));
                    Some(true)
                }
                _ => Some(false),
            },
            "append" if md.params.len() == 1 && Self::concat_safe(&md.params[0]) => {
                // The stack is `[…, receiver, operand]` — commit only when the receiver is a
                // collecting chain (a builder that came from a local, parameter, or field keeps
                // its real `append` calls).
                let receiver = self.stack.len().checked_sub(2).map(|i| &self.stack[i]);
                if !matches!(receiver, Some(Expr::PendingBuilder(_))) {
                    return Some(false);
                }
                let arg = self.pop()?;
                let part = ConcatPart {
                    expr: Self::coerce(arg, &md.params[0])?,
                    stringy: Self::is_string(&md.params[0]),
                };
                let Some(Expr::PendingBuilder(parts)) = self.stack.last_mut() else {
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

    /// Coerce a raw stack operand to the declared type of the slot consuming it: the JVM models
    /// `boolean` and `char` values as `int`s, so a constant flowing into a boolean/char-typed
    /// concatenation operand must be re-rendered (`1` → `true`, `33` → `'!'`) for the source to
    /// mean what the bytecode computed. A non-literal operand is already typed by its own source
    /// expression and passes through, as does every other operand type.
    fn coerce(expr: Expr, param: &FieldType) -> Option<Expr> {
        let FieldType::Base(base @ (BaseType::Boolean | BaseType::Char)) = param else {
            return Some(expr);
        };
        if !matches!(expr, Expr::Literal(_)) {
            return Some(expr);
        }
        // A literal here must be an `int` constant (the JVM models `boolean`/`char` as `int`);
        // any other literal spelling means we mis-read the operand, so bail.
        let value = expr.as_int_const()?;
        if matches!(base, BaseType::Boolean) {
            return match value {
                0 => Some(Expr::lit("false")),
                1 => Some(Expr::lit("true")),
                _ => None,
            };
        }
        let value = u32::try_from(value).ok()?;
        if value > 0xFFFF {
            return None;
        }
        // A lone surrogate code unit has no literal spelling; a cast keeps the value exact.
        Some(char::from_u32(value).map_or_else(
            || Expr::Cast {
                ty: "char".into(),
                expr: Box::new(expr),
            },
            |c| Expr::lit(Literal::char_literal(c)),
        ))
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
        let mut args = self.pop_args(md.params.len())?.into_iter();
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
                        expr: Self::coerce(arg, param)?,
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
        self.stack.push(Expr::concat(parts));
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
        let (owner, name, _) = self.method_ref(*reference_index)?;
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

    /// Push a one-sized-dimension array creation (`newarray` / `anewarray`), popping its length.
    /// A constant length starts a collecting [`Expr::PendingArray`] (a following `dup; <index>;
    /// <value>; Xastore` run folds into a `new T[]{…}` initializer; consumption finalizes it) — a
    /// dynamic length can never take an initializer in source, so it is final immediately.
    fn new_array(&mut self, elem: String, empty_dims: usize) -> Option<()> {
        let len = self.pop()?;
        let constant_len = len.as_int_const().and_then(|v| usize::try_from(v).ok());
        self.stack.push(match constant_len {
            Some(len) => Expr::PendingArray {
                elem,
                empty_dims,
                len,
                elems: Vec::new(),
            },
            None => Expr::NewArray {
                elem,
                empty_dims,
                form: ArrayForm::Sized(vec![len]),
            },
        });
        Some(())
    }

    /// `anewarray`: the pool entry names the *element* class — itself an array type (`[I`) for a
    /// `new int[n][]`-shaped creation.
    fn anew_array(&mut self, index: u16) -> Option<()> {
        let (elem, empty_dims) = JavaType::array_base(&self.class_ref_type(index)?);
        self.new_array(elem, empty_dims)
    }

    /// `multianewarray`: the pool entry is the full array descriptor (`[[I`); `dimensions` counts
    /// are popped as the sized dimensions, any remaining depth rendering as empty `[]` pairs
    /// (`new int[a][b]`, `new int[a][b][]`). Never collecting — no compiler runs the initializer
    /// store pattern on one (a following `dup` bails).
    fn multi_new_array(&mut self, index: u16, dimensions: u8) -> Option<()> {
        let (elem, depth) = JavaType::array_base(&self.class_ref_type(index)?);
        let dimensions = usize::from(dimensions);
        if dimensions == 0 || dimensions > depth {
            return None;
        }
        let dims = self.pop_args(dimensions)?;
        self.stack.push(Expr::NewArray {
            elem,
            empty_dims: depth - dimensions,
            form: ArrayForm::Sized(dims),
        });
        Some(())
    }

    /// An array element read (`*aload`, all eight flavors): `array[index]`. The element type never
    /// changes the rendered text, so the flavors are uniform.
    fn array_load(&mut self) -> Option<()> {
        let index = self.pop()?;
        let array = self.pop()?;
        self.stack.push(Expr::Index {
            array: Box::new(array),
            index: Box::new(index),
        });
        Some(())
    }

    /// An array element write (`*astore`, all eight flavors): fold into a collecting array
    /// initializer when the array operand is the `dup`'d creation marker, else a plain
    /// `array[index] = value;` (mirroring [`Sim::field_store`]).
    fn array_store(&mut self) -> Option<()> {
        let value = self.pop()?;
        let index = self.pop()?;
        // The array operand is popped raw: the initializer-store marker must reach `push_elem`,
        // not the finalizing `pop` (which rejects it).
        match self.stack.pop()? {
            Expr::PendingArrayDup => self.push_elem(&index, value),
            array => {
                let array = Self::finalize(array)?;
                self.stmts.push(Stmt::Assign {
                    target: Expr::Index {
                        array: Box::new(array),
                        index: Box::new(index),
                    },
                    value,
                });
                Some(())
            }
        }
    }

    /// Fold one `dup; <index>; <value>; Xastore` element store into the collecting
    /// [`Expr::PendingArray`] beneath the popped marker. Only the exact `javac` initializer shape
    /// folds — the index must be the next sequential constant from 0 and in bounds — so a partial
    /// or out-of-order fill (a default-skipping compiler) can never render a wrong-length
    /// `new T[]{…}`; anything else bails.
    fn push_elem(&mut self, index: &Expr, value: Expr) -> Option<()> {
        let Some(Expr::PendingArray {
            elem,
            empty_dims,
            len,
            elems,
        }) = self.stack.last_mut()
        else {
            return None;
        };
        let position = usize::try_from(index.as_int_const()?).ok()?;
        if position != elems.len() || position >= *len {
            return None;
        }
        // `bastore` serves both `byte[]` and `boolean[]`; the collecting creation's element type
        // pins which, so re-coerce the int constants back to boolean literals (a non-literal,
        // e.g. a boolean-typed local or call, passes through as-is).
        let value = if elem == "boolean" && *empty_dims == 0 {
            Self::coerce(value, &FieldType::Base(BaseType::Boolean))?
        } else {
            value
        };
        elems.push(value);
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
            I::InvokeDynamic { index } => self.invoke_dynamic(*index)?,

            // Object creation.
            I::New(i) => {
                let ty = JavaType::internal_to_java(&self.class_ref(*i)?);
                self.stack.push(Expr::Uninitialized(ty));
            }
            I::Dup => match self.stack.last() {
                // Only two shapes are modelled — the object-creation `new; dup; …; invokespecial`
                // and the array-initializer `newarray/anewarray; (dup; <index>; <value>;
                // Xastore)*` — since a `dup` of any real value would duplicate a side effect;
                // everything else bails.
                Some(Expr::Uninitialized(ty)) => {
                    let ty = ty.clone();
                    self.stack.push(Expr::Uninitialized(ty));
                }
                Some(Expr::PendingArray { .. }) => self.stack.push(Expr::PendingArrayDup),
                _ => return None,
            },
            I::CheckCast(i) => {
                let ty = JavaType::render_field_type(&self.class_ref_type(*i)?);
                self.cast(ty)?;
            }
            I::ArrayLength => {
                let array = self.pop()?;
                self.stack.push(Expr::ArrayLength(Box::new(array)));
            }

            // Arrays: element reads / writes and creation.
            I::Iaload
            | I::Laload
            | I::Faload
            | I::Daload
            | I::Aaload
            | I::Baload
            | I::Caload
            | I::Saload => self.array_load()?,
            I::Iastore
            | I::Lastore
            | I::Fastore
            | I::Dastore
            | I::Aastore
            | I::Bastore
            | I::Castore
            | I::Sastore => self.array_store()?,
            I::NewArray(atype) => {
                let elem = BaseType::from_atype(*atype)?;
                self.new_array(elem.keyword().into(), 0)?;
            }
            I::ANewArray(i) => self.anew_array(*i)?,
            I::MultiANewArray { index, dimensions } => {
                self.multi_new_array(*index, *dimensions)?;
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

            // Discard: a call (or a discarded object creation / builder chain) whose result is
            // unused becomes an expression statement.
            I::Pop => match self.stack.last() {
                Some(Expr::Call { .. } | Expr::New { .. } | Expr::PendingBuilder(_)) => {
                    let call = self.pop()?;
                    self.stmts.push(Stmt::Expr(call));
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

    /// The type a `Class` entry points to, as a [`FieldType`]: an array class entry holds the full
    /// field descriptor (`[I`, `[Ljava/lang/String;`), any other a plain internal name (JVMS
    /// §4.4.1) — the ambiguity is resolved once here for every instruction that takes a class
    /// operand (`checkcast`, `anewarray`, `multianewarray`).
    fn class_ref_type(&self, index: u16) -> Option<FieldType> {
        let internal = self.class_ref(index)?;
        if internal.starts_with('[') {
            FieldType::parse(&internal).ok()
        } else {
            Some(FieldType::Object(internal))
        }
    }
}

/// A `lcmp`/`fcmpl`/`fcmpg`/`dcmpl`/`dcmpg` fused into the following `if<cond>` branch: the pair
/// pushes -1/0/1 and immediately tests it against 0, reading back a source-level `long`/`float`/
/// `double` comparison. The flavor records what either operand being NaN pushes (`*cmpl` -1,
/// `*cmpg` +1; `lcmp` compares totally), which decides whether a rendered operator is faithful.
#[derive(Clone, Copy)]
enum Cmp {
    /// `lcmp` — a total order, every operator is exact.
    Total,
    /// `fcmpl` / `dcmpl` — NaN pushes -1.
    NanNeg,
    /// `fcmpg` / `dcmpg` — NaN pushes +1.
    NanPos,
}

impl Cmp {
    /// Classify a comparison instruction, or `None` for any other instruction.
    const fn of(ins: &Instruction) -> Option<Self> {
        match ins {
            Instruction::Lcmp => Some(Self::Total),
            Instruction::Fcmpl | Instruction::Dcmpl => Some(Self::NanNeg),
            Instruction::Fcmpg | Instruction::Dcmpg => Some(Self::NanPos),
            _ => None,
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
            Self::Total => true,
            Self::NanNeg => !matches!(op, "<" | "<="),
            Self::NanPos => !matches!(op, ">" | ">="),
        }
    }
}

/// Recovers structured statements from a method's [`Cfg`], running each block through [`Sim`] and
/// folding forward conditional branches into `if` / `if`-`else`.
struct Structurer<'a> {
    code: &'a [Instruction],
    cfg: &'a Cfg,
    pool: &'a ConstantPool,
    bootstrap: &'a [BootstrapMethod],
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
            let (mut stmts, cond_stack, cmp) = self.run_block(b)?;
            out.append(&mut stmts);
            // Only a conditional-branch block may leave operands (its condition) on the stack; a
            // leftover on any other terminator means we mis-read the block, so bail.
            if !cond_stack.is_empty() && !matches!(self.cfg.blocks[b].term, Term::Branch { .. }) {
                return None;
            }
            match &self.cfg.blocks[b].term {
                Term::Fall(next) => {
                    let next = *next;
                    match (next.cmp(&hi), next == b + 1, next == exit) {
                        (_, _, true) => break,
                        (core::cmp::Ordering::Less, true, false) => b = next,
                        _ => return None,
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
                        // `e == taken` merely produces an empty else and renders as a plain if, so
                        // accepting the full closed range makes the actual safety bound explicit.
                        Term::Goto(e) if (taken..=hi).contains(&e) => {
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

    /// Replay one block's value-level effects, returning its statements, any operand(s) left on the
    /// stack (the condition of a conditional-branch block; empty for every other block), and the
    /// flavor of a trailing `*cmp` fused into the block's conditional branch (its two operands are
    /// then the leftover stack, for [`Self::branch_condition`] to read back).
    fn run_block(&self, b: usize) -> Option<(Vec<Stmt>, Vec<Expr>, Option<Cmp>)> {
        let mut sim = Sim {
            pool: self.pool,
            bootstrap: self.bootstrap,
            owner: &self.owner,
            is_static: self.is_static,
            locals: &self.locals,
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
        for ins in &self.code[body] {
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
                let (mut tail, cond_stack, cmp) = self.run_block(latch)?;
                body.append(&mut tail);
                let cond = Self::branch_condition(&self.code[instr], false, cmp, cond_stack)?;
                Some((Stmt::DoWhile { body, cond }, exit))
            }
            // while (top-test): the latch's `goto` is the back-edge; the header's branch exits.
            // `emit_region` below independently requires the latch's target to equal `header`.
            // Matching every `goto` here keeps that single validation authoritative.
            Term::Goto(_) => {
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
                let (head_stmts, cond_stack, cmp) = self.run_block(header)?;
                if !head_stmts.is_empty() {
                    return None;
                }
                let cond = Self::branch_condition(&self.code[instr], true, cmp, cond_stack)?;
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
    /// With `cmp` the branch tests a fused `*cmp` result against 0 and the two operands are the
    /// stack: `lhs <op> rhs` reads back the source `long`/`float`/`double` comparison.
    fn branch_condition(
        branch: &Instruction,
        negate: bool,
        cmp: Option<Cmp>,
        mut stack: Vec<Expr>,
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
            Self::compare(operator, &mut stack)?
        } else {
            match branch {
                I::IfIcmpeq(_) | I::IfAcmpeq(_) => Self::compare(op("==", "!="), &mut stack)?,
                I::IfIcmpne(_) | I::IfAcmpne(_) => Self::compare(op("!=", "=="), &mut stack)?,
                I::IfIcmplt(_) => Self::compare(op("<", ">="), &mut stack)?,
                I::IfIcmpge(_) => Self::compare(op(">=", "<"), &mut stack)?,
                I::IfIcmpgt(_) => Self::compare(op(">", "<="), &mut stack)?,
                I::IfIcmple(_) => Self::compare(op("<=", ">"), &mut stack)?,
                I::Iflt(_) | I::Ifge(_) | I::Ifgt(_) | I::Ifle(_) => {
                    let (taken, negated) = Self::zero_test(branch)?;
                    Self::compare_lit(op(taken, negated), "0", &mut stack)?
                }
                I::IfNull(_) => Self::compare_lit(op("==", "!="), "null", &mut stack)?,
                I::IfNonNull(_) => Self::compare_lit(op("!=", "=="), "null", &mut stack)?,
                // A bare `ifne` is taken when the value is truthy, `ifeq` when it is falsy;
                // negating the taken test gives the fall-through condition.
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

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::vec;

    use jals_classfile::{ClassFile, ConstantPoolEntry, Instruction as I};

    use super::*;

    fn consts() -> ClassFile {
        ClassFile::read(include_bytes!(
            "../../jals-classpath/tests/fixtures/Consts.class"
        ))
        .expect("parse Consts fixture")
    }

    fn sim<'a>(pool: &'a ConstantPool, locals: &'a BTreeMap<u16, String>) -> Sim<'a> {
        Sim {
            pool,
            bootstrap: &[],
            owner: "demo/Owner",
            is_static: true,
            locals,
            stack: Vec::new(),
            stmts: Vec::new(),
        }
    }

    fn pool_index(pool: &ConstantPool, predicate: impl Fn(&ConstantPoolEntry) -> bool) -> u16 {
        (1..1024)
            .find(|&index| pool.get(index).is_some_and(&predicate))
            .expect("matching constant-pool entry")
    }

    fn synthetic_concat_class() -> ClassFile {
        fn u16(bytes: &mut Vec<u8>, value: u16) {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        fn utf8(bytes: &mut Vec<u8>, value: &str) {
            bytes.push(1);
            u16(
                bytes,
                u16::try_from(value.len()).expect("short test constant"),
            );
            bytes.extend_from_slice(value.as_bytes());
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xCAFE_BABE_u32.to_be_bytes());
        u16(&mut bytes, 0);
        u16(&mut bytes, 61);
        u16(&mut bytes, 22);
        utf8(&mut bytes, "Test"); // 1
        bytes.extend_from_slice(&[7, 0, 1]); // 2 Class Test
        utf8(&mut bytes, "java/lang/Object"); // 3
        bytes.extend_from_slice(&[7, 0, 3]); // 4 Class Object
        utf8(&mut bytes, "java/lang/invoke/StringConcatFactory"); // 5
        bytes.extend_from_slice(&[7, 0, 5]); // 6 Class StringConcatFactory
        utf8(&mut bytes, "makeConcat"); // 7
        utf8(&mut bytes, "()V"); // 8
        bytes.extend_from_slice(&[12, 0, 7, 0, 8]); // 9 NameAndType
        bytes.extend_from_slice(&[10, 0, 6, 0, 9]); // 10 MethodRef
        bytes.extend_from_slice(&[15, 6, 0, 10]); // 11 MethodHandle
        utf8(&mut bytes, "call"); // 12
        utf8(&mut bytes, "(I)Ljava/lang/String;"); // 13
        bytes.extend_from_slice(&[12, 0, 12, 0, 13]); // 14 NameAndType
        bytes.extend_from_slice(&[18, 0, 0, 0, 14]); // 15 InvokeDynamic
        utf8(&mut bytes, "x"); // 16
        bytes.extend_from_slice(&[8, 0, 16]); // 17 String "x"
        utf8(&mut bytes, "makeConcatWithConstants"); // 18
        bytes.extend_from_slice(&[12, 0, 18, 0, 8]); // 19 NameAndType
        bytes.extend_from_slice(&[10, 0, 6, 0, 19]); // 20 MethodRef
        bytes.extend_from_slice(&[15, 6, 0, 20]); // 21 MethodHandle
        bytes.extend_from_slice(&[
            0, 0x21, // access_flags
            0, 2, // this_class
            0, 4, // super_class
            0, 0, // interfaces_count
            0, 0, // fields_count
            0, 0, // methods_count
            0, 0, // attributes_count
        ]);
        ClassFile::read(&bytes).expect("parse synthetic concat class")
    }

    // The fixtures only cover the flavor/operator pairings `javac` emits, so the full NaN
    // faithfulness table is asserted here: an ordering operator whose true side would capture
    // NaN (the flavor's sign) is inexact, everything else — and all of `lcmp` — is exact.
    #[test]
    fn cmp_exactness_table() {
        for (cmp, expected) in [
            (Cmp::Total, [true, true, true, true, true, true]),
            (Cmp::NanNeg, [true, true, false, false, true, true]),
            (Cmp::NanPos, [true, true, true, true, false, false]),
        ] {
            for (op, exact) in ["==", "!=", "<", "<=", ">", ">="].into_iter().zip(expected) {
                assert_eq!(cmp.exact(op), exact, "{op}");
            }
        }
    }

    #[test]
    fn recognizes_all_local_store_encodings() {
        for (instruction, expected) in [
            (I::Istore(7), 7),
            (I::Lstore(7), 7),
            (I::Fstore(7), 7),
            (I::Dstore(7), 7),
            (I::Astore(7), 7),
            (I::Istore0, 0),
            (I::Lstore1, 1),
            (I::Fstore2, 2),
            (I::Dstore3, 3),
            (I::Astore0, 0),
            (I::Wide(WideInstruction::Istore(300)), 300),
            (I::Wide(WideInstruction::Lstore(301)), 301),
            (I::Wide(WideInstruction::Fstore(302)), 302),
            (I::Wide(WideInstruction::Dstore(303)), 303),
            (I::Wide(WideInstruction::Astore(304)), 304),
        ] {
            assert_eq!(MethodBody::store_slot(&instruction), Some(expected));
            assert_eq!(MethodBody::stored_slot(&instruction), Some(expected));
        }
        assert_eq!(
            MethodBody::stored_slot(&I::Iinc { index: 9, value: 1 }),
            Some(9)
        );
        assert_eq!(
            MethodBody::stored_slot(&I::Wide(WideInstruction::Iinc {
                index: 400,
                value: -2,
            })),
            Some(400)
        );
        assert_eq!(MethodBody::stored_slot(&I::Nop), None);
    }

    #[test]
    fn instance_slot_zero_is_not_hoisted_as_a_local() {
        let cf = consts();
        let code = CodeAttribute {
            max_stack: 1,
            max_locals: 1,
            code: vec![I::Iconst0, I::Istore0, I::Return],
            exception_table: Vec::new(),
            attributes: Vec::new(),
        };
        let mut locals = BTreeMap::new();
        assert!(
            MethodBody::local_declarations(&code, &cf.constant_pool, false, &mut locals)
                .is_some_and(|decls| decls.is_empty())
        );
        assert!(
            MethodBody::local_declarations(&code, &cf.constant_pool, true, &mut locals).is_none()
        );
    }

    #[test]
    fn simulator_covers_constants_loads_arithmetic_and_conversions() {
        let cf = consts();
        let locals = BTreeMap::from([(7, "x".to_owned())]);
        let mut sim = sim(&cf.constant_pool, &locals);
        assert_eq!(sim.unary("-"), None);
        assert_eq!(sim.step(&I::Nop), Some(()));
        for instruction in [
            I::AconstNull,
            I::Pop,
            I::Iload(7),
            I::Pop,
            I::Iconst5,
            I::Sipush(9),
            I::Isub,
            I::Ineg,
            I::Pop,
            I::Lconst0,
            I::Lconst1,
            I::Lsub,
            I::Pop,
        ] {
            assert_eq!(sim.step(&instruction), Some(()), "{instruction:?}");
        }

        let cases = [
            (I::Irem, "%"),
            (I::Ishl, "<<"),
            (I::Ishr, ">>"),
            (I::Iushr, ">>>"),
            (I::Iand, "&"),
            (I::Ior, "|"),
            (I::Ixor, "^"),
        ];
        for (instruction, operator) in cases {
            sim.stack.extend([Expr::lit("8"), Expr::lit("2")]);
            assert_eq!(sim.step(&instruction), Some(()));
            assert_eq!(
                sim.pop().map(|e| e.render()),
                Some(format!("8 {operator} 2"))
            );
        }

        for (instruction, expected) in [
            (I::I2l, "(long) 1"),
            (I::I2f, "(float) 1"),
            (I::I2d, "(double) 1"),
            (I::L2i, "(int) 1"),
            (I::I2b, "(byte) 1"),
            (I::I2c, "(char) 1"),
            (I::I2s, "(short) 1"),
        ] {
            sim.stack.push(Expr::lit("1"));
            assert_eq!(sim.step(&instruction), Some(()));
            assert_eq!(sim.pop().map(|e| e.render()).as_deref(), Some(expected));
        }
    }

    #[test]
    fn iinc_handles_negative_zero_positive_and_wide_forms() {
        let cf = consts();
        let locals = BTreeMap::from([(7, "x".to_owned()), (400, "wide".to_owned())]);
        let mut sim = sim(&cf.constant_pool, &locals);
        for instruction in [
            I::Iinc {
                index: 7,
                value: -2,
            },
            I::Iinc { index: 7, value: 0 },
            I::Iinc { index: 7, value: 3 },
            I::Wide(WideInstruction::Iinc {
                index: 400,
                value: -4,
            }),
        ] {
            assert_eq!(sim.step(&instruction), Some(()));
        }
        assert_eq!(
            Stmt::render_block(&sim.stmts),
            ["x = x - 2;", "x = x + 0;", "x = x + 3;", "wide = wide - 4;"]
        );
        assert_eq!(sim.iinc(999, 1), None);
    }

    #[test]
    fn simulator_executes_generic_numbered_and_wide_stores() {
        let cf = consts();
        let locals = BTreeMap::from([
            (0, "zero".to_owned()),
            (7, "seven".to_owned()),
            (300, "wide".to_owned()),
        ]);
        let mut sim = sim(&cf.constant_pool, &locals);
        for instruction in [
            I::Iconst1,
            I::Istore(7),
            I::Iconst2,
            I::Istore0,
            I::Iconst3,
            I::Wide(WideInstruction::Istore(300)),
        ] {
            assert_eq!(sim.step(&instruction), Some(()));
        }
        assert_eq!(
            Stmt::render_block(&sim.stmts),
            ["seven = 1;", "zero = 2;", "wide = 3;"]
        );
    }

    #[test]
    fn resolves_each_ldc_constant_kind() {
        let cf = consts();
        let locals = BTreeMap::new();
        let sim = sim(&cf.constant_pool, &locals);
        for (predicate, expected) in [
            (
                (|entry: &ConstantPoolEntry| matches!(entry, ConstantPoolEntry::Integer(_)))
                    as fn(&ConstantPoolEntry) -> bool,
                "42",
            ),
            (
                (|entry: &ConstantPoolEntry| matches!(entry, ConstantPoolEntry::Long(_)))
                    as fn(&ConstantPoolEntry) -> bool,
                "9000000000L",
            ),
            (
                (|entry: &ConstantPoolEntry| matches!(entry, ConstantPoolEntry::Float(_)))
                    as fn(&ConstantPoolEntry) -> bool,
                "0.25f",
            ),
            (
                (|entry: &ConstantPoolEntry| matches!(entry, ConstantPoolEntry::Double(_)))
                    as fn(&ConstantPoolEntry) -> bool,
                "1.5d",
            ),
            (
                (|entry: &ConstantPoolEntry| matches!(entry, ConstantPoolEntry::String { .. }))
                    as fn(&ConstantPoolEntry) -> bool,
                "\"jals\"",
            ),
            (
                (|entry: &ConstantPoolEntry| matches!(entry, ConstantPoolEntry::Class { .. }))
                    as fn(&ConstantPoolEntry) -> bool,
                "java.lang.Object.class",
            ),
        ] {
            let index = pool_index(&cf.constant_pool, predicate);
            assert_eq!(
                sim.constant(index).map(|e| e.render()).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn static_field_and_method_instructions_are_observable() {
        let cf = consts();
        let locals = BTreeMap::new();
        let field_ref = pool_index(&cf.constant_pool, |entry| {
            matches!(entry, ConstantPoolEntry::FieldRef { .. })
        });
        let method_ref = pool_index(&cf.constant_pool, |entry| {
            let ConstantPoolEntry::MethodRef {
                name_and_type_index,
                ..
            } = entry
            else {
                return false;
            };
            let Some(ConstantPoolEntry::NameAndType {
                descriptor_index, ..
            }) = cf.constant_pool.get(*name_and_type_index)
            else {
                return false;
            };
            cf.constant_pool.utf8(*descriptor_index).as_deref() == Some("()V")
        });

        let mut sim = sim(&cf.constant_pool, &locals);
        assert_eq!(sim.step(&I::GetStatic(field_ref)), Some(()));
        assert!(sim.pop().is_some_and(|e| e.render().contains('.')));
        sim.stack.push(Expr::lit("1"));
        assert_eq!(sim.step(&I::PutStatic(field_ref)), Some(()));
        assert_eq!(sim.step(&I::InvokeStatic(method_ref)), Some(()));
        assert_eq!(sim.stmts.len(), 2);
    }

    #[test]
    fn builder_guards_and_coercions_cover_boundary_cases() {
        let cf = consts();
        let locals = BTreeMap::new();
        let mut sim = sim(&cf.constant_pool, &locals);
        let no_args = MethodDescriptor::parse("()Ljava/lang/String;").expect("descriptor");
        sim.stack.push(Expr::PendingBuilder(Vec::new()));
        assert_eq!(sim.fold_builder("toString", &no_args), Some(false));
        let Some(Expr::PendingBuilder(parts)) = sim.stack.last_mut() else {
            panic!("builder")
        };
        parts.push(ConcatPart {
            expr: Expr::lit("1"),
            stringy: false,
        });
        assert_eq!(sim.fold_builder("toString", &no_args), Some(true));
        assert_eq!(sim.pop().map(|e| e.render()).as_deref(), Some("\"\" + 1"));

        let one_arg = MethodDescriptor::parse("(I)Ljava/lang/String;").expect("descriptor");
        sim.stack.push(Expr::PendingBuilder(vec![ConcatPart {
            expr: Expr::lit("1"),
            stringy: false,
        }]));
        sim.stack.push(Expr::lit("2"));
        assert_eq!(sim.fold_builder("toString", &one_arg), Some(false));
        assert_eq!(sim.stack.len(), 2);
        sim.stack.clear();
        sim.stack.push(Expr::PendingBuilder(vec![ConcatPart {
            expr: Expr::lit("1"),
            stringy: false,
        }]));
        assert_eq!(sim.fold_builder("toString", &one_arg), Some(false));
        assert!(matches!(sim.stack.as_slice(), [Expr::PendingBuilder(_)]));
        let two_args =
            MethodDescriptor::parse("(II)Ljava/lang/StringBuilder;").expect("descriptor");
        assert_eq!(sim.fold_builder("append", &no_args), Some(false));
        sim.stack.push(Expr::lit("2"));
        assert_eq!(sim.fold_builder("append", &two_args), Some(false));
        assert_eq!(sim.stack.len(), 2);

        assert!(Sim::concat_safe(&FieldType::Base(BaseType::Int)));
        assert!(Sim::concat_safe(&FieldType::Object(
            "java/lang/CharSequence".to_owned()
        )));
        assert!(!Sim::concat_safe(&FieldType::Object(
            "pkg/Other".to_owned()
        )));
        assert!(!Sim::concat_safe(&FieldType::Array(Box::new(
            FieldType::Base(BaseType::Char)
        ))));

        for (value, ty, expected) in [
            ("0", BaseType::Boolean, Some("false")),
            ("1", BaseType::Boolean, Some("true")),
            ("2", BaseType::Boolean, None),
            ("65", BaseType::Char, Some("'A'")),
            ("55296", BaseType::Char, Some("(char) 55296")),
            ("65535", BaseType::Char, Some("'\u{ffff}'")),
            ("65536", BaseType::Char, None),
        ] {
            assert_eq!(
                Sim::coerce(Expr::lit(value), &FieldType::Base(ty))
                    .map(|e| e.render())
                    .as_deref(),
                expected
            );
        }
    }

    #[test]
    fn concat_bootstrap_guards_and_recipe_consumption_are_exact() {
        let cf = synthetic_concat_class();
        let locals = BTreeMap::new();
        let empty = BootstrapMethod {
            bootstrap_method_ref: 11,
            bootstrap_arguments: Vec::new(),
        };
        let nonempty = BootstrapMethod {
            bootstrap_method_ref: 11,
            bootstrap_arguments: vec![17],
        };
        let sim = sim(&cf.constant_pool, &locals);
        assert_eq!(
            sim.concat_shape(&empty, 2)
                .map(|(recipe, constants)| (recipe, constants.len())),
            Some(("\u{1}\u{1}".to_owned(), 0))
        );
        assert!(sim.concat_shape(&nonempty, 2).is_none());

        let mismatched = BootstrapMethod {
            bootstrap_method_ref: 21,
            bootstrap_arguments: vec![17],
        };
        let bootstraps = [mismatched];
        let mut sim = Sim {
            pool: &cf.constant_pool,
            bootstrap: &bootstraps,
            owner: "Test",
            is_static: true,
            locals: &locals,
            stack: vec![Expr::lit("1")],
            stmts: Vec::new(),
        };
        assert_eq!(sim.invoke_dynamic(15), None);
    }

    #[test]
    fn multi_array_and_initializer_bounds_bail() {
        let cf = ClassFile::read(include_bytes!(
            "../../jals-classpath/tests/fixtures/Arrays.class"
        ))
        .expect("parse Arrays fixture");
        let locals = BTreeMap::new();
        let array_class = pool_index(&cf.constant_pool, |entry| {
            let ConstantPoolEntry::Class { name_index } = entry else {
                return false;
            };
            cf.constant_pool
                .utf8(*name_index)
                .is_some_and(|name| name.starts_with("[["))
        });
        let mut sim = sim(&cf.constant_pool, &locals);
        sim.stack
            .extend([Expr::lit("1"), Expr::lit("2"), Expr::lit("3")]);
        assert_eq!(sim.multi_new_array(array_class, 0), None);
        assert_eq!(sim.multi_new_array(array_class, 3), None);

        sim.stack.push(Expr::PendingArray {
            elem: "int".to_owned(),
            empty_dims: 0,
            len: 1,
            elems: Vec::new(),
        });
        assert_eq!(sim.push_elem(&Expr::lit("1"), Expr::lit("7")), None);
        assert_eq!(sim.push_elem(&Expr::lit("0"), Expr::lit("7")), Some(()));
        assert_eq!(sim.push_elem(&Expr::lit("1"), Expr::lit("8")), None);
    }

    #[test]
    fn every_reference_and_integer_branch_operator_is_rendered() {
        let pairs = [
            (I::IfIcmpeq(0), "a == b"),
            (I::IfAcmpeq(0), "a == b"),
            (I::IfIcmpne(0), "a != b"),
            (I::IfAcmpne(0), "a != b"),
            (I::IfIcmpgt(0), "a > b"),
        ];
        for (branch, expected) in pairs {
            let expr = Structurer::branch_condition(
                &branch,
                false,
                None,
                vec![Expr::lit("a"), Expr::lit("b")],
            )
            .expect("condition");
            assert_eq!(expr.render(), expected);
        }
        for (branch, expected) in [(I::IfNull(0), "a == null"), (I::IfNonNull(0), "a != null")] {
            let expr = Structurer::branch_condition(&branch, false, None, vec![Expr::lit("a")])
                .expect("condition");
            assert_eq!(expr.render(), expected);
        }
    }

    fn structurer<'a>(
        code: &'a [Instruction],
        cfg: &'a Cfg,
        pool: &'a ConstantPool,
    ) -> Structurer<'a> {
        Structurer {
            code,
            cfg,
            pool,
            bootstrap: &[],
            owner: "demo/Owner".to_owned(),
            is_static: true,
            locals: BTreeMap::new(),
        }
    }

    #[test]
    fn emit_region_accepts_boundaries_and_rejects_each_bad_branch_edge() {
        let cf = consts();
        let code = [I::Iconst1, I::Ifeq(0), I::Return];
        let valid = Cfg {
            blocks: vec![
                crate::cfg::Block {
                    start: 0,
                    end: 2,
                    term: Term::Branch {
                        instr: 1,
                        taken: 2,
                        fallthrough: 1,
                    },
                },
                crate::cfg::Block {
                    start: 2,
                    end: 3,
                    term: Term::Ret,
                },
            ],
        };
        let mut visited = vec![false; 2];
        let stmts = structurer(&code, &valid, &cf.constant_pool)
            .emit_region(0, 2, 2, &mut visited)
            .expect("taken == hi is a valid no-else join");
        assert_eq!(Stmt::render_block(&stmts), ["if (1) {", "    return;", "}"]);

        let leftover = Cfg {
            blocks: vec![crate::cfg::Block {
                start: 0,
                end: 1,
                term: Term::Ret,
            }],
        };
        let mut visited = vec![false];
        assert!(
            structurer(&[I::Iconst1], &leftover, &cf.constant_pool)
                .emit_region(0, 1, 1, &mut visited)
                .is_none()
        );

        for (taken, fallthrough) in [(2, 0), (3, 1)] {
            let malformed = Cfg {
                blocks: vec![
                    crate::cfg::Block {
                        start: 0,
                        end: 2,
                        term: Term::Branch {
                            instr: 1,
                            taken,
                            fallthrough,
                        },
                    },
                    crate::cfg::Block {
                        start: 2,
                        end: 3,
                        term: Term::Ret,
                    },
                ],
            };
            let mut visited = vec![false; 2];
            assert!(
                structurer(&code, &malformed, &cf.constant_pool)
                    .emit_region(0, 2, 2, &mut visited)
                    .is_none(),
                "taken={taken}, fallthrough={fallthrough}"
            );
        }

        let backward_code = [I::Nop, I::Iconst1, I::Ifeq(0), I::Return];
        let backward = Cfg {
            blocks: vec![
                crate::cfg::Block {
                    start: 0,
                    end: 1,
                    term: Term::Fall(1),
                },
                crate::cfg::Block {
                    start: 1,
                    end: 3,
                    term: Term::Branch {
                        instr: 2,
                        taken: 0,
                        fallthrough: 2,
                    },
                },
                crate::cfg::Block {
                    start: 3,
                    end: 4,
                    term: Term::Ret,
                },
            ],
        };
        let mut visited = vec![false; 3];
        assert!(
            structurer(&backward_code, &backward, &cf.constant_pool)
                .emit_region(1, 3, 3, &mut visited)
                .is_none()
        );
    }

    #[test]
    fn emit_region_recovers_an_else_whose_join_is_region_end() {
        let cf = consts();
        let code = [I::Iconst1, I::Ifeq(0), I::Goto(0), I::Return];
        let cfg = Cfg {
            blocks: vec![
                crate::cfg::Block {
                    start: 0,
                    end: 2,
                    term: Term::Branch {
                        instr: 1,
                        taken: 2,
                        fallthrough: 1,
                    },
                },
                crate::cfg::Block {
                    start: 2,
                    end: 3,
                    term: Term::Goto(3),
                },
                crate::cfg::Block {
                    start: 3,
                    end: 4,
                    term: Term::Ret,
                },
            ],
        };
        let mut visited = vec![false; 3];
        let stmts = structurer(&code, &cfg, &cf.constant_pool)
            .emit_region(0, 3, 3, &mut visited)
            .expect("valid if-else");
        assert_eq!(
            Stmt::render_block(&stmts),
            ["if (1) {", "} else {", "    return;", "}"]
        );
    }

    #[test]
    fn emit_region_rejects_skipped_fallthrough_and_else_join_past_region() {
        let cf = consts();
        let skipped = Cfg {
            blocks: vec![
                crate::cfg::Block {
                    start: 0,
                    end: 1,
                    term: Term::Fall(2),
                },
                crate::cfg::Block {
                    start: 1,
                    end: 2,
                    term: Term::Ret,
                },
                crate::cfg::Block {
                    start: 2,
                    end: 3,
                    term: Term::Ret,
                },
            ],
        };
        let mut visited = vec![false; 3];
        assert!(
            structurer(&[I::Nop, I::Return, I::Return], &skipped, &cf.constant_pool)
                .emit_region(0, 3, 3, &mut visited)
                .is_none()
        );

        let code = [I::Iconst1, I::Ifeq(0), I::Goto(0), I::Return];
        let past_end = Cfg {
            blocks: vec![
                crate::cfg::Block {
                    start: 0,
                    end: 2,
                    term: Term::Branch {
                        instr: 1,
                        taken: 2,
                        fallthrough: 1,
                    },
                },
                crate::cfg::Block {
                    start: 2,
                    end: 3,
                    term: Term::Goto(4),
                },
                crate::cfg::Block {
                    start: 3,
                    end: 4,
                    term: Term::Ret,
                },
                crate::cfg::Block {
                    start: 3,
                    end: 4,
                    term: Term::Ret,
                },
            ],
        };
        let mut visited = vec![false; 4];
        assert!(
            structurer(&code, &past_end, &cf.constant_pool)
                .emit_region(0, 3, 3, &mut visited)
                .is_none()
        );
    }

    #[test]
    fn empty_branch_body_is_not_treated_as_a_comparison() {
        let cf = consts();
        let code = [I::Ifeq(0)];
        let cfg = Cfg {
            blocks: vec![crate::cfg::Block {
                start: 0,
                end: 1,
                term: Term::Branch {
                    instr: 0,
                    taken: 0,
                    fallthrough: 0,
                },
            }],
        };
        let result = structurer(&code, &cfg, &cf.constant_pool)
            .run_block(0)
            .expect("empty branch body");
        assert!(result.0.is_empty());
        assert!(result.1.is_empty());
        assert!(result.2.is_none());
    }

    #[test]
    fn do_while_validates_taken_edge_and_exit_boundaries() {
        let cf = consts();
        let code = [I::Iconst1, I::Ifne(0)];
        let make_cfg = |taken, fallthrough| Cfg {
            blocks: vec![crate::cfg::Block {
                start: 0,
                end: 2,
                term: Term::Branch {
                    instr: 1,
                    taken,
                    fallthrough,
                },
            }],
        };

        let valid = make_cfg(0, 1);
        let mut visited = vec![false];
        assert!(
            structurer(&code, &valid, &cf.constant_pool)
                .structure_loop(0, 0, 1, &mut visited)
                .is_some()
        );

        for (taken, fallthrough, hi) in [(1, 1, 1), (0, 0, 1), (0, 1, 0)] {
            let cfg = make_cfg(taken, fallthrough);
            let mut visited = vec![false];
            assert!(
                structurer(&code, &cfg, &cf.constant_pool)
                    .structure_loop(0, 0, hi, &mut visited)
                    .is_none(),
                "taken={taken}, fallthrough={fallthrough}, hi={hi}"
            );
        }
    }

    #[test]
    fn while_validates_body_start_and_exit_boundaries() {
        let cf = consts();
        let code = [I::Iconst1, I::Ifeq(0), I::Goto(0)];
        let make_cfg = |taken, fallthrough| Cfg {
            blocks: vec![
                crate::cfg::Block {
                    start: 0,
                    end: 2,
                    term: Term::Branch {
                        instr: 1,
                        taken,
                        fallthrough,
                    },
                },
                crate::cfg::Block {
                    start: 2,
                    end: 3,
                    term: Term::Goto(0),
                },
            ],
        };
        let valid = make_cfg(2, 1);
        let mut visited = vec![false; 2];
        assert!(
            structurer(&code, &valid, &cf.constant_pool)
                .structure_loop(0, 1, 2, &mut visited)
                .is_some()
        );

        for (taken, fallthrough, hi) in [(2, 2, 2), (1, 1, 2), (3, 1, 2)] {
            let cfg = make_cfg(taken, fallthrough);
            let mut visited = vec![false; 2];
            assert!(
                structurer(&code, &cfg, &cf.constant_pool)
                    .structure_loop(0, 1, hi, &mut visited)
                    .is_none(),
                "taken={taken}, fallthrough={fallthrough}, hi={hi}"
            );
        }
    }
}
