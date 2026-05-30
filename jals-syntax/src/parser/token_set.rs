//! [`SyntaxKind`] の集合をビットセットで表す。回復集合(recovery set)などで使う。

use crate::syntax_kind::SyntaxKind;

/// `SyntaxKind` の小さな集合。`u128` 2 本で最大 256 種別まで表せる。
#[derive(Clone, Copy)]
pub(crate) struct TokenSet([u128; 2]);

impl TokenSet {
    /// 種別の列から集合を作る。
    pub(crate) const fn new(kinds: &[SyntaxKind]) -> TokenSet {
        let mut bits = [0u128; 2];
        let mut i = 0;
        while i < kinds.len() {
            let v = kinds[i] as usize;
            bits[v / 128] |= 1u128 << (v % 128);
            i += 1;
        }
        TokenSet(bits)
    }

    /// `kind` を含むか。
    pub(crate) const fn contains(&self, kind: SyntaxKind) -> bool {
        let v = kind as usize;
        self.0[v / 128] & (1u128 << (v % 128)) != 0
    }
}
