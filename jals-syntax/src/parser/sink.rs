//! イベント列と全トークン列から `rowan` の緑木([`GreenNode`])を組み立てる。
//!
//! トリビアはここで再付与する。方針はシンプルで、各 significant トークンを積む直前に、
//! その手前のトリビアをすべて積む。末尾(最後の significant トークン以降)のトリビアは
//! ルートノードを閉じる直前にまとめて積む。これにより全バイトが必ず木へ入り、
//! `node.text() == src`(lossless)が保証される。トリビアの「どのノードに属するか」という
//! 付与ポリシーは素朴だが、lossless であることを硬い不変条件とし、整形器の着手時に精密化する。

use alloc::string::String;
use alloc::vec::Vec;
use core::mem;

use rowan::{GreenNode, GreenNodeBuilder, Language};
use text_size::{TextRange, TextSize};

use crate::language::JavaLanguage;
use crate::lexer::LexedToken;
use crate::parser::event::Event;
use crate::parser::input::Input;
use crate::syntax_error::SyntaxError;
use crate::syntax_kind::SyntaxKind;

struct Sink<'a> {
    builder: GreenNodeBuilder<'static>,
    all: &'a [LexedToken<'a>],
    all_idx: usize,
    errors: Vec<SyntaxError>,
}

impl Sink<'_> {
    /// 次が significant になるまで、トリビアを木へ積む。
    fn eat_trivia(&mut self) {
        while let Some(t) = self.all.get(self.all_idx) {
            if !t.kind.is_trivia() {
                break;
            }
            self.builder
                .token(JavaLanguage::kind_to_raw(t.kind), t.text);
            self.all_idx += 1;
        }
    }

    /// significant トークンを1つ積む(手前のトリビアを先に積む)。
    fn token(&mut self, remap: Option<SyntaxKind>) {
        self.eat_trivia();
        let t = self.all[self.all_idx];
        let kind = remap.unwrap_or(t.kind);
        self.builder.token(JavaLanguage::kind_to_raw(kind), t.text);
        self.all_idx += 1;
    }

    fn start_node(&mut self, kind: SyntaxKind) {
        self.builder.start_node(JavaLanguage::kind_to_raw(kind));
    }

    fn finish_node(&mut self) {
        self.builder.finish_node();
    }

    /// 次に積むトークンの開始オフセット(エラー位置に使う)。
    fn current_offset(&self) -> TextSize {
        self.all.get(self.all_idx).map_or_else(
            || self.all.last().map_or(TextSize::new(0), |t| t.range.end()),
            |t| t.range.start(),
        )
    }

    fn error(&mut self, msg: String) {
        let offset = self.current_offset();
        self.errors
            .push(SyntaxError::new(msg, TextRange::empty(offset)));
    }
}

/// イベント列を処理して緑木とエラー一覧を得る。
pub(super) fn build(input: &Input, mut events: Vec<Event>) -> (GreenNode, Vec<SyntaxError>) {
    let mut sink = Sink {
        builder: GreenNodeBuilder::new(),
        all: input.all(),
        all_idx: 0,
        errors: Vec::new(),
    };
    let mut depth = 0i32;
    let mut forward_parents: Vec<Option<SyntaxKind>> = Vec::new();

    for i in 0..events.len() {
        match mem::replace(&mut events[i], Event::tombstone()) {
            // 墓石(破棄されたノード)は無視する。
            Event::Start {
                kind: None,
                forward_parent: None,
            } => {}
            Event::Start {
                kind,
                forward_parent,
            } => {
                // forward_parent チェーンを辿り、外側の親から順に開く。
                forward_parents.push(kind);
                let mut idx = i;
                let mut fp = forward_parent;
                while let Some(rel) = fp {
                    idx += rel;
                    fp = match mem::replace(&mut events[idx], Event::tombstone()) {
                        Event::Start {
                            kind,
                            forward_parent,
                        } => {
                            forward_parents.push(kind);
                            forward_parent
                        }
                        _ => unreachable!("forward_parent は Start を指す"),
                    };
                }
                for kind in forward_parents.into_iter().rev().flatten() {
                    sink.start_node(kind);
                    depth += 1;
                }
                forward_parents = Vec::new();
            }
            Event::Finish => {
                depth -= 1;
                // ルートを閉じる直前に末尾トリビアを積む(lossless 保証)。
                if depth == 0 {
                    sink.eat_trivia();
                }
                sink.finish_node();
            }
            Event::Token { remap } => sink.token(remap),
            Event::Error { msg } => sink.error(msg),
        }
    }

    let Sink {
        builder, errors, ..
    } = sink;
    (builder.finish(), errors)
}
