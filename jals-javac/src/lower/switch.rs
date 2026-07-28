//! `switch`, as a statement and as an expression.
//!
//! # The JVM switches on an `int` and nothing else
//!
//! `tableswitch` and `lookupswitch` both take an `int` key, so every other selector type is a
//! *lowering*. An integral one narrower than `long` is already an `int`. A `String` is not: it becomes
//! a `switch` on `hashCode()` followed by an `equals` chain per hash bucket, because two different
//! strings can hash alike and the switch must not pick one of them arbitrarily.
//!
//! # Two syntaxes, one shape
//!
//! The colon form falls through — a group's statements run into the next group's unless a `break`
//! intervenes — and the arrow form does not. That is the only difference, and it is one `goto` per arm.
//!
//! # A `case` label is a constant, and this has to know its value
//!
//! A jump table is built at compile time, so `case X` needs `X`'s value now. Only what the JLS calls a
//! constant expression can appear there, and the subset evaluated here is literals, `+` / `-` on them,
//! and parentheses. A label this cannot evaluate is *reported*: guessing would silently send a key to
//! the wrong arm, in a class file that verifies and runs.

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::{Primitive, Ty};
use jals_syntax::SyntaxKind::{
    CHAR_LITERAL, INT_LITERAL, MINUS, PLUS, RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN,
};
use jals_syntax::ast::{self, AstNode as _};

use crate::jvm::{Branch, Compare, Label};
use crate::lower::expr::Expr;
use crate::lower::stmt::Stmt;
use crate::lower::{Context, Emit, LowerError, Result};

/// One arm of a lowered `switch`: what it matches, and where its body is.
struct Arm {
    /// The `case` keys that reach this arm, already evaluated. Empty for `default`.
    keys: Vec<Key>,
    /// The `case T t` patterns that reach this arm, in the order they are written.
    ///
    /// A pattern is not a constant, so it indexes no jump table: a `switch` with one dispatches by
    /// testing each arm's type in source order, which is what §14.11.1 says a pattern `switch` does.
    patterns: Vec<jals_syntax::SyntaxNode>,
    /// The arm's `when` clause, which runs after the pattern bound and before the arm is taken.
    guard: Option<ast::Expr>,
    /// Whether one of this arm's labels is `default`.
    is_default: bool,
    /// Where the arm's body begins.
    entry: Label,
}

/// A `case` label's value.
#[derive(Clone, PartialEq, Eq)]
enum Key {
    /// An integral constant, which is what the jump table indexes on directly.
    Int(i32),
    /// A `String` constant, matched by hash and then by `equals`.
    Text(String),
}

/// `switch` lowering.
pub(crate) struct Switch;

impl Switch {
    /// `switch (selector) { … }` as a statement.
    pub(crate) fn statement(
        statement: &ast::SwitchStmt,
        labels: Vec<String>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let selector = statement
            .selector()
            .ok_or(LowerError::Unsupported("a `switch` with no selector"))?;
        let body = statement
            .body()
            .ok_or(LowerError::Unsupported("a `switch` with no body"))?;
        Self::lower(&selector, &body, labels, None, context, emit)
    }

    /// `switch (selector) { … }` as an expression, leaving its value on the stack.
    pub(crate) fn expression(
        expression: &ast::SwitchExpr,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let selector = expression
            .selector()
            .ok_or(LowerError::Unsupported("a `switch` with no selector"))?;
        let body = expression
            .body()
            .ok_or(LowerError::Unsupported("a `switch` with no body"))?;
        let result = Expr::type_of(expression.syntax(), context)?;
        Self::lower(&selector, &body, Vec::new(), Some(&result), context, emit)
    }

    /// The shared shape. `result` is `Some` for an expression, whose arms produce a value.
    fn lower(
        selector: &ast::Expr,
        body: &ast::SwitchBlock,
        labels: Vec<String>,
        result: Option<&Ty>,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let rules: Vec<ast::SwitchRule> = body.rules().collect();
        let groups: Vec<ast::SwitchGroup> = body.groups().collect();
        if !rules.is_empty() && !groups.is_empty() {
            // JLS §14.11.1 forbids mixing them, so this is not a program.
            return Err(LowerError::Unsupported("a `switch` mixing both forms"));
        }

        let done = emit.asm.label();
        let arms: Vec<Arm> = if rules.is_empty() {
            groups
                .iter()
                .map(|group| Self::arm(group.labels(), emit))
                .collect::<Result<_>>()?
        } else {
            rules
                .iter()
                .map(|rule| Self::arm(rule.label().into_iter(), emit))
                .collect::<Result<_>>()?
        };
        // A `switch` *expression* has to produce a value on every path, so an unmatched key cannot
        // simply fall out of it. Exhaustiveness over an `enum` or a sealed hierarchy is the other way
        // to satisfy that, and neither is lowered — so a `default` is required rather than assumed.
        let fallback = arms.iter().find(|arm| arm.is_default).map(|arm| arm.entry);
        if result.is_some() && fallback.is_none() {
            return Err(LowerError::Unsupported(
                "a `switch` expression with no `default`",
            ));
        }
        let fallback = fallback.unwrap_or(done);

        Self::dispatch(selector, &arms, fallback, context, emit)?;

        // `break` leaves the whole `switch`; `yield` hands it a value. Neither is a loop, so a
        // `continue` looks straight past it.
        emit.enter(labels, done, None);
        if let Some(result) = result {
            emit.enter_yield(done, result.clone());
        }
        let outcome = if rules.is_empty() {
            Self::groups(&groups, &arms, result, done, context, emit)
        } else {
            Self::rules(&rules, &arms, result, done, context, emit)
        };
        if result.is_some() {
            emit.leave_yield();
        }
        emit.leave();
        outcome?;

        if emit.asm.reachable() || emit.asm.is_targeted(done)? {
            emit.asm.bind(done)?;
        }
        Ok(())
    }

    /// One arm's keys and entry label, from its `case` / `default` labels.
    fn arm(labels: impl Iterator<Item = ast::SwitchLabel>, emit: &mut Emit<'_, '_>) -> Result<Arm> {
        let mut keys = Vec::new();
        let mut patterns = Vec::new();
        let mut guard = None;
        let mut is_default = false;
        for label in labels {
            if label.is_default() {
                is_default = true;
            }
            patterns.extend(label.syntax().children().filter(|child| {
                matches!(
                    child.kind(),
                    TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
                )
            }));
            if let Some(clause) = label.syntax().children().find_map(ast::Guard::cast) {
                guard = clause.condition();
                if guard.is_none() {
                    return Err(LowerError::Unsupported("a guarded `case`"));
                }
            }
            // A `Guard`'s condition is an expression child of the label too, so the keys are read only
            // when there is no guard to have contributed one.
            if guard.is_none() {
                for value in label.syntax().children().filter_map(ast::Expr::cast) {
                    keys.push(Self::key(&value)?);
                }
            }
        }
        Ok(Arm {
            keys,
            patterns,
            guard,
            is_default,
            entry: emit.asm.label(),
        })
    }

    /// Emit the selector and the jump into the arms.
    fn dispatch(
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: Label,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        if arms
            .iter()
            .any(|arm| !arm.patterns.is_empty() || arm.guard.is_some())
        {
            return Self::dispatch_patterns(selector, arms, fallback, context, emit);
        }
        let text = arms
            .iter()
            .flat_map(|arm| &arm.keys)
            .any(|key| matches!(key, Key::Text(_)));
        if text {
            return Self::dispatch_text(selector, arms, fallback, context, emit);
        }
        // The selector has to *already* be an `int` on the stack. Converting one that is not would
        // narrow it silently — a `long` selector is not a Java program, but an `l2i` would compile it
        // into one that switches on the low 32 bits.
        if !matches!(
            Expr::type_of(selector.syntax(), context)?,
            Ty::Primitive(Primitive::Byte | Primitive::Short | Primitive::Char | Primitive::Int)
        ) {
            return Err(LowerError::Unsupported("a `switch` on this selector type"));
        }
        Expr::lower(selector, context, emit)?;
        let cases = Self::int_cases(arms)?;
        Ok(emit.asm.switch(&cases, fallback)?)
    }

    /// A pattern `switch`: each arm's type is tested in source order, and the first match wins.
    ///
    /// No jump table, because a pattern is not a constant and there is nothing to index on. §14.11.1
    /// gives the first *matching* label, so the tests are emitted in the order they are written and a
    /// `default` is only reached by falling out of all of them — which is what `fallback` already is.
    ///
    /// Every binding is set to `null` before the chain rather than only on its own matching path. Java
    /// scopes a pattern variable to its arm so nothing can read another's, but the verifier merges every
    /// edge into an arm's entry and refuses a slot some edge left unwritten; `null` joins into any
    /// reference type, so one store up front settles all of them.
    fn dispatch_patterns(
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: Label,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // A constant beside a pattern would need the jump table this does not build.
        if arms.iter().any(|arm| !arm.keys.is_empty()) {
            return Err(LowerError::Unsupported("a `switch` mixing key types"));
        }
        for arm in arms {
            for pattern in &arm.patterns {
                Expr::declare_bindings(pattern, context, emit)?;
            }
        }
        let scratch = emit.slots.declare_temporary(1);
        Expr::lower(selector, context, emit)?;
        emit.asm.store(scratch)?;
        for arm in arms {
            // A bare `default` matches nothing here: it is where the chain lands when every test failed.
            if arm.patterns.is_empty() && arm.guard.is_none() {
                continue;
            }
            let next = emit.asm.label();
            for pattern in &arm.patterns {
                // Bound before the guard runs, because the guard is written in terms of the binding.
                Expr::match_pattern(pattern, scratch, next, None, context, emit)?;
            }
            if let Some(guard) = &arm.guard {
                Expr::lower(guard, context, emit)?;
                emit.asm.branch(Branch::IntZero(Compare::Eq), next)?;
            }
            emit.asm.branch(Branch::Always, arm.entry)?;
            emit.asm.bind(next)?;
        }
        Ok(emit.asm.branch(Branch::Always, fallback)?)
    }

    /// The `(key, target)` pairs an integral `switch` jumps on.
    fn int_cases(arms: &[Arm]) -> Result<Vec<(i32, Label)>> {
        let mut cases = Vec::new();
        for arm in arms {
            for key in &arm.keys {
                let Key::Int(value) = key else {
                    return Err(LowerError::Unsupported("a `switch` mixing key types"));
                };
                cases.push((*value, arm.entry));
            }
        }
        Ok(cases)
    }

    /// A `String` switch: hash first, then confirm by `equals`.
    ///
    /// Two different strings can hash alike, so the hash only *narrows* the candidates — the switch
    /// arms of the hash table each test the actual strings that hash there, and a key whose hash
    /// matches but whose text does not falls through to `default`. Skipping the `equals` would send a
    /// colliding string to the wrong arm, which is a wrong answer rather than a crash.
    fn dispatch_text(
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: Label,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        // The selector is read three times — once for the hash and once per `equals` — so it is held
        // rather than re-evaluated.
        let held = emit.slots.declare_temporary(1);
        Expr::lower(selector, context, emit)?;
        emit.asm.store(held)?;

        // Group the labels by hash, keeping source order within a bucket.
        let mut buckets: Vec<(i32, Vec<(String, Label)>)> = Vec::new();
        for arm in arms {
            for key in &arm.keys {
                let Key::Text(text) = key else {
                    return Err(LowerError::Unsupported("a `switch` mixing key types"));
                };
                let hash = Self::java_hash(text);
                match buckets.iter_mut().find(|(existing, _)| *existing == hash) {
                    Some((_, entries)) => entries.push((text.clone(), arm.entry)),
                    None => buckets.push((hash, alloc::vec![(text.clone(), arm.entry)])),
                }
            }
        }

        let checks: Vec<(i32, Label)> = buckets
            .iter()
            .map(|(hash, _)| (*hash, emit.asm.label()))
            .collect();
        emit.asm.load(held)?;
        emit.asm
            .invoke_virtual("java/lang/String", "hashCode", "()I")?;
        emit.asm.switch(&checks, fallback)?;

        for ((_, entries), &(_, check)) in buckets.iter().zip(&checks) {
            emit.asm.bind(check)?;
            for (text, entry) in entries {
                emit.asm.load(held)?;
                emit.asm.const_string(text)?;
                emit.asm
                    .invoke_virtual("java/lang/String", "equals", "(Ljava/lang/Object;)Z")?;
                emit.asm.branch(Branch::IntZero(Compare::Ne), *entry)?;
            }
            // Every string in this bucket was rejected, so the key matches no arm.
            emit.asm.branch(Branch::Always, fallback)?;
        }
        Ok(())
    }

    /// `java.lang.String.hashCode()`: `s[0]*31^(n-1) + s[1]*31^(n-2) + … + s[n-1]`, over UTF-16 code
    /// units and wrapping at 32 bits.
    ///
    /// Specified, not incidental — `String.hashCode`'s contract fixes the algorithm, which is what
    /// lets a compiler build the table at all.
    fn java_hash(text: &str) -> i32 {
        text.encode_utf16().fold(0i32, |hash, unit| {
            hash.wrapping_mul(31).wrapping_add(i32::from(unit))
        })
    }

    /// A `case` label's constant value.
    fn key(value: &ast::Expr) -> Result<Key> {
        match value {
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(LowerError::Unsupported("a `case` with no value"))?;
                Self::key(&inner)
            }
            ast::Expr::Unary(unary) => {
                let operand = unary
                    .operand()
                    .ok_or(LowerError::Unsupported("a `case` with no value"))?;
                let Key::Int(inner) = Self::key(&operand)? else {
                    return Err(LowerError::Unsupported("a `case` this cannot evaluate"));
                };
                let signs: Vec<_> = unary
                    .syntax()
                    .children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .map(|token| token.kind())
                    .filter(|kind| !kind.is_trivia())
                    .collect();
                match signs.as_slice() {
                    [PLUS] => Ok(Key::Int(inner)),
                    [MINUS] => Ok(Key::Int(inner.wrapping_neg())),
                    _ => Err(LowerError::Unsupported("a `case` this cannot evaluate")),
                }
            }
            ast::Expr::Literal(literal) => Self::literal_key(literal),
            // A name is a constant only if it is a `static final` with a constant initialiser, and
            // resolving *that* means evaluating the initialiser of a member that may be in another
            // file. An enum constant is not a value expression at all — it names an arm by identity.
            _ => Err(LowerError::Unsupported("a non-literal `case`")),
        }
    }

    /// A literal `case` label: an integer, a character, or a string.
    fn literal_key(literal: &ast::Literal) -> Result<Key> {
        use jals_syntax::SyntaxKind::STRING_LITERAL;
        let token = literal
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
            .ok_or(LowerError::Unsupported("a `case` with no value"))?;
        match token.kind() {
            INT_LITERAL => {
                let value = Expr::integer_literal(token.text())?;
                i32::try_from(value)
                    .map(Key::Int)
                    .map_err(|_| LowerError::Unsupported("a `case` outside an `int`"))
            }
            CHAR_LITERAL => {
                let text = Expr::literal_text(token.text())?;
                let character = text
                    .chars()
                    .next()
                    .ok_or(LowerError::Unsupported("an empty character `case`"))?;
                Ok(Key::Int(character as i32))
            }
            STRING_LITERAL => Ok(Key::Text(Expr::literal_text(token.text())?)),
            _ => Err(LowerError::Unsupported("a `case` of this literal kind")),
        }
    }

    /// The colon form, which falls through from one group into the next.
    fn groups(
        groups: &[ast::SwitchGroup],
        arms: &[Arm],
        result: Option<&Ty>,
        done: Label,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        for (group, arm) in groups.iter().zip(arms) {
            emit.asm.bind(arm.entry)?;
            for statement in group.stmts() {
                Stmt::lower(&statement, context, emit)?;
            }
            // No jump: falling into the next group is what the colon form means, and a group that
            // wanted to stop said `break`.
        }
        // The last group falls out of the `switch`. In an expression that is a path with no value,
        // which the required `default` is there to prevent — but only for a *matched* key, so a
        // colon-form arm that neither yields nor throws is still reported.
        if result.is_some() && emit.asm.reachable() {
            return Err(LowerError::Unsupported(
                "a `switch` expression arm that yields nothing",
            ));
        }
        if emit.asm.reachable() {
            emit.asm.branch(Branch::Always, done)?;
        }
        Ok(())
    }

    /// The arrow form, where each arm stands alone.
    fn rules(
        rules: &[ast::SwitchRule],
        arms: &[Arm],
        result: Option<&Ty>,
        done: Label,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        for (rule, arm) in rules.iter().zip(arms) {
            emit.asm.bind(arm.entry)?;
            // Three body forms: an expression, a `throw`, or a block. In an expression `switch` the
            // first *is* the arm's value; in a statement one it is evaluated for its effect.
            if let Some(value) = rule.expr() {
                match result {
                    Some(result) => Expr::lower_as(&value, result, context, emit)?,
                    None => Stmt::discarded(&value, context, emit)?,
                }
            } else if let Some(block) = rule.syntax().children().find_map(ast::Block::cast) {
                Stmt::block(&block, context, emit)?;
            } else if let Some(thrown) = rule.syntax().children().find_map(ast::ThrowStmt::cast) {
                Stmt::lower(&ast::Stmt::Throw(thrown), context, emit)?;
            }
            if emit.asm.reachable() {
                if result.is_some() && rule.expr().is_none() {
                    // A block arm has to `yield`, and one that fell out of its own end produced no
                    // value for the expression to have.
                    return Err(LowerError::Unsupported(
                        "a `switch` expression arm that yields nothing",
                    ));
                }
                emit.asm.branch(Branch::Always, done)?;
            }
        }
        Ok(())
    }
}
