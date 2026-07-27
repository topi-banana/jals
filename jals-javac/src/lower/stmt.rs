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
use jals_syntax::SyntaxKind::{IDENT, RPAREN, SEMICOLON};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::desc::Descriptor;
use crate::jvm::{Branch, Compare, Label};
use crate::lower::expr::Expr;
use crate::lower::slots::Slots;
use crate::lower::{Context, Emit, LowerError, Result};

/// Statement lowering.
pub(crate) struct Stmt;

impl Stmt {
    /// Emit every statement in `block`.
    pub(crate) fn block(
        block: &ast::Block,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
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
            ast::Stmt::Break(statement) => Self::leave(statement.syntax(), true, emit),
            ast::Stmt::Continue(statement) => Self::leave(statement.syntax(), false, emit),
            _ => Err(LowerError::Unsupported("this statement form")),
        }
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
        // The CST is flat: each declarator name takes the next expression sibling as its value.
        let names: Vec<_> = declaration.names().collect();
        let values: Vec<_> = declaration
            .syntax()
            .children()
            .filter_map(ast::Expr::cast)
            .collect();
        for (index, name) in names.iter().enumerate() {
            let id = context
                .resolved
                .symbol_at(usize::from(name.text_range().start()))
                .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
            let ty = context.inference.type_of_def(id).clone();
            let slot = emit.slots.declare(id, Slots::ty_width(&ty));
            let Some(value) = values.get(index) else {
                // A declaration with no initialiser writes nothing; the slot stays unset until an
                // assignment gives it a type, which is exactly what the verifier assumes.
                continue;
            };
            // Converted to the *declared* type, which is where `long n = 1;` gets its `i2l`.
            Expr::lower_as(value, &ty, context, emit)?;
            emit.asm.store(slot)?;
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
    fn discarded(
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
        match statement.expr() {
            Some(value) => {
                // Converted to the *declared* return type. Reading the opcode off the stack instead
                // emitted `ireturn` for `long f() { return 1; }` — a class file whose descriptor
                // promises a `long` and whose body returns an `int`.
                let returns = emit.returns().clone();
                Expr::lower_as(&value, &returns, context, emit)?;
                let ty = emit.asm.stack_top().ok_or(LowerError::Unsupported(
                    "a `return` whose value left nothing on the stack",
                ))?;
                emit.asm.return_(Some(&ty))?;
            }
            None => emit.asm.return_(None)?,
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
    /// The optional label is a bare `IDENT` token on the statement — the grammar has no slot for it,
    /// because there is nothing else it could be.
    fn leave(node: &SyntaxNode, exit: bool, emit: &mut Emit<'_, '_>) -> Result<()> {
        let label = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == IDENT)
            .map(|token| String::from(token.text()));
        let target = if exit {
            emit.exit_of(label.as_deref())?
        } else {
            emit.next_of(label.as_deref())?
        };
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
        Expr::lower(&condition, context, emit)?;
        emit.asm.branch(Branch::IntZero(Compare::Eq), done)?;
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
            Expr::lower(&condition, context, emit)?;
            emit.asm.branch(Branch::IntZero(Compare::Ne), top)?;
        }
        Self::join(done, emit)
    }

    /// `for (init; condition; update) body`.
    ///
    /// The header is flat in the CST: the two `;` and the `)` are direct token children, and every
    /// section between them is a run of siblings. The body is whatever follows the `)`.
    fn for_loop(
        statement: &ast::ForStmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let (init, condition, update, body) = Self::for_sections(statement.syntax());

        for section in &init {
            Self::for_section(section, context, emit)?;
        }

        let test = emit.asm.label();
        let next = emit.asm.label();
        let done = emit.asm.label();
        emit.asm.bind(test)?;
        // `for (;;)` has no condition, which means no exit but a `break`.
        if let Some(condition) = &condition {
            Expr::lower(condition, context, emit)?;
            emit.asm.branch(Branch::IntZero(Compare::Eq), done)?;
        }

        // A `continue` runs the update section (JLS §14.14.1.3), so it goes to `next` rather than to
        // `test`. Sending it to `test` skips the update and never terminates.
        Self::in_scope(labels, done, Some(next), emit, |emit| {
            Self::body(body.as_ref(), context, emit)
        })?;

        // The update section is reachable if the body can fall out of it *or* a `continue` jumped
        // there. `for (;;) { return; }` is the ordinary shape where neither holds.
        if emit.asm.reachable() || emit.asm.is_targeted(next)? {
            emit.asm.bind(next)?;
            for section in &update {
                Self::for_section(section, context, emit)?;
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

    /// Split a `for`'s header into its three sections plus the body.
    fn for_sections(
        node: &SyntaxNode,
    ) -> (
        Vec<SyntaxNode>,
        Option<ast::Expr>,
        Vec<SyntaxNode>,
        Option<ast::Stmt>,
    ) {
        let (mut init, mut update) = (Vec::new(), Vec::new());
        let (mut condition, mut body) = (None, None);
        // 0 = init, 1 = condition, 2 = update; past the `)`, the body.
        let mut section = 0;
        let mut in_header = true;
        for child in node.children_with_tokens() {
            match child {
                jals_syntax::SyntaxElement::Token(token) => match token.kind() {
                    SEMICOLON if in_header => section += 1,
                    RPAREN => in_header = false,
                    _ => {}
                },
                jals_syntax::SyntaxElement::Node(child) => {
                    if !in_header {
                        body = ast::Stmt::cast(child);
                    } else if section == 0 {
                        init.push(child);
                    } else if section == 1 {
                        condition = ast::Expr::cast(child);
                    } else {
                        update.push(child);
                    }
                }
            }
        }
        (init, condition, update, body)
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
        let binding = Self::for_each_binding(statement.syntax(), context)?;
        let variable = emit.slots.declare(
            binding,
            Slots::ty_width(context.inference.type_of_def(binding)),
        );
        emit.asm.load(array)?;
        emit.asm.load(cursor)?;
        emit.asm.array_load(&descriptor)?;
        emit.asm.store(variable)?;

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

        let binding = Self::for_each_binding(statement.syntax(), context)?;
        let element = context.inference.type_of_def(binding).clone();
        let variable = emit.slots.declare(binding, Slots::ty_width(&element));
        emit.asm.load(cursor)?;
        emit.asm
            .invoke_interface(ITERATOR, "next", "()Ljava/lang/Object;")?;
        emit.asm
            .check_cast(&Descriptor::class_entry(&element, context.index)?)?;
        emit.asm.store(variable)?;

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
    fn for_each_binding(node: &SyntaxNode, context: &Context<'_>) -> Result<jals_hir::DefId> {
        let name: SyntaxToken = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == IDENT)
            .ok_or(LowerError::Unsupported("a `for`-each with no variable"))?;
        context
            .resolved
            .symbol_at(usize::from(name.text_range().start()))
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
