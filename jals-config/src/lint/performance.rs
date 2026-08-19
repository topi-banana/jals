//! `[performance]` — the same meaning is available in a cheaper form.
//!
//! A finding here is a cost the code pays for nothing: an allocation with a cached alternative, a
//! copy with an in-place alternative. The section is deliberately small, because a rule belongs in
//! it only when the cheaper form is *always* cheaper — a claim that needs no benchmark of the
//! surrounding program to hold.

use super::NoOptions;

lint_section! {
    /// `[performance]` — needless cost with a drop-in cheaper spelling.
    Performance: Performance {
        /// `boxed-primitive-constructor` — `new Integer(1)`, `new Boolean(true)`, `new Double(x)`
        /// and their siblings, each of which allocates where `Integer.valueOf(1)` and its siblings
        /// return a cached instance. The Java analogue of `clippy::box_default`. The constructors
        /// have also been deprecated since Java 9, so the finding is a portability one too; it is
        /// filed here because the allocation is what it costs a program that keeps compiling.
        "boxed-primitive-constructor" => boxed_primitive_constructor: NoOptions = Warn,
    }
}
