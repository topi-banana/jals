//! The identity frontend.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_storage::ContentDigest;

use crate::frontend::{Frontend, FrontendCaps, FrontendFuture};
use crate::ir::{FrontendOutput, Ir};
use crate::level::IrLevel;

/// Emits every input file unchanged: the authored sources *are* the Java sources.
///
/// Unlike the builtin toolchain's copy — a placeholder that puts `.java` where `.class` belongs
/// and produces nothing runnable — identity is the correct and final semantics of "this project
/// uses no jals-specific language features". Vanilla is a supported mode, not a stand-in.
#[derive(Debug, Clone, Copy, Default)]
pub struct VanillaFrontend;

impl VanillaFrontend {
    pub const ID: &'static str = "vanilla";
}

impl Frontend for VanillaFrontend {
    fn caps(&self) -> FrontendCaps {
        FrontendCaps {
            id: Self::ID,
            needs: IrLevel::Bytes,
            extensions: &["java"],
            type_stable: true,
            version: 1,
        }
    }

    fn config_digest(&self) -> ContentDigest {
        // Vanilla has no configuration, so its config digest is a constant. It is still folded
        // into every key so that the key layout is identical to a configurable frontend's.
        ContentDigest::of(b"")
    }

    fn run<'a>(&'a self, ir: Ir<'a>) -> FrontendFuture<'a> {
        Box::pin(async move {
            let files = ir
                .files()
                .iter()
                .map(|file| (file.path.clone(), file.bytes.to_vec()))
                .collect::<Vec<_>>();
            Ok(FrontendOutput {
                files,
                diagnostics: Vec::new(),
                // Output offsets are input offsets, so an explicit origin map would be pure
                // redundancy. A rewriting frontend fills this in.
                origins: Vec::new(),
            })
        })
    }

    fn describe(&self, ir: &Ir<'_>) -> String {
        format!("copy {} source file(s) unchanged", ir.files().len())
    }
}
