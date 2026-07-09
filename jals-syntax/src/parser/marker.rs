//! パース位置のマーカ。`rust-analyzer` 流の「開始位置を覚えておき、後から種別を確定する」方式。

use super::Parser;
use super::event::Event;
use crate::syntax_kind::SyntaxKind;

/// 開いたノードのマーカ。必ず [`complete`](Marker::complete) か [`abandon`](Marker::abandon)
/// で消費する(消費し忘れると `Drop` で panic)。
#[must_use]
pub(crate) struct Marker {
    pos: usize,
    completed: bool,
}

impl Marker {
    pub(super) const fn new(pos: usize) -> Self {
        Self {
            pos,
            completed: false,
        }
    }

    /// ノードを `kind` として閉じる。
    pub(crate) fn complete(mut self, p: &mut Parser, kind: SyntaxKind) -> CompletedMarker {
        self.completed = true;
        match &mut p.events[self.pos] {
            Event::Start { kind: slot, .. } => *slot = Some(kind),
            _ => unreachable!("Marker は Start イベントを指す"),
        }
        p.push_event(Event::Finish);
        CompletedMarker { pos: self.pos }
    }

    /// ノードを破棄する(木に現れない)。
    pub(crate) fn abandon(mut self, p: &mut Parser) {
        self.completed = true;
        // 末尾の墓石なら取り除く。途中のものは `sink` が墓石として無視する。
        if self.pos == p.events.len() - 1 {
            match p.events.pop() {
                Some(Event::Start {
                    kind: None,
                    forward_parent: None,
                }) => {}
                _ => unreachable!("abandon 対象は未完了の Start"),
            }
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        assert!(
            self.completed || currently_panicking(),
            "Marker は complete か abandon で消費しなければならない"
        );
    }
}

/// Whether the current thread is unwinding from a panic. Used to avoid a double-panic in
/// [`Marker`]'s drop guard. `std::thread::panicking` has no `core`/`alloc` equivalent, so under
/// `no_std` this degrades to `false`. That is safe in practice: the realized `no_std` target
/// (`wasm32`) aborts rather than unwinds on panic, so a `Marker` is never dropped mid-unwind there
/// and this guard could not observe an in-progress panic regardless.
#[cfg(test)]
fn currently_panicking() -> bool {
    std::thread::panicking()
}

#[cfg(not(test))]
const fn currently_panicking() -> bool {
    false
}

/// 完了したノードのマーカ。[`precede`](CompletedMarker::precede) で後から親で包める。
pub(crate) struct CompletedMarker {
    pos: usize,
}

impl CompletedMarker {
    /// この完了ノードを子として包む新しい親ノードを開く(左結合・優先順位登攀で使う)。
    pub(crate) fn precede(self, p: &mut Parser) -> Marker {
        let new_m = p.start();
        match &mut p.events[self.pos] {
            Event::Start { forward_parent, .. } => {
                *forward_parent = Some(new_m.index() - self.pos);
            }
            _ => unreachable!("CompletedMarker は Start イベントを指す"),
        }
        new_m
    }
}

impl Marker {
    /// イベント列内の開始位置。
    pub(super) const fn index(&self) -> usize {
        self.pos
    }
}
