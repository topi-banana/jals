//! パーサ入力。字句解析の全トークン(トリビア含む)を保持しつつ、文法が見るのは
//! トリビアを除いた significant トークンの列だけにする。
//!
//! 文法は significant の位置(0 起点)で進み、トリビアの再付与は [`super::sink`] が
//! 全トークン列から行う。隣接判定([`Input::adjacent`])は `>` 系の合成に使う。

use alloc::borrow::Cow;
use alloc::vec::Vec;

use jals_exec::Yielder;
use text_size::TextRange;

use crate::lexer::{LexedToken, Lexer};
use crate::syntax_kind::SyntaxKind;

pub(crate) struct Input<'a> {
    /// 全トークン(トリビア含む)。lossless 復元に使う。
    all: Vec<LexedToken<'a>>,
    /// significant トークンの種別。
    sig_kinds: Vec<SyntaxKind>,
    /// significant トークンの範囲(隣接判定用)。
    sig_ranges: Vec<TextRange>,
    /// significant トークンの JLS §3.3 復号済みテキスト(文脈依存キーワード判定用)。
    ///
    /// エスケープを含まない大半のトークンは元の `&'a str` を借用したままで、復号が必要な
    /// ものだけが所有文字列になる。予約語は [`Lexer::tokenize`] が翻訳済みテキストから
    /// 種別を決めるので `\u0070ublic` は `PUBLIC_KW` になるのに、文脈依存キーワードは
    /// 生の綴りと比較していたため `\u0072ecord R(int x) {}` がエラーなしの `METHOD_DECL`
    /// になっていた。同じ一回のパースの中で語彙の半分だけが §3.3 に従っていたことになる。
    sig_texts: Vec<Cow<'a, str>>,
}

impl<'a> Input<'a> {
    pub(crate) async fn new(src: &'a str) -> Self {
        let all = Lexer::tokenize(src).await;
        let mut yielder = Yielder::new();
        let mut sig_kinds = Vec::new();
        let mut sig_ranges = Vec::new();
        let mut sig_texts = Vec::new();
        for t in &all {
            yielder.tick().await;
            if !t.kind.is_trivia() {
                sig_kinds.push(t.kind);
                sig_ranges.push(t.range);
                sig_texts.push(crate::unicode_escape::decode(t.text));
            }
        }
        Input {
            all,
            sig_kinds,
            sig_ranges,
            sig_texts,
        }
    }

    /// `sig_pos` 番目の significant トークンの種別。範囲外は [`SyntaxKind::EOF`]。
    pub(crate) fn kind(&self, sig_pos: usize) -> SyntaxKind {
        self.sig_kinds
            .get(sig_pos)
            .copied()
            .unwrap_or(SyntaxKind::EOF)
    }

    /// `sig_pos` 番目の significant トークンの復号済みテキスト(文脈依存キーワード判定用)。
    /// 範囲外は空。
    pub(crate) fn text(&self, sig_pos: usize) -> &str {
        self.sig_texts.get(sig_pos).map_or("", Cow::as_ref)
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
