//! `rowan` の [`Language`](rowan::Language) 実装と構文木の型エイリアス。
//!
//! [`SyntaxKind`] と `rowan` の u16 ベースの種別とを、`num-derive` が生成する
//! `from_u16` / `to_u16`(安全。`transmute` を使わない)で相互変換する。

use num_traits::{FromPrimitive, ToPrimitive};

use crate::syntax_kind::SyntaxKind;

/// jals が対象とする Java の言語定義(`rowan` 用のマーカ型)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JavaLanguage {}

impl rowan::Language for JavaLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        SyntaxKind::from_u16(raw.0).unwrap_or(SyntaxKind::ERROR)
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind.to_u16().expect("SyntaxKind は u16 に収まる"))
    }
}

/// 構文木のノード(`JavaLanguage` に特殊化した [`rowan::SyntaxNode`])。
pub type SyntaxNode = rowan::SyntaxNode<JavaLanguage>;
/// 構文木のトークン。
pub type SyntaxToken = rowan::SyntaxToken<JavaLanguage>;
/// ノードまたはトークン。
pub type SyntaxElement = rowan::SyntaxElement<JavaLanguage>;
