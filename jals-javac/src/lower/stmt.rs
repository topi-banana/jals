//! Statement lowering: every form leaves the operand stack as it found it.
//!
//! # A loop is three labels, not one
//!
//! The JVM has no loop, only jumps, and the three positions a Java loop needs are all distinct: where
//! the condition is tested, where a `continue` lands, and where a `break` lands. In a `while` the
//! first two coincide; in a `for` they do not, because `continue` has to run the update section
//! (JLS §14.14.1). Sending a `continue` to the condition instead is an infinite loop that only
//! appears when a body actually contains one, which is why the two labels are separate here even
//! where they would fold.
//!
//! # A label names a loop, not a statement
//!
//! `break l` and `continue l` are resolved against a stack of enclosing constructs, and a
//! `LabeledStmt` does not lower to anything itself — it hands its name down to whatever it wraps. All
//! of `a: b: for (…)`'s labels name the same loop, so they are collected rather than nested.

use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

use jals_hir::Ty;
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::desc::Descriptor;
use crate::facts::Facts;
use crate::jvm::{Branch, Compare, Label};
use crate::lower::emit::Cleanup;
use crate::lower::expr::Expr;
use crate::lower::slots::Slots;
use crate::lower::{Context, Emit, LowerError, Result};

/// The catch-all a `synchronized` block and a `finally` clause both need.
const THROWABLE: &str = "java/lang/Throwable";

/// What an `assert` throws.
const ASSERTION_ERROR: &str = "java/lang/AssertionError";

/// The synthetic field that makes an `assert` a no-op unless the JVM was started with `-ea`.
///
/// javac's own name for it, and the name matters: a class compiled by one and read by the other has
/// to agree, and a debugger or a decompiler recognises it.
pub(crate) const ASSERTIONS_DISABLED: &str = "$assertionsDisabled";

/// Statement lowering.
pub(crate) struct Stmt;

impl Stmt {
    /// Emit every statement in `block`.
    pub(crate) fn block(
        block: &ast::Block,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // A *local* type declaration is a child of the block that is not a statement, so iterating
        // `stmts()` walks straight past it — and a class that vanished would be a `NoClassDefFoundError`
        // at the first use, which is exactly the failure a compiler is in a position to report.
        // A local type declaration is not a statement to emit: `Compile::file` compiles it as its own
        // class file, the same way it does every other declaration in the file.
        for statement in block.stmts() {
            Self::lower(&statement, context, emit)?;
        }
        Ok(())
    }

    /// Emit one statement.
    pub(crate) fn lower(
        statement: &ast::Stmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        Self::labelled(statement, Vec::new(), context, emit)
    }

    /// Emit one statement, which `labels` name.
    ///
    /// A `LabeledStmt` emits nothing of its own: a label is a *name for a jump target*, and which
    /// target depends on what it wraps. So the names are passed down, and every construct that can
    /// be left registers them itself.
    fn labelled(
        statement: &ast::Stmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        match statement {
            ast::Stmt::Labeled(outer) => {
                // `a: b: for (…)` — both names reach the loop, because `break a` and `break b` leave
                // the same one. Nesting a scope per label would make the outer one a separate target
                // at the same offset for no gain.
                let mut labels = labels;
                labels.extend(outer.label());
                let inner = outer
                    .stmt()
                    .ok_or(LowerError::Unsupported("a label with no statement"))?;
                Self::labelled(&inner, labels, context, emit)
            }
            ast::Stmt::While(statement) => Self::while_loop(statement, labels, context, emit),
            ast::Stmt::DoWhile(statement) => Self::do_while(statement, labels, context, emit),
            ast::Stmt::For(statement) => Self::for_loop(statement, labels, context, emit),
            ast::Stmt::ForEach(statement) => Self::for_each(statement, labels, context, emit),
            // Anything else a label names is a `break` target and nothing more: `l: { … break l; }`
            // leaves the block, and there is no loop for a `continue` to reach.
            other if !labels.is_empty() => Self::labelled_block(other, labels, context, emit),
            ast::Stmt::Block(block) => Self::block(block, context, emit),
            ast::Stmt::Empty(_) => Ok(()),
            ast::Stmt::LocalVar(declaration) => Self::local(declaration, context, emit),
            ast::Stmt::Expr(expression) => Self::expression(expression, context, emit),
            ast::Stmt::Return(statement) => Self::ret(statement, context, emit),
            ast::Stmt::If(statement) => Self::conditional(statement, context, emit),
            ast::Stmt::Break(statement) => Self::leave(statement.label(), true, context, emit),
            ast::Stmt::Continue(statement) => Self::leave(statement.label(), false, context, emit),
            ast::Stmt::Throw(statement) => Self::throw(statement, context, emit),
            ast::Stmt::Synchronized(statement) => Self::synchronized(statement, context, emit),
            ast::Stmt::Try(statement) => Self::try_catch(statement, context, emit),
            ast::Stmt::Assert(statement) => Self::assert(statement, context, emit),
            ast::Stmt::Switch(statement) => {
                crate::lower::switch::Switch::statement(statement, labels, context, emit)
            }
            // Every `Stmt` variant is covered, so there is no catch-all left to write. What a statement
            // still cannot reach it reports from inside — a `case` label with no constant value, a
            // resource with no `close()` — rather than by not being handled at all.
            ast::Stmt::Yield(statement) => Self::yield_value(statement, context, emit),
        }
    }

    /// `yield v;` — a `switch` expression's arm handing back its value.
    fn yield_value(
        statement: &ast::YieldStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let (done, result, depth) = emit.yield_target()?;
        let value = statement
            .expr()
            .ok_or(LowerError::Unsupported("a `yield` with no value"))?;
        Expr::lower_as(&value, &result, context, emit)?;
        // Leaving the arm runs whatever `finally` blocks it sits inside, and those cannot be allowed
        // to see the value on the stack — so it goes into a slot, exactly as a `return`'s does.
        let crossed = emit.crossed(depth);
        if !crossed.is_empty() {
            let ty = emit
                .asm
                .stack_top()
                .ok_or(LowerError::Unsupported("a `yield` that produced no value"))?;
            let width = u16::from(matches!(
                ty,
                jals_classfile::VerificationType::Long | jals_classfile::VerificationType::Double
            ));
            let held = emit.slots.declare_temporary(width + 1);
            emit.asm.store(held)?;
            Self::run_guards(&crossed, context, emit)?;
            if !emit.asm.reachable() {
                return Ok(());
            }
            emit.asm.load(held)?;
        }
        Ok(emit.asm.branch(Branch::Always, done)?)
    }

    /// `throw e;`
    fn throw(
        statement: &ast::ThrowStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let value = statement
            .expr()
            .ok_or(LowerError::Unsupported("a `throw` with nothing to throw"))?;
        Expr::lower(&value, context, emit)?;
        Ok(emit.asm.throw()?)
    }

    /// One copy of what a guarded region runs on the way out.
    fn cleanup(cleanup: &Cleanup, context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        match cleanup {
            Cleanup::Finally(block) => Self::block(block, context, emit),
            Cleanup::Unlock(slot) => {
                emit.asm.load(*slot)?;
                Ok(emit.asm.monitor_exit()?)
            }
            Cleanup::Close {
                slot,
                owner,
                interface,
            } => Self::close(*slot, owner, *interface, emit),
        }
    }

    /// `if (r != null) r.close();` — a resource is only closed if it was actually acquired.
    fn close(slot: u16, owner: &str, interface: bool, emit: &mut Emit<'_, '_>) -> Result<()> {
        let skip = emit.asm.label();
        emit.asm.load(slot)?;
        emit.asm.branch(Branch::RefNull(true), skip)?;
        emit.asm.load(slot)?;
        if interface {
            emit.asm.invoke_interface(owner, "close", "()V")?;
        } else {
            emit.asm.invoke_virtual(owner, "close", "()V")?;
        }
        Ok(emit.asm.bind(skip)?)
    }

    /// `synchronized (lock) { … }`.
    ///
    /// The monitor has to be released however the block ends, which is what the catch-all handler is
    /// for — and the lock expression has to be held in a slot rather than re-evaluated, because the
    /// handler needs the *same* object the `monitorenter` took.
    fn synchronized(
        statement: &ast::SynchronizedStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let lock = statement
            .lock()
            .ok_or(LowerError::Unsupported("a `synchronized` with no lock"))?;
        let body = statement
            .body()
            .ok_or(LowerError::Unsupported("a `synchronized` with no body"))?;

        let held = emit.slots.declare_temporary(1);
        Expr::lower(&lock, context, emit)?;
        emit.asm.store(held)?;
        emit.asm.load(held)?;
        emit.asm.monitor_enter()?;

        let start = emit.asm.label();
        let handler = emit.asm.label();
        let after = emit.asm.label();

        emit.asm.bind(start)?;
        // A `return` or a `break` out of the body has to release the monitor too, which is the same
        // problem a `finally` has — so it is the same machinery. Skipping it produced a method the JVM
        // refuses to leave, with `IllegalMonitorStateException` at run time.
        emit.guard(Cleanup::Unlock(held), start);
        Self::block(&body, context, emit)?;
        let end = emit.asm.label();
        emit.asm.mark(end)?;
        let ranges = emit.unguard(end);
        if emit.asm.reachable() {
            emit.asm.load(held)?;
            emit.asm.monitor_exit()?;
            emit.asm.branch(Branch::Always, after)?;
        }

        emit.asm.bind_handler(handler, start, THROWABLE)?;
        for &(from, to) in &ranges {
            emit.asm.protect(from, to, handler, None)?;
        }
        let thrown = emit.slots.declare_temporary(1);
        emit.asm.store(thrown)?;
        emit.asm.load(held)?;
        emit.asm.monitor_exit()?;
        emit.asm.load(thrown)?;
        emit.asm.throw()?;

        Self::join(after, emit)
    }

    /// `try { … } catch (E e) { … } …`.
    ///
    /// The handlers are recorded in source order, because the JVM takes the *first* entry whose range
    /// covers the throwing instruction and whose type matches — so a `catch (Exception)` written before
    /// a `catch (IOException)` would swallow it, exactly as the source says.
    fn try_catch(
        statement: &ast::TryStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let body = statement
            .block()
            .ok_or(LowerError::Unsupported("a `try` with no block"))?;
        let resources: Vec<ast::Resource> = statement
            .resources()
            .into_iter()
            .flat_map(|list| list.resources())
            .collect();
        let clauses: Vec<ast::CatchClause> = statement.catches().collect();
        let cleanup = statement
            .finally()
            .and_then(|clause| clause.syntax().children().find_map(ast::Block::cast));
        if clauses.is_empty() && cleanup.is_none() && resources.is_empty() {
            return Err(LowerError::Unsupported("a `try` with no `catch`"));
        }

        let after = emit.asm.label();
        // The clause handlers exist before any body is emitted, because a `protect` entry has to be
        // recorded in *source* order: the JVM takes the first whose range covers the throw and whose
        // type matches, so a `catch (Exception)` written first swallows what follows it — exactly as
        // the source says.
        let handlers: Vec<Label> = clauses.iter().map(|_| emit.asm.label()).collect();
        let mut protected: Vec<(Label, Label)> = Vec::new();

        // --- the `try` block ---
        let start = emit.asm.label();
        emit.asm.bind(start)?;
        if let Some(cleanup) = &cleanup {
            emit.guard(Cleanup::Finally(cleanup.clone()), start);
        }
        // A `try` with both resources and a `catch` / `finally` is the nesting JLS §14.20.3.2 spells
        // out: the resource-closing `try` sits *inside*, so a `catch` sees an exception `close()` threw.
        Self::resource_chain(&resources, &body, context, emit)?;
        let end = emit.asm.label();
        emit.asm.mark(end)?;
        let body_ranges = if cleanup.is_some() {
            emit.unguard(end)
        } else {
            alloc::vec![(start, end)]
        };
        protected.extend(body_ranges.iter().copied());
        if emit.asm.reachable() {
            // Falling off the end of the `try` runs the cleanup like any other exit.
            if let Some(cleanup) = &cleanup {
                Self::block(cleanup, context, emit)?;
            }
            if emit.asm.reachable() {
                emit.asm.branch(Branch::Always, after)?;
            }
        }

        for (clause, &handler) in clauses.iter().zip(&handlers) {
            for ty in clause.types() {
                let caught = context.ty_of_type(&ty)?;
                let entry = Descriptor::class_entry(&caught, context.index)?;
                for &(from, to) in &body_ranges {
                    emit.asm.protect(from, to, handler, Some(&entry))?;
                }
            }
        }

        // --- the `catch` clauses ---
        for (clause, &handler) in clauses.iter().zip(&handlers) {
            // A multi-catch binds its variable at the nearest type every arm shares, because that is
            // all the source may call on it. Naming one arm would be a lie for the others, and naming
            // `Throwable` would refuse a legal call on a common supertype below it.
            let caught = Self::caught_type(clause, context)?;
            emit.asm.bind_handler(
                handler,
                start,
                &Descriptor::class_entry(&caught, context.index)?,
            )?;
            if let Some(cleanup) = &cleanup {
                emit.guard(Cleanup::Finally(cleanup.clone()), handler);
            }
            match clause.binding() {
                Some(name) => {
                    let id = context
                        .facts()
                        .def_at_token(&name)
                        .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
                    let slot = emit.slots.declare(id, 1);
                    emit.asm.store(slot)?;
                }
                // An unnamed `_` binding still has to come off the stack.
                None => emit.asm.pop()?,
            }
            if let Some(block) = clause.block() {
                Self::block(&block, context, emit)?;
            }
            let clause_end = emit.asm.label();
            emit.asm.mark(clause_end)?;
            if cleanup.is_some() {
                // A `catch` body is guarded by the `finally` too, so its ranges join the set the
                // catch-all handler protects.
                protected.extend(emit.unguard(clause_end));
            }
            if emit.asm.reachable() {
                if let Some(cleanup) = &cleanup {
                    Self::block(cleanup, context, emit)?;
                }
                if emit.asm.reachable() {
                    emit.asm.branch(Branch::Always, after)?;
                }
            }
        }

        // --- the `finally`'s catch-all ---
        if let Some(cleanup) = &cleanup {
            let catch_all = emit.asm.label();
            emit.asm.bind_handler(catch_all, start, THROWABLE)?;
            for &(from, to) in &protected {
                emit.asm.protect(from, to, catch_all, None)?;
            }
            let thrown = emit.slots.declare_temporary(1);
            emit.asm.store(thrown)?;
            Self::block(cleanup, context, emit)?;
            if emit.asm.reachable() {
                emit.asm.load(thrown)?;
                emit.asm.throw()?;
            }
        }

        Self::join(after, emit)
    }

    /// The `try`'s block, wrapped in one closing region per declared resource.
    ///
    /// JLS §14.20.3 defines a multi-resource `try` as nested single-resource ones, so this recurses —
    /// which is also what makes the *last* resource declared the first one closed.
    fn resource_chain(
        resources: &[ast::Resource],
        body: &ast::Block,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let Some((first, rest)) = resources.split_first() else {
            return Self::block(body, context, emit);
        };
        let (slot, owner, interface) = Self::acquire(first, context, emit)?;
        let close = Cleanup::Close {
            slot,
            owner: owner.clone(),
            interface,
        };

        let start = emit.asm.label();
        let handler = emit.asm.label();
        let after = emit.asm.label();
        emit.asm.bind(start)?;
        emit.guard(close, start);
        Self::resource_chain(rest, body, context, emit)?;
        let end = emit.asm.label();
        emit.asm.mark(end)?;
        let ranges = emit.unguard(end);
        if emit.asm.reachable() {
            Self::close(slot, &owner, interface, emit)?;
            emit.asm.branch(Branch::Always, after)?;
        }

        // On the exceptional path a failing `close()` is *suppressed* rather than replacing the
        // exception the body threw (JLS §14.20.3.1). Losing the body's exception is the whole reason
        // try-with-resources exists, so this is not an optional refinement.
        emit.asm.bind_handler(handler, start, THROWABLE)?;
        for &(from, to) in &ranges {
            emit.asm.protect(from, to, handler, None)?;
        }
        let primary = emit.slots.declare_temporary(1);
        emit.asm.store(primary)?;
        Self::close_suppressing(slot, &owner, interface, primary, emit)?;
        emit.asm.load(primary)?;
        emit.asm.throw()?;

        Self::join(after, emit)
    }

    /// Evaluate a resource into a slot, and work out what its `close()` is called on.
    fn acquire(
        resource: &ast::Resource,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<(u16, String, bool)> {
        let value = resource
            .syntax()
            .children()
            .find_map(ast::Expr::cast)
            .ok_or(LowerError::Unsupported("a resource with no value"))?;
        let ty = match resource.syntax().children().find_map(ast::Type::cast) {
            // A declared resource takes its written type; one that names an existing variable takes
            // the expression's.
            Some(declared) => context.ty_of_type(&declared)?,
            None => Expr::type_of(value.syntax(), context)?,
        };
        let Ty::Class(jals_hir::ClassTy::Project { id, .. }) = &ty else {
            return Err(LowerError::Unsupported("a resource of an unindexed type"));
        };
        let closer = context
            .index
            .resolve_member(*id, "close", jals_hir::Namespace::Method)
            .ok_or(LowerError::Unsupported("a resource with no `close()`"))?;
        let declaring = context.index.member(closer).owner;
        let owner = Descriptor::internal_name_of(declaring, context.index);
        let interface = context.index.item(declaring).kind == jals_hir::DefKind::Interface;

        // A declared resource is a local the body can read; one naming an existing variable still gets
        // a slot, because the handler needs the value the acquisition produced rather than whatever
        // the name holds later.
        let slot = match resource.binding() {
            Some(name) => {
                let id = context
                    .facts()
                    .def_at_token(&name)
                    .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
                emit.slots.declare(id, 1)
            }
            None => emit.slots.declare_temporary(1),
        };
        Expr::lower_as(&value, &ty, context, emit)?;
        let descriptor = Descriptor::descriptor_of(&ty, context.index)?.to_string();
        emit.asm.store_as(slot, &descriptor)?;
        Ok((slot, owner, interface))
    }

    /// `try { r.close(); } catch (Throwable s) { primary.addSuppressed(s); }`, guarded by a null check.
    fn close_suppressing(
        slot: u16,
        owner: &str,
        interface: bool,
        primary: u16,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let skip = emit.asm.label();
        let start = emit.asm.label();
        let handler = emit.asm.label();

        emit.asm.load(slot)?;
        emit.asm.branch(Branch::RefNull(true), skip)?;
        emit.asm.bind(start)?;
        emit.asm.load(slot)?;
        if interface {
            emit.asm.invoke_interface(owner, "close", "()V")?;
        } else {
            emit.asm.invoke_virtual(owner, "close", "()V")?;
        }
        let end = emit.asm.label();
        emit.asm.mark(end)?;
        emit.asm.branch(Branch::Always, skip)?;

        emit.asm.bind_handler(handler, start, THROWABLE)?;
        emit.asm.protect(start, end, handler, None)?;
        emit.asm.load(primary)?;
        emit.asm.swap()?;
        emit.asm
            .invoke_virtual(THROWABLE, "addSuppressed", "(Ljava/lang/Throwable;)V")?;
        Ok(emit.asm.bind(skip)?)
    }

    /// The type a `catch` clause's binding has: its one arm, or the nearest type every arm shares.
    fn caught_type(clause: &ast::CatchClause, context: &Context<'_>) -> Result<Ty> {
        let arms: Vec<Ty> = clause
            .types()
            .map(|ty| context.ty_of_type(&ty))
            .collect::<Result<_>>()?;
        let (first, rest) = arms
            .split_first()
            .ok_or(LowerError::Unsupported("a `catch` with no type"))?;
        if rest.is_empty() {
            return Ok(first.clone());
        }
        Ok(context.common_supertype(&arms))
    }

    /// `assert c;` / `assert c : message;`
    ///
    /// Guarded by the synthetic `$assertionsDisabled` field, because assertions are *off* unless the
    /// JVM was started with `-ea`. Emitting the check unguarded would change what the program does:
    /// every `assert` would run, and one that a release build relies on being skipped would throw.
    fn assert(
        statement: &ast::AssertStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let mut parts = statement.syntax().children().filter_map(ast::Expr::cast);
        let condition = parts
            .next()
            .ok_or(LowerError::Unsupported("an `assert` with no condition"))?;
        let message = parts.next();

        let holds = emit.asm.label();
        emit.asm
            .get_static(&context.this_class, ASSERTIONS_DISABLED, "Z")?;
        emit.asm.branch(Branch::IntZero(Compare::Ne), holds)?;
        Expr::lower(&condition, context, emit)?;
        emit.asm.branch(Branch::IntZero(Compare::Ne), holds)?;

        emit.asm.new_object(ASSERTION_ERROR)?;
        emit.asm.dup()?;
        match &message {
            // The one-argument form takes an `Object`, which is why a `String` message needs no
            // conversion and an `int` one would need boxing.
            Some(message) => {
                Expr::lower(message, context, emit)?;
                emit.asm.invoke_special(
                    ASSERTION_ERROR,
                    "<init>",
                    "(Ljava/lang/Object;)V",
                    false,
                )?;
            }
            None => emit
                .asm
                .invoke_special(ASSERTION_ERROR, "<init>", "()V", false)?,
        }
        emit.asm.throw()?;
        Ok(emit.asm.bind(holds)?)
    }

    /// `Type name = value;` — allocate the slot, then store the initialiser into it.
    ///
    /// The slot is allocated *before* the initialiser runs, matching the order a JVM frame is laid
    /// out; the initialiser cannot read the variable it is initialising, so nothing observes it.
    fn local(
        declaration: &ast::LocalVarDecl,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // Each declarator with the value written after its own `=`. Pairing names with expressions
        // by index — which this did — gave `int a, b = 2;` its `2` on `a` and left `b`'s slot
        // unwritten, which the verifier rejects on the first read of `b`.
        for (name, value) in Facts::declarators(declaration.syntax()) {
            let id = context
                .facts()
                .def_at_token(&name)
                .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
            let ty = context.typed.type_of_def(id).clone();
            let slot = emit.slots.declare(id, Slots::ty_width(&ty));
            let Some(value) = value else {
                // A declaration with no initialiser writes nothing; the slot stays unset until an
                // assignment gives it a type, which is exactly what the verifier assumes.
                continue;
            };
            // Converted to the *declared* type, which is where `long n = 1;` gets its `i2l`.
            Expr::lower_as(&value, &ty, context, emit)?;
            let descriptor = Descriptor::descriptor_of(&ty, context.index)?.to_string();
            emit.asm.store_as(slot, &descriptor)?;
        }
        Ok(())
    }

    /// An expression evaluated for its effect: whatever it left on the stack has to come off.
    fn expression(
        statement: &ast::ExprStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let Some(expression) = statement.expr() else {
            return Ok(());
        };
        Self::discarded(&expression, context, emit)
    }

    /// Emit `expression` for its effect, popping whatever value it produced.
    ///
    /// The JVM has no "evaluate and drop", so the caller pops back down to the depth it started at.
    /// A `for` header's init and update sections need this too: they are bare expressions in the
    /// grammar, and an `i++` left on the stack at the back edge is a frame the loop head cannot merge.
    pub(crate) fn discarded(
        expression: &ast::Expr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let before = emit.asm.stack_depth();
        Expr::lower(expression, context, emit)?;
        while emit.asm.stack_depth() > before {
            emit.asm.pop()?;
        }
        Ok(())
    }

    fn ret(
        statement: &ast::ReturnStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let crossed = emit.all_crossed();
        let Some(value) = statement.expr() else {
            Self::run_guards(&crossed, context, emit)?;
            // A cleanup that took its own exit *is* the exit taken; this `return` was discarded.
            if !emit.asm.reachable() {
                return Ok(());
            }
            return Ok(emit.asm.return_(None)?);
        };
        // Converted to the *declared* return type. Reading the opcode off the stack instead emitted
        // `ireturn` for `long f() { return 1; }` — a class file whose descriptor promises a `long` and
        // whose body returns an `int`.
        let returns = emit.returns().clone();
        Expr::lower_as(&value, &returns, context, emit)?;
        // And cast down to it when the value's own erasure is `Object`: `<T> T pick(..)` returned
        // where the method declares `Exception[]` is legal source, a right descriptor, and an
        // `areturn` the verifier rejects. javac emits the same `checkcast`.
        Expr::narrow_erased(&value, &returns, context, emit)?;
        let ty = emit.asm.stack_top().ok_or(LowerError::Unsupported(
            "a `return` whose value left nothing on the stack",
        ))?;
        if crossed.is_empty() {
            return Ok(emit.asm.return_(Some(&ty))?);
        }
        // A `finally` runs *after* the value is computed and cannot change it (JLS §14.20.2), so the
        // value goes into a slot of its own first. javac does the same.
        let width = u16::from(matches!(
            ty,
            jals_classfile::VerificationType::Long | jals_classfile::VerificationType::Double
        ));
        let held = emit.slots.declare_temporary(width + 1);
        emit.asm.store(held)?;
        Self::run_guards(&crossed, context, emit)?;
        if !emit.asm.reachable() {
            return Ok(());
        }
        emit.asm.load(held)?;
        Ok(emit.asm.return_(Some(&ty))?)
    }

    /// Run the `finally` blocks `crossed` names, innermost first.
    ///
    /// Each copy is inlined rather than jumped to — `jsr` / `ret` is the alternative and no verifier
    /// since Java 6 accepts it. Each copy also *interrupts* the protected range it sits in: an
    /// exception thrown by a `finally` must not reach the handler whose job is to run that `finally`,
    /// so the range closes before the copy and a fresh one opens after it. javac splits its ranges the
    /// same way, which is how one finds out it is required at all.
    fn run_guards(crossed: &[usize], context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        for &index in crossed {
            let Some(cleanup) = emit.guard_cleanup(index) else {
                continue;
            };
            let end = emit.asm.label();
            emit.asm.mark(end)?;
            emit.close_range(index, end);

            // The copy runs outside its own guard and every guard inside it: those have already run,
            // and re-entering this one would make a `return` inside a `finally` run it twice.
            let inner = emit.split_guards(index);
            let result = Self::cleanup(&cleanup, context, emit);
            emit.rejoin_guards(inner);
            result?;

            // Whatever follows the exit this copy belongs to is protected again. `mark` rather than
            // `bind`, because the exit's own transfer has already made this position unreachable.
            let start = emit.asm.label();
            emit.asm.mark(start)?;
            emit.open_range(index, start);

            // The cleanup completed abruptly — a `return`, `break`, or `continue` of its own — and
            // §14.20.2 gives that priority over the exit it interrupted: the original one is
            // discarded, so neither the cleanups outside this one nor the exit itself happen.
            // Emitting them anyway is code after an unconditional transfer, which is what the
            // assembler reported instead of compiling `try { return 1; } finally { return 2; }`.
            if !emit.asm.reachable() {
                break;
            }
        }
        Ok(())
    }

    /// `if (c) { … } else { … }`: test, jump over the taken branch when false, and — when there is
    /// an `else` — jump over that from the end of the taken one.
    fn conditional(
        statement: &ast::IfStmt,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(LowerError::Unsupported("an `if` with no condition"))?;
        let otherwise = emit.asm.label();
        let done = emit.asm.label();

        // The `then` and `else` arms are the condition's sibling statements, in that order.
        let mut branches = statement.branches();
        let then_branch = branches.next();
        let else_branch = branches.next();

        Expr::lower(&condition, context, emit)?;
        // The condition is a `boolean`, which is an `int` on the stack: zero is false.
        emit.asm.branch(Branch::IntZero(Compare::Eq), otherwise)?;
        if let Some(then) = then_branch {
            Self::lower(&then, context, emit)?;
        }
        // The jump over the `else` arm exists only when the `then` arm can fall out of it. A
        // `then` ending in `return` leaves nothing to jump *from*, and `if (c) { return; } …` is
        // the ordinary shape of exactly that.
        let joins = emit.asm.reachable();
        if joins {
            emit.asm.branch(Branch::Always, done)?;
        }
        emit.asm.bind(otherwise)?;
        if let Some(otherwise) = else_branch {
            Self::lower(&otherwise, context, emit)?;
        }
        // `done` is a label only if something arrives there. When both arms returned, nothing
        // does, and binding it would report a label control cannot reach.
        if joins || emit.asm.reachable() {
            emit.asm.bind(done)?;
        }
        Ok(())
    }

    /// `break;` / `break l;` / `continue;` / `continue l;`.
    ///
    /// The label comes in already read: where it lives on the statement is a grammar fact, and both
    /// lowerings used to walk for it themselves.
    fn leave(
        label: Option<SyntaxToken>,
        exit: bool,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let label = label.map(|token| jals_syntax::decoded_ident(&token).into_owned());
        let (target, depth) = if exit {
            emit.exit_of(label.as_deref())?
        } else {
            emit.next_of(label.as_deref())?
        };
        // Leaving a region runs its `finally` first, however far out the jump goes.
        let crossed = emit.crossed(depth);
        Self::run_guards(&crossed, context, emit)?;
        // A cleanup that took its own exit replaced this one, so there is nothing left to jump to.
        if !emit.asm.reachable() {
            return Ok(());
        }
        Ok(emit.asm.branch(Branch::Always, target)?)
    }

    /// A labelled statement that is not a loop: `l: { … break l; … }`.
    fn labelled_block(
        statement: &ast::Stmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let done = emit.asm.label();
        Self::in_scope(labels, done, None, emit, |emit| {
            Self::lower(statement, context, emit)
        })?;
        Self::join(done, emit)
    }

    fn while_loop(
        statement: &ast::WhileStmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(LowerError::Unsupported("a `while` with no condition"))?;
        let test = emit.asm.label();
        let done = emit.asm.label();

        emit.asm.bind(test)?;
        // `while (true)` has no test and no exit but a `break` (JLS §14.21): the statement after it
        // is unreachable, so emitting the branch anyway leaves a conditional jump to an offset past
        // the last instruction — which is a `StackMapTable` frame on no instruction and a verifier
        // saying the control flow falls through the code end.
        if context.facts().constant_condition(&condition) != Some(true) {
            Expr::lower(&condition, context, emit)?;
            emit.asm.branch(Branch::IntZero(Compare::Eq), done)?;
        }
        // A `while`'s condition is also where a `continue` goes, which is the one loop shape where
        // the two labels coincide.
        Self::in_scope(labels, done, Some(test), emit, |emit| {
            Self::body(statement.body().as_ref(), context, emit)
        })?;
        // A body that ends in `return` never reaches the back edge.
        if emit.asm.reachable() {
            emit.asm.branch(Branch::Always, test)?;
        }
        Self::join(done, emit)
    }

    /// `do { … } while (c);` — the body runs before the condition is ever tested, so the back edge is
    /// the *only* edge into it.
    fn do_while(
        statement: &ast::DoWhileStmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(LowerError::Unsupported("a `do` with no condition"))?;
        let top = emit.asm.label();
        let test = emit.asm.label();
        let done = emit.asm.label();

        emit.asm.bind(top)?;
        // A `continue` in a `do` runs the condition, not the body again.
        Self::in_scope(labels, done, Some(test), emit, |emit| {
            Self::body(statement.body().as_ref(), context, emit)
        })?;
        if emit.asm.reachable() || emit.asm.is_targeted(test)? {
            emit.asm.bind(test)?;
            // The back edge of a `do … while (true)` is unconditional, for the same reason a
            // `while (true)` has no forward one. The label is still bound: a `continue` in the body
            // jumps to the condition, and here that is the `goto` itself.
            if context.facts().constant_condition(&condition) == Some(true) {
                emit.asm.branch(Branch::Always, top)?;
            } else {
                Expr::lower(&condition, context, emit)?;
                emit.asm.branch(Branch::IntZero(Compare::Ne), top)?;
            }
        }
        Self::join(done, emit)
    }

    /// `for (init; condition; update) body`.
    ///
    /// The header is flat in the CST, so which section a child sits in is not a matter of its type;
    /// [`ast::ForStmt`]'s accessors own that walk.
    fn for_loop(
        statement: &ast::ForStmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        for section in statement.init() {
            Self::for_section(&section, context, emit)?;
        }

        let test = emit.asm.label();
        let next = emit.asm.label();
        let done = emit.asm.label();
        emit.asm.bind(test)?;
        // `for (;;)` has no condition, which means no exit but a `break` — and `for (; true;)` is
        // the same loop written out, so a constantly-true condition takes the same arm rather than
        // emitting a branch past the end of the method.
        if let Some(condition) = statement
            .condition()
            .filter(|condition| context.facts().constant_condition(condition) != Some(true))
        {
            Expr::lower(&condition, context, emit)?;
            emit.asm.branch(Branch::IntZero(Compare::Eq), done)?;
        }

        // A `continue` runs the update section (JLS §14.14.1.3), so it goes to `next` rather than to
        // `test`. Sending it to `test` skips the update and never terminates.
        let body = statement.body();
        Self::in_scope(labels, done, Some(next), emit, |emit| {
            Self::body(body.as_ref(), context, emit)
        })?;

        // The update section is reachable if the body can fall out of it *or* a `continue` jumped
        // there. `for (;;) { return; }` is the ordinary shape where neither holds.
        if emit.asm.reachable() || emit.asm.is_targeted(next)? {
            emit.asm.bind(next)?;
            for section in statement.update() {
                Self::for_section(&section, context, emit)?;
            }
            emit.asm.branch(Branch::Always, test)?;
        }
        Self::join(done, emit)
    }

    /// One entry of a `for` header's init or update section.
    ///
    /// A `LocalVarDecl` in the init declares the loop variable; anything else is a bare `Expr`
    /// evaluated for its effect, which is not an `ExprStmt` in the grammar and so needs the pop
    /// treatment applied here rather than inherited.
    fn for_section(
        node: &SyntaxNode,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        if let Some(declaration) = ast::LocalVarDecl::cast(node.clone()) {
            return Self::local(&declaration, context, emit);
        }
        let expression = ast::Expr::cast(node.clone())
            .ok_or(LowerError::Unsupported("a `for` header this cannot read"))?;
        Self::discarded(&expression, context, emit)
    }

    /// `for (T v : array) body`, which JLS §14.14.2 defines as an indexed loop.
    ///
    /// The array, its length, and the index all live in slots the source never named: the array
    /// expression may not be re-evaluated, and re-reading `arraylength` every iteration would be
    /// wrong if the loop assigned to the variable holding it.
    fn for_each(
        statement: &ast::ForEachStmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let iterable = statement.iterable().ok_or(LowerError::Unsupported(
            "a `for`-each with nothing to iterate",
        ))?;
        let Ty::Array(element) = Expr::type_of(iterable.syntax(), context)? else {
            return Self::for_each_iterator(statement, &iterable, labels, context, emit);
        };
        let descriptor = Descriptor::descriptor_of(&element, context.index)?.to_string();

        let array = emit.slots.declare_temporary(1);
        let length = emit.slots.declare_temporary(1);
        let cursor = emit.slots.declare_temporary(1);
        Expr::lower(&iterable, context, emit)?;
        emit.asm.store(array)?;
        emit.asm.load(array)?;
        emit.asm.array_length()?;
        emit.asm.store(length)?;
        emit.asm.const_int(0)?;
        emit.asm.store(cursor)?;

        let test = emit.asm.label();
        let next = emit.asm.label();
        let done = emit.asm.label();
        emit.asm.bind(test)?;
        emit.asm.load(cursor)?;
        emit.asm.load(length)?;
        emit.asm.branch(Branch::IntCmp(Compare::Ge), done)?;

        // The loop variable is written from the element at the top of every iteration, which is what
        // makes it a fresh binding per pass rather than one the body carries over.
        let binding = Self::for_each_binding(statement, context)?;
        let variable = emit
            .slots
            .declare(binding, Slots::ty_width(context.typed.type_of_def(binding)));
        emit.asm.load(array)?;
        emit.asm.load(cursor)?;
        emit.asm.array_load(&descriptor)?;
        // The element descriptor is the array's, and the binding's declared type may be wider than
        // it (`for (Object o : strings)`), so the slot is typed by the declaration.
        let declared =
            Descriptor::descriptor_of(&context.typed.type_of_def(binding).clone(), context.index)?
                .to_string();
        emit.asm.store_as(variable, &declared)?;

        Self::in_scope(labels, done, Some(next), emit, |emit| {
            Self::body(statement.body().as_ref(), context, emit)
        })?;

        if emit.asm.reachable() || emit.asm.is_targeted(next)? {
            emit.asm.bind(next)?;
            emit.asm.increment(cursor, 1)?;
            emit.asm.branch(Branch::Always, test)?;
        }
        Self::join(done, emit)
    }

    /// `for (T v : iterable) body` over something that is not an array, which JLS §14.14.2 defines as
    /// a loop over `iterable.iterator()`.
    ///
    /// The three calls are named on the *interfaces* that declare them rather than on the receiver's
    /// own type. That resolves for any receiver assignable to `Iterable`, which is exactly the
    /// condition checked before emitting — and it means one pair of descriptors serves every
    /// collection instead of one per static type.
    ///
    /// `next()` returns `Object` after erasure, so the element needs a `checkcast` on the way into
    /// the loop variable. Without it the variable would hold an `Object` the frame describes as one,
    /// and the first method call on it would fail verification.
    fn for_each_iterator(
        statement: &ast::ForEachStmt,
        iterable: &ast::Expr,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        const ITERABLE: &str = "java/lang/Iterable";
        const ITERATOR: &str = "java/util/Iterator";

        // Only a type whose members the index holds can be checked for `iterator()`, and a receiver
        // that does not have one is a program the linter reports rather than one to emit for.
        let Ty::Class(jals_hir::ClassTy::Project { id, .. }) =
            Expr::type_of(iterable.syntax(), context)?
        else {
            return Err(LowerError::Unsupported("a `for`-each over this type"));
        };
        if context
            .index
            .resolve_member(id, "iterator", jals_hir::Namespace::Method)
            .is_none()
        {
            return Err(LowerError::Unsupported(
                "a `for`-each over a non-`Iterable`",
            ));
        }

        let cursor = emit.slots.declare_temporary(1);
        Expr::lower(iterable, context, emit)?;
        emit.asm
            .invoke_interface(ITERABLE, "iterator", "()Ljava/util/Iterator;")?;
        emit.asm.store(cursor)?;

        let test = emit.asm.label();
        let next = emit.asm.label();
        let done = emit.asm.label();
        emit.asm.bind(test)?;
        emit.asm.load(cursor)?;
        emit.asm.invoke_interface(ITERATOR, "hasNext", "()Z")?;
        emit.asm.branch(Branch::IntZero(Compare::Eq), done)?;

        let binding = Self::for_each_binding(statement, context)?;
        let element = context.typed.type_of_def(binding).clone();
        let variable = emit.slots.declare(binding, Slots::ty_width(&element));
        emit.asm.load(cursor)?;
        emit.asm
            .invoke_interface(ITERATOR, "next", "()Ljava/lang/Object;")?;
        emit.asm
            .check_cast(&Descriptor::class_entry(&element, context.index)?)?;
        let declared = Descriptor::descriptor_of(&element, context.index)?.to_string();
        emit.asm.store_as(variable, &declared)?;

        Self::in_scope(labels, done, Some(next), emit, |emit| {
            Self::body(statement.body().as_ref(), context, emit)
        })?;

        if emit.asm.reachable() || emit.asm.is_targeted(next)? {
            emit.asm.bind(next)?;
            emit.asm.branch(Branch::Always, test)?;
        }
        Self::join(done, emit)
    }

    /// The definition a `for`-each's loop variable declares.
    fn for_each_binding(
        statement: &ast::ForEachStmt,
        context: &Context<'_>,
    ) -> Result<jals_hir::DefId> {
        let name: SyntaxToken = statement
            .name_token()
            .ok_or(LowerError::Unsupported("a `for`-each with no variable"))?;
        context
            .facts()
            .def_at_token(&name)
            .ok_or_else(|| LowerError::Unresolved(name.text().into()))
    }

    /// A loop's body, which the grammar allows to be absent (`while (c);` is a legal, if pointless,
    /// statement).
    fn body(
        body: Option<&ast::Stmt>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        body.map_or(Ok(()), |body| Self::lower(body, context, emit))
    }

    /// Run `body` with one more construct a `break` (and maybe a `continue`) can leave.
    fn in_scope(
        labels: Vec<String>,
        exit: Label,
        next: Option<Label>,
        emit: &mut Emit<'_, '_>,
        body: impl FnOnce(&mut Emit<'_, '_>) -> Result<()>,
    ) -> Result<()> {
        emit.enter(labels, exit, next);
        let result = body(emit);
        emit.leave();
        result
    }

    /// Bind a construct's exit label, if anything arrives there.
    ///
    /// `while (true) { }` with no `break` has no exit: nothing falls out of it and nothing jumps out,
    /// so the statement after it is unreachable — which is what the JLS says too, and what leaving
    /// the label unbound records.
    fn join(done: Label, emit: &mut Emit<'_, '_>) -> Result<()> {
        if emit.asm.reachable() || emit.asm.is_targeted(done)? {
            emit.asm.bind(done)?;
        }
        Ok(())
    }
}
