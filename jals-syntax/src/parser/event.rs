//! パーサが生成する中間イベント列。
//!
//! 文法関数は木を直接組み立てず、まずイベント列を吐く。木の構築は [`super::sink`] が
//! 行う。これにより、トリビアの再付与や [`CompletedMarker::precede`](super::marker::CompletedMarker::precede)
//! による後付けの親ノード化(左結合・優先順位)を、文法をトリビア非依存に保ったまま実現できる。

use alloc::string::String;

use crate::syntax_kind::SyntaxKind;

/// 木構築のための1イベント。
pub enum Event {
    /// ノード開始。`kind` が `None` の間は墓石(tombstone)で、木には現れない。
    /// `forward_parent` は、このノードを包む親 `Start` への(イベント列内の)相対距離。
    Start {
        kind: Option<SyntaxKind>,
        forward_parent: Option<usize>,
    },
    /// 直近に開始したノードを閉じる。
    Finish,
    /// significant トークンを1つ消費する。`remap` があればその種別で木に積む
    /// (文脈依存キーワードの昇格など。テキストは元のまま)。
    Token { remap: Option<SyntaxKind> },
    /// エラーを記録する(木の構造は変えない)。
    Error { msg: String },
}

impl Event {
    /// 墓石イベント(まだ種別が確定していない `Start`)。
    pub(crate) const fn tombstone() -> Self {
        Self::Start {
            kind: None,
            forward_parent: None,
        }
    }
}
