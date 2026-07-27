//! Statement lowering: every form leaves the operand stack as it found it.

use jals_syntax::ast::{self, AstNode as _};

use crate::jvm::{Assembler, Branch, Compare};
use crate::lower::expr::Expr;
use crate::lower::slots::Slots;
use crate::lower::{Context, LowerError, Result};

/// Statement lowering.
pub(crate) struct Stmt;

impl Stmt {
    /// Emit every statement in `block`.
    pub(crate) fn block(
        block: &ast::Block,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &mut Slots,
    ) -> Result<()> {
        for statement in block.stmts() {
            Self::lower(&statement, context, asm, slots)?;
        }
        Ok(())
    }

    /// Emit one statement.
    fn lower(
        statement: &ast::Stmt,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &mut Slots,
    ) -> Result<()> {
        match statement {
            ast::Stmt::Block(block) => Self::block(block, context, asm, slots),
            ast::Stmt::Empty(_) => Ok(()),
            ast::Stmt::LocalVar(declaration) => Self::local(declaration, context, asm, slots),
            ast::Stmt::Expr(expression) => Self::expression(expression, context, asm, slots),
            ast::Stmt::Return(statement) => Self::ret(statement, context, asm, slots),
            ast::Stmt::If(statement) => Self::conditional(statement, context, asm, slots),
            ast::Stmt::While(statement) => Self::while_loop(statement, context, asm, slots),
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
        asm: &mut Assembler<'_>,
        slots: &mut Slots,
    ) -> Result<()> {
        // The CST is flat: each declarator name takes the next expression sibling as its value.
        let names: alloc::vec::Vec<_> = declaration.names().collect();
        let values: alloc::vec::Vec<_> = declaration
            .syntax()
            .children()
            .filter_map(ast::Expr::cast)
            .collect();
        for (index, name) in names.iter().enumerate() {
            let id = context
                .resolved
                .symbol_at(usize::from(name.text_range().start()))
                .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
            let width = Slots::ty_width(context.inference.type_of_def(id));
            let slot = slots.declare(id, width);
            let Some(value) = values.get(index) else {
                // A declaration with no initialiser writes nothing; the slot stays unset until an
                // assignment gives it a type, which is exactly what the verifier assumes.
                continue;
            };
            Expr::lower(value, context, asm, slots)?;
            asm.store(slot)?;
        }
        Ok(())
    }

    /// An expression evaluated for its effect: whatever it left on the stack has to come off.
    fn expression(
        statement: &ast::ExprStmt,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        let Some(expression) = statement.expr() else {
            return Ok(());
        };
        let before = asm.stack_depth();
        Expr::lower(&expression, context, asm, slots)?;
        while asm.stack_depth() > before {
            asm.pop()?;
        }
        Ok(())
    }

    fn ret(
        statement: &ast::ReturnStmt,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
    ) -> Result<()> {
        match statement.expr() {
            Some(value) => {
                Expr::lower(&value, context, asm, slots)?;
                let ty = asm.stack_top().ok_or(LowerError::Unsupported(
                    "a `return` whose value left nothing on the stack",
                ))?;
                asm.return_(Some(&ty))?;
            }
            None => asm.return_(None)?,
        }
        Ok(())
    }

    /// `if (c) { … } else { … }`: test, jump over the taken branch when false, and — when there is
    /// an `else` — jump over that from the end of the taken one.
    fn conditional(
        statement: &ast::IfStmt,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &mut Slots,
    ) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(LowerError::Unsupported("an `if` with no condition"))?;
        let otherwise = asm.label();
        let done = asm.label();

        // The `then` and `else` arms are the condition's sibling statements, in that order.
        let mut branches = statement.branches();
        let then_branch = branches.next();
        let else_branch = branches.next();

        Expr::lower(&condition, context, asm, slots)?;
        // The condition is a `boolean`, which is an `int` on the stack: zero is false.
        asm.branch(Branch::IntZero(Compare::Eq), otherwise)?;
        if let Some(then) = then_branch {
            Self::lower(&then, context, asm, slots)?;
        }
        // The jump over the `else` arm exists only when the `then` arm can fall out of it. A
        // `then` ending in `return` leaves nothing to jump *from*, and `if (c) { return; } …` is
        // the ordinary shape of exactly that.
        let joins = asm.reachable();
        if joins {
            asm.branch(Branch::Always, done)?;
        }
        asm.bind(otherwise)?;
        if let Some(otherwise) = else_branch {
            Self::lower(&otherwise, context, asm, slots)?;
        }
        // `done` is a label only if something arrives there. When both arms returned, nothing
        // does, and binding it would report a label control cannot reach.
        if joins || asm.reachable() {
            asm.bind(done)?;
        }
        Ok(())
    }

    fn while_loop(
        statement: &ast::WhileStmt,
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &mut Slots,
    ) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(LowerError::Unsupported("a `while` with no condition"))?;
        let test = asm.label();
        let done = asm.label();

        asm.bind(test)?;
        Expr::lower(&condition, context, asm, slots)?;
        asm.branch(Branch::IntZero(Compare::Eq), done)?;
        if let Some(body) = statement.body() {
            Self::lower(&body, context, asm, slots)?;
        }
        // A body that ends in `return` never reaches the back edge. `done` still has the exit
        // branch arriving at it either way, so it binds regardless.
        if asm.reachable() {
            asm.branch(Branch::Always, test)?;
        }
        asm.bind(done)?;
        Ok(())
    }
}
