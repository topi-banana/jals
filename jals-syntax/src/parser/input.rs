//! パーサ入力。字句解析の全トークン(トリビア含む)を保持しつつ、文法が見るのは
//! トリビアを除いた significant トークンの列だけにする。
//!
//! 文法は significant の位置(0 起点)で進み、トリビアの再付与は [`super::sink`] が
//! 全トークン列から行う。隣接判定([`Input::adjacent`])は `>` 系の合成に使う。

use alloc::vec::Vec;

use text_size::TextRange;

use crate::lexer::{LexedToken, tokenize};
use crate::syntax_kind::SyntaxKind;

pub struct Input<'a> {
    /// 全トークン(トリビア含む)。lossless 復元に使う。
    all: Vec<LexedToken<'a>>,
    /// significant トークンの種別。
    sig_kinds: Vec<SyntaxKind>,
    /// significant トークンの範囲(隣接判定用)。
    sig_ranges: Vec<TextRange>,
    /// `sig_*[i]` に対応する `all` 内のインデックス。
    sig_to_all: Vec<usize>,
}

impl<'a> Input<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        let all = tokenize(src);
        let mut sig_kinds = Vec::new();
        let mut sig_ranges = Vec::new();
        let mut sig_to_all = Vec::new();
        for (i, t) in all.iter().enumerate() {
            if !t.kind.is_trivia() {
                sig_kinds.push(t.kind);
                sig_ranges.push(t.range);
                sig_to_all.push(i);
            }
        }
        Input {
            all,
            sig_kinds,
            sig_ranges,
            sig_to_all,
        }
    }

    /// `sig_pos` 番目の significant トークンの種別。範囲外は [`SyntaxKind::EOF`]。
    pub(crate) fn kind(&self, sig_pos: usize) -> SyntaxKind {
        self.sig_kinds
            .get(sig_pos)
            .copied()
            .unwrap_or(SyntaxKind::EOF)
    }

    /// `sig_pos` 番目の significant トークンのテキスト(文脈依存キーワード判定用)。範囲外は空。
    pub(crate) fn text(&self, sig_pos: usize) -> &'a str {
        self.sig_to_all
            .get(sig_pos)
            .map_or("", |&i| self.all[i].text)
    }

    /// `sig_pos` 番目と `sig_pos + 1` 番目の significant トークンが隣接しているか
    /// (間にトリビアがない)。`>>` などの合成に使う。範囲外は `false`。
    pub(crate) fn adjacent(&self, sig_pos: usize) -> bool {
        match (
            self.sig_ranges.get(sig_pos),
            self.sig_ranges.get(sig_pos + 1),
        ) {
            (Some(a), Some(b)) => a.end() == b.start(),
            _ => false,
        }
    }

    /// 全トークン(トリビア含む)。
    pub(crate) fn all(&self) -> &[LexedToken<'a>] {
        &self.all
    }
}
