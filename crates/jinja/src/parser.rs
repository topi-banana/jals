//! Source text to [`Node`]s.
//!
//! Three passes, each of which is a whole answer to one question: [`Scan`] finds the tags,
//! [`Lower`] turns the source plus those tags into a flat run of text and tags with the whitespace
//! rule applied, and [`Parser`] folds that run into a tree. Splitting the whitespace rule out is
//! what keeps it one rule rather than a condition repeated at every tag site.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast::{Arm, CompareOp, Expr, Node};
use crate::error::{Error, ErrorKind, Position};
use crate::value::Value;

/// A parsed template body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ast {
    pub(crate) nodes: Vec<Node>,
}

impl Ast {
    /// How deeply blocks may nest before the parser refuses rather than recurses.
    ///
    /// Malformed input must never take the process down with it, and the tree builder is
    /// recursive, so the depth is capped where the recursion is rather than trusted to be shallow.
    const MAX_DEPTH: u32 = 64;

    /// Parse one template.
    ///
    /// `trim_block_lines` is the whitespace rule described on
    /// [`Environment::set_trim_block_lines`](crate::Environment::set_trim_block_lines); it applies
    /// here rather than at render time, because it is a fact about the source.
    pub(crate) fn parse(source: &str, trim_block_lines: bool) -> Result<Self, Error> {
        let tags = Scan::tags(source)?;
        let mut parser = Parser {
            items: Lower::items(source, &tags, trim_block_lines),
            index: 0,
        };
        let (nodes, terminator) = parser.nodes(0)?;
        if let Some((directive, at)) = terminator {
            return Err(Error::new(
                ErrorKind::SyntaxError,
                format!("`{}` has no block to close", directive.name()),
            )
            .at(at));
        }
        Ok(Self { nodes })
    }
}

/// Which delimiter pair a tag is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    /// `{{ … }}`, which never affects the whitespace around it.
    Emit,
    /// `{% … %}`.
    Block,
    /// `{# … #}`, dropped entirely.
    Comment,
}

impl TagKind {
    const fn opener(self) -> &'static str {
        match self {
            Self::Emit => "{{",
            Self::Block => "{%",
            Self::Comment => "{#",
        }
    }

    const fn closer(self) -> [u8; 2] {
        match self {
            Self::Emit => *b"}}",
            Self::Block => *b"%}",
            Self::Comment => *b"#}",
        }
    }
}

/// One tag as it sits in the source, before the whitespace rule widens it.
#[derive(Debug, Clone)]
struct RawTag {
    start: usize,
    end: usize,
    at: Position,
    kind: TagKind,
    body: String,
}

/// The tag scanner.
struct Scan;

impl Scan {
    /// Every tag in the source, in order, with the position of its opening delimiter.
    fn tags(source: &str) -> Result<Vec<RawTag>, Error> {
        let bytes = source.as_bytes();
        let mut tags = Vec::new();
        let mut at = Position::START;
        let mut index = 0usize;
        while index < bytes.len() {
            let kind = if bytes[index] == b'{' {
                match bytes.get(index + 1) {
                    Some(b'{') => Some(TagKind::Emit),
                    Some(b'%') => Some(TagKind::Block),
                    Some(b'#') => Some(TagKind::Comment),
                    _ => None,
                }
            } else {
                None
            };
            let Some(kind) = kind else {
                // A lone `{` is never a delimiter, which is what lets a JSON or XML document be
                // written exactly as it is read.
                let Some(ch) = source[index..].chars().next() else {
                    break;
                };
                at.advance(ch);
                index += ch.len_utf8();
                continue;
            };
            let body_end = Self::close(source, index + 2, kind).ok_or_else(|| {
                Error::new(
                    ErrorKind::SyntaxError,
                    format!("`{}` is never closed", kind.opener()),
                )
                .at(at)
            })?;
            let end = body_end + 2;
            tags.push(RawTag {
                start: index,
                end,
                at,
                kind,
                body: source[index + 2..body_end].trim().to_owned(),
            });
            for ch in source[index..end].chars() {
                at.advance(ch);
            }
            index = end;
        }
        Ok(tags)
    }

    /// Where this tag's closing delimiter starts, skipping over string literals.
    ///
    /// The scan has to know about strings: `{{ "}}" }}` is how a template writes a literal `}}`,
    /// and a plain search for the closer would end the tag in the middle of it.
    fn close(source: &str, from: usize, kind: TagKind) -> Option<usize> {
        let bytes = source.as_bytes();
        let closer = kind.closer();
        let mut index = from;
        let mut in_string = false;
        while index < bytes.len() {
            if kind != TagKind::Comment && bytes[index] == b'"' {
                in_string = !in_string;
                index += 1;
                continue;
            }
            if in_string {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
                continue;
            }
            if bytes[index] == closer[0] && bytes.get(index + 1) == Some(&closer[1]) {
                return Some(index);
            }
            index += 1;
        }
        None
    }
}

/// The flat sequence a template parses from: text and tags, comments already dropped.
#[derive(Debug, Clone)]
enum Item {
    Text(String),
    Emit { body: String, at: Position },
    Block { body: String, at: Position },
}

/// The pass that owns the whitespace rule.
struct Lower;

impl Lower {
    /// Split the source into text and tags, applying the whole-line rule as it goes.
    fn items(source: &str, tags: &[RawTag], trim_block_lines: bool) -> Vec<Item> {
        let mut items = Vec::new();
        let mut cursor = 0usize;
        for tag in tags {
            let (start, end) = if trim_block_lines {
                Self::span(source, tag)
            } else {
                (tag.start, tag.end)
            };
            let start = start.max(cursor);
            if start > cursor {
                items.push(Item::Text(source[cursor..start].to_owned()));
            }
            cursor = end.max(cursor);
            match tag.kind {
                TagKind::Comment => {}
                TagKind::Emit => items.push(Item::Emit {
                    body: tag.body.clone(),
                    at: tag.at,
                }),
                TagKind::Block => items.push(Item::Block {
                    body: tag.body.clone(),
                    at: tag.at,
                }),
            }
        }
        if cursor < source.len() {
            items.push(Item::Text(source[cursor..].to_owned()));
        }
        items
    }

    /// A tag's effective span: widened over its own line when it is the only thing on it.
    ///
    /// Both sides have to be blank for the rule to apply, so two tags sharing a line keep every
    /// byte between them. An emitting tag is never widened — a value written into the middle of a
    /// document is not a directive, and taking its line would delete text nobody wrote a tag for.
    fn span(source: &str, tag: &RawTag) -> (usize, usize) {
        if tag.kind == TagKind::Emit {
            return (tag.start, tag.end);
        }
        let bytes = source.as_bytes();
        let mut start = tag.start;
        while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        if start > 0 && bytes[start - 1] != b'\n' {
            return (tag.start, tag.end);
        }
        let mut end = tag.end;
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
        if end < bytes.len() {
            if bytes[end] == b'\n' {
                end += 1;
            } else if bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n') {
                end += 2;
            } else {
                return (tag.start, tag.end);
            }
        }
        (start, end)
    }
}

/// The tree builder over the flat item sequence.
struct Parser {
    items: Vec<Item>,
    index: usize,
}

impl Parser {
    /// Nodes up to the first tag this level does not own, which is handed back to the caller.
    fn nodes(&mut self, depth: u32) -> Result<(Vec<Node>, Option<(Directive, Position)>), Error> {
        let mut nodes = Vec::new();
        while let Some(item) = self.items.get(self.index).cloned() {
            self.index += 1;
            match item {
                Item::Text(text) => nodes.push(Node::Text(text)),
                Item::Emit { body, at } => nodes.push(Node::Emit {
                    expr: ExprParser::parse(&body, at)?,
                    at,
                }),
                Item::Block { body, at } => match Directive::parse(&body, at)? {
                    Directive::If(condition) => nodes.push(self.if_node(condition, at, depth)?),
                    Directive::For { binding, source } => {
                        nodes.push(self.for_node(binding, source, at, depth)?);
                    }
                    Directive::Include(name) => nodes.push(Node::Include { name, at }),
                    other => return Ok((nodes, Some((other, at)))),
                },
            }
        }
        Ok((nodes, None))
    }

    fn if_node(&mut self, condition: Expr, at: Position, depth: u32) -> Result<Node, Error> {
        Self::guard_depth(depth, at)?;
        let mut arms = Vec::new();
        let mut condition = condition;
        let mut condition_at = at;
        loop {
            let (body, terminator) = self.nodes(depth + 1)?;
            let Some((directive, tag_at)) = terminator else {
                return Err(Self::unclosed("if", at));
            };
            match directive {
                Directive::Elif(next) => {
                    arms.push(Arm {
                        condition,
                        at: condition_at,
                        body,
                    });
                    condition = next;
                    condition_at = tag_at;
                }
                Directive::Else => {
                    arms.push(Arm {
                        condition,
                        at: condition_at,
                        body,
                    });
                    let (otherwise, terminator) = self.nodes(depth + 1)?;
                    let Some((directive, tag_at)) = terminator else {
                        return Err(Self::unclosed("if", at));
                    };
                    if !matches!(directive, Directive::EndIf) {
                        return Err(Self::unexpected(&directive, tag_at));
                    }
                    return Ok(Node::If { arms, otherwise });
                }
                Directive::EndIf => {
                    arms.push(Arm {
                        condition,
                        at: condition_at,
                        body,
                    });
                    return Ok(Node::If {
                        arms,
                        otherwise: Vec::new(),
                    });
                }
                other => return Err(Self::unexpected(&other, tag_at)),
            }
        }
    }

    fn for_node(
        &mut self,
        binding: String,
        source: Expr,
        at: Position,
        depth: u32,
    ) -> Result<Node, Error> {
        Self::guard_depth(depth, at)?;
        let (body, terminator) = self.nodes(depth + 1)?;
        let Some((directive, tag_at)) = terminator else {
            return Err(Self::unclosed("for", at));
        };
        if !matches!(directive, Directive::EndFor) {
            return Err(Self::unexpected(&directive, tag_at));
        }
        Ok(Node::For {
            binding,
            source,
            at,
            body,
        })
    }

    fn guard_depth(depth: u32, at: Position) -> Result<(), Error> {
        if depth >= Ast::MAX_DEPTH {
            return Err(Error::new(ErrorKind::SyntaxError, "blocks are nested too deeply").at(at));
        }
        Ok(())
    }

    fn unclosed(tag: &'static str, at: Position) -> Error {
        Error::new(ErrorKind::SyntaxError, format!("`{tag}` is never closed")).at(at)
    }

    fn unexpected(directive: &Directive, at: Position) -> Error {
        Error::new(
            ErrorKind::SyntaxError,
            format!("`{}` has no block to close", directive.name()),
        )
        .at(at)
    }
}

/// What a `{% … %}` tag says.
#[derive(Debug, Clone, PartialEq)]
enum Directive {
    If(Expr),
    Elif(Expr),
    Else,
    EndIf,
    For { binding: String, source: Expr },
    EndFor,
    Include(Expr),
}

impl Directive {
    fn parse(body: &str, at: Position) -> Result<Self, Error> {
        let (keyword, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        let rest = rest.trim();
        match keyword {
            "if" => Ok(Self::If(ExprParser::parse(rest, at)?)),
            "elif" => Ok(Self::Elif(ExprParser::parse(rest, at)?)),
            "include" => Ok(Self::Include(ExprParser::parse(rest, at)?)),
            "for" => {
                let (binding, source) = rest.split_once(" in ").ok_or_else(|| {
                    Error::new(
                        ErrorKind::SyntaxError,
                        "expected `for <name> in <expression>`",
                    )
                    .at(at)
                })?;
                let binding = binding.trim();
                if !ExprParser::is_name(binding) {
                    return Err(Error::new(
                        ErrorKind::SyntaxError,
                        "the loop variable must be a plain name",
                    )
                    .at(at));
                }
                // A binding the expression grammar already spells something else can be written but
                // never read: `{% for none in xs %}{{ none }}{% endfor %}` binds each item and then
                // writes the literal, and `loop` is shadowed by the namespace the body reads. Both
                // render successfully and both render the wrong bytes, so they are refused here.
                if ExprParser::is_reserved(binding) {
                    return Err(Error::new(
                        ErrorKind::SyntaxError,
                        format!(
                            "`{binding}` already means something in an expression, so it cannot be \
                             a loop variable"
                        ),
                    )
                    .at(at));
                }
                Ok(Self::For {
                    binding: binding.to_owned(),
                    source: ExprParser::parse(source.trim(), at)?,
                })
            }
            "else" | "endif" | "endfor" if !rest.is_empty() => {
                Err(Error::new(ErrorKind::SyntaxError, "this tag takes no expression").at(at))
            }
            "else" => Ok(Self::Else),
            "endif" => Ok(Self::EndIf),
            "endfor" => Ok(Self::EndFor),
            other => {
                Err(Error::new(ErrorKind::SyntaxError, format!("unknown tag `{other}`")).at(at))
            }
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::If(_) => "if",
            Self::Elif(_) => "elif",
            Self::Else => "else",
            Self::EndIf => "endif",
            Self::For { .. } => "for",
            Self::EndFor => "endfor",
            Self::Include(_) => "include",
        }
    }
}

/// One lexed piece of an expression.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Name(String),
    Str(String),
    Int(i64),
    Float(f64),
    Dot,
    OpenBracket,
    CloseBracket,
    OpenParen,
    CloseParen,
    Pipe,
    Comma,
    Compare(CompareOp),
}

/// The expression parser, which also owns the expression lexer.
struct ExprParser {
    tokens: Vec<Token>,
    index: usize,
    at: Position,
}

impl ExprParser {
    /// Parse one expression, which must consume the whole of `source`.
    fn parse(source: &str, at: Position) -> Result<Expr, Error> {
        let mut parser = Self {
            tokens: Self::tokens(source, at)?,
            index: 0,
            at,
        };
        if parser.tokens.is_empty() {
            return Err(parser.malformed("expected an expression"));
        }
        let expr = parser.any()?;
        if parser.index != parser.tokens.len() {
            return Err(parser.malformed("the expression has trailing tokens"));
        }
        Ok(expr)
    }

    /// The names an expression already spells for itself, which is what a loop binding may not be.
    ///
    /// `true`/`false`/`none` are literals and the three operators are operators, so a binding with
    /// one of those names is shadowed at every use site; `loop` is the namespace a `{% for %}` body
    /// reads, pushed after the binding and therefore found first.
    const RESERVED: [&'static str; 7] = ["and", "or", "not", "true", "false", "none", "loop"];

    /// Whether this name is one of [`Self::RESERVED`].
    fn is_reserved(text: &str) -> bool {
        Self::RESERVED.contains(&text)
    }

    /// Whether this is a bare identifier, which is what a loop binding has to be.
    fn is_name(text: &str) -> bool {
        let mut chars = text.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }

    fn tokens(source: &str, at: Position) -> Result<Vec<Token>, Error> {
        let mut tokens = Vec::new();
        let mut chars = source.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                ch if ch.is_whitespace() => {}
                '.' => tokens.push(Token::Dot),
                '[' => tokens.push(Token::OpenBracket),
                ']' => tokens.push(Token::CloseBracket),
                '(' => tokens.push(Token::OpenParen),
                ')' => tokens.push(Token::CloseParen),
                ',' => tokens.push(Token::Comma),
                '|' => tokens.push(Token::Pipe),
                '=' | '!' | '<' | '>' => {
                    tokens.push(Self::compare(ch, &mut chars, at)?);
                }
                '"' => tokens.push(Token::Str(Self::string(&mut chars, at)?)),
                // A field name may begin with a digit — `features.8` and `features.1_20` are
                // feature names, not numbers — so after a `.` a digit starts a name. Everywhere
                // else it starts a literal, which is what keeps `1.5` and `items[0]` numbers.
                ch if ch.is_ascii_digit() && tokens.last() == Some(&Token::Dot) => {
                    tokens.push(Token::Name(Self::word(ch, &mut chars)));
                }
                ch if ch.is_ascii_digit() => {
                    Self::number(&mut tokens, ch, &mut chars, at)?;
                }
                ch if ch.is_ascii_alphabetic() || ch == '_' => {
                    tokens.push(Token::Name(Self::word(ch, &mut chars)));
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::SyntaxError,
                        "unexpected character in the expression",
                    )
                    .at(at));
                }
            }
        }
        Ok(tokens)
    }

    /// One identifier, from its first character to the last one that can continue it.
    fn word(first: char, chars: &mut core::iter::Peekable<core::str::Chars<'_>>) -> String {
        let mut name = String::new();
        name.push(first);
        while let Some(next) = chars.peek() {
            if next.is_ascii_alphanumeric() || *next == '_' {
                name.push(*next);
                chars.next();
            } else {
                break;
            }
        }
        name
    }

    /// A comparison operator. `=` and `!` exist only as the first half of one, so a lone one is
    /// rejected here rather than becoming a token nothing accepts.
    fn compare(
        first: char,
        chars: &mut core::iter::Peekable<core::str::Chars<'_>>,
        at: Position,
    ) -> Result<Token, Error> {
        let paired = chars.peek() == Some(&'=');
        if paired {
            chars.next();
        }
        match (first, paired) {
            ('=', true) => Ok(Token::Compare(CompareOp::Eq)),
            ('!', true) => Ok(Token::Compare(CompareOp::Ne)),
            ('<', true) => Ok(Token::Compare(CompareOp::Le)),
            ('>', true) => Ok(Token::Compare(CompareOp::Ge)),
            ('<', false) => Ok(Token::Compare(CompareOp::Lt)),
            ('>', false) => Ok(Token::Compare(CompareOp::Gt)),
            _ => Err(Error::new(
                ErrorKind::SyntaxError,
                "expected `==` or `!=`; a single `=` assigns nothing here",
            )
            .at(at)),
        }
    }

    fn string(
        chars: &mut core::iter::Peekable<core::str::Chars<'_>>,
        at: Position,
    ) -> Result<String, Error> {
        let mut text = String::new();
        loop {
            let Some(ch) = chars.next() else {
                return Err(
                    Error::new(ErrorKind::SyntaxError, "a string literal is never closed").at(at),
                );
            };
            match ch {
                '"' => return Ok(text),
                '\\' => match chars.next() {
                    Some(escaped @ ('"' | '\\')) => text.push(escaped),
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    _ => {
                        return Err(Error::new(
                            ErrorKind::SyntaxError,
                            "only `\\\"`, `\\\\`, `\\n` and `\\t` are escapes",
                        )
                        .at(at));
                    }
                },
                ch => text.push(ch),
            }
        }
    }

    /// One number literal, appended to `tokens` — plural, because a trailing `.` is handed back as
    /// the `Dot` a field access needs rather than swallowed.
    fn number(
        tokens: &mut Vec<Token>,
        first: char,
        chars: &mut core::iter::Peekable<core::str::Chars<'_>>,
        at: Position,
    ) -> Result<(), Error> {
        let mut text = String::new();
        text.push(first);
        let mut float = false;
        while let Some(next) = chars.peek() {
            if next.is_ascii_digit() {
                text.push(*next);
            } else if *next == '.' && !float {
                float = true;
                text.push('.');
            } else {
                break;
            }
            chars.next();
        }
        // A trailing `.` is a field access on an integer, not part of the number, so the digits
        // stop here and the `.` is re-emitted as the `Dot` the accessor needs.
        let trailing_dot = float && text.ends_with('.');
        if trailing_dot {
            text.pop();
            float = false;
        }
        // `f64::from_str` answers `Ok(inf)` for a magnitude it cannot hold, so a literal that does
        // not fit has to be refused here — the integer path already refuses the same magnitude, and
        // writing `inf` into a rendered document is the silent wrong answer.
        let parsed = if float {
            text.parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(Token::Float)
        } else {
            text.parse::<i64>().ok().map(Token::Int)
        };
        let token = parsed
            .ok_or_else(|| Error::new(ErrorKind::SyntaxError, "this number does not fit").at(at))?;
        tokens.push(token);
        if trailing_dot {
            tokens.push(Token::Dot);
        }
        Ok(())
    }

    fn any(&mut self) -> Result<Expr, Error> {
        let mut left = self.all()?;
        while self.eat_name("or") {
            let right = self.all()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn all(&mut self) -> Result<Expr, Error> {
        let mut left = self.negated()?;
        while self.eat_name("and") {
            let right = self.negated()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `not` is a level of its own, **above** the comparison rather than inside it.
    ///
    /// Jinja's precedence is `or` < `and` < `not` < comparison < filter, so `not a == b` denies the
    /// comparison. Folding `not` into the operand level instead reads `(not a) == b`, which for two
    /// values of different shapes is `false` whatever they hold — the wrong arm, silently, with no
    /// error to notice it by.
    fn negated(&mut self) -> Result<Expr, Error> {
        if self.eat_name("not") {
            return Ok(Expr::Not(Box::new(self.negated()?)));
        }
        self.compared()
    }

    /// Comparisons do not chain: `a < b < c` is a mistake far more often than it is a request, and
    /// Jinja's own answer to it is not one a reader would predict.
    fn compared(&mut self) -> Result<Expr, Error> {
        let left = self.unary()?;
        let Some(Token::Compare(op)) = self.tokens.get(self.index).cloned() else {
            return Ok(left);
        };
        self.index += 1;
        let right = self.unary()?;
        if matches!(self.tokens.get(self.index), Some(Token::Compare(_))) {
            return Err(self.malformed("comparisons do not chain; write `and` between them"));
        }
        Ok(Expr::Compare {
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    /// One operand: a primary with its accesses, then every `|` filter folded onto it. Filters bind
    /// tighter than everything above, which is why they are here and `not` is not.
    fn unary(&mut self) -> Result<Expr, Error> {
        let (mut value, _) = self.postfix()?;
        while self.eat(&Token::Pipe) {
            value = self.filter(value)?;
        }
        Ok(value)
    }

    fn filter(&mut self, value: Expr) -> Result<Expr, Error> {
        let Some(Token::Name(name)) = self.next() else {
            return Err(self.malformed("expected a filter name after `|`"));
        };
        let mut args = Vec::new();
        if self.eat(&Token::OpenParen) && !self.eat(&Token::CloseParen) {
            loop {
                args.push(self.any()?);
                if self.eat(&Token::Comma) {
                    continue;
                }
                if self.eat(&Token::CloseParen) {
                    break;
                }
                return Err(self.malformed("expected `,` or `)` in the filter arguments"));
            }
        }
        Ok(Expr::Filter {
            name,
            value: Box::new(value),
            args,
        })
    }

    /// A primary with every `.name` and `[…]` access folded onto it, beside how it was spelled.
    fn postfix(&mut self) -> Result<(Expr, Option<String>), Error> {
        let (mut value, mut path) = self.primary()?;
        loop {
            if self.eat(&Token::Dot) {
                let Some(Token::Name(field)) = self.next() else {
                    return Err(self.malformed("expected a name after `.`"));
                };
                let base = path.clone();
                path = path.map(|path| format!("{path}.{field}"));
                value = Expr::Get {
                    base: Box::new(value),
                    key: Box::new(Expr::Const(Value::from(field.as_str()))),
                    path: base,
                };
            } else if self.eat(&Token::OpenBracket) {
                let key = self.any()?;
                if !self.eat(&Token::CloseBracket) {
                    return Err(self.malformed("expected `]`"));
                }
                let base = path.clone();
                // The bracket spelling is not a convenience: a name may be `1.20.1` or
                // `mixin-extras`, neither of which is a name `a.b` can carry — so the spelling an
                // error shows keeps the literal that was written.
                let literal = match &key {
                    Expr::Const(literal) => literal.as_str(),
                    _ => None,
                };
                path = path.map(|path| {
                    literal.map_or_else(
                        || format!("{path}[…]"),
                        |text| format!("{path}[\"{text}\"]"),
                    )
                });
                value = Expr::Get {
                    base: Box::new(value),
                    key: Box::new(key),
                    path: base,
                };
            } else {
                return Ok((value, path));
            }
        }
    }

    fn primary(&mut self) -> Result<(Expr, Option<String>), Error> {
        match self.next() {
            Some(Token::OpenParen) => {
                let inner = self.any()?;
                if !self.eat(&Token::CloseParen) {
                    return Err(self.malformed("expected `)`"));
                }
                Ok((inner, None))
            }
            Some(Token::Str(text)) => Ok((Expr::Const(Value::from(text)), None)),
            Some(Token::Int(value)) => Ok((Expr::Const(Value::from(value)), None)),
            Some(Token::Float(value)) => Ok((Expr::Const(Value::from(value)), None)),
            Some(Token::Name(name)) => match name.as_str() {
                "and" | "or" | "not" => Err(self.malformed("expected a value")),
                "true" => Ok((Expr::Const(Value::from(true)), None)),
                "false" => Ok((Expr::Const(Value::from(false)), None)),
                "none" => Ok((Expr::Const(Value::NONE), None)),
                _ => Ok((Expr::Var { name: name.clone() }, Some(name))),
            },
            _ => Err(self.malformed("expected a value")),
        }
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.index).cloned();
        if token.is_some() {
            self.index += 1;
        }
        token
    }

    fn eat(&mut self, token: &Token) -> bool {
        let found = self.tokens.get(self.index) == Some(token);
        if found {
            self.index += 1;
        }
        found
    }

    fn eat_name(&mut self, name: &str) -> bool {
        let found =
            matches!(self.tokens.get(self.index), Some(Token::Name(found)) if found == name);
        if found {
            self.index += 1;
        }
        found
    }

    fn malformed(&self, reason: &'static str) -> Error {
        Error::new(ErrorKind::SyntaxError, reason).at(self.at)
    }
}
