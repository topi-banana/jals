//! Platform-dependent facts about the downloaded `jals` binary: the CI artifact name and the
//! executable file name for the host's OS/architecture, plus whether the extracted binary needs
//! its executable bit restored. Everything the rest of the extension needs to know about "which
//! platform am I on" lives here, resolved from `zed::current_platform()`.

use zed_extension_api::{self as zed, Architecture, Os, Result};

use crate::JALS_BINARY;

/// Platform-dependent naming for the downloaded `jals` binary.
pub(crate) struct Platform;

impl Platform {
    /// The name of the CI artifact holding this platform's `jals` binary
    /// (`jals-<target-triple>`, uploaded by the `lsp-binary` job in `.github/workflows/ci.yml`).
    pub(crate) fn artifact_name() -> Result<String> {
        let (os, arch) = zed::current_platform();
        let target = match (os, arch) {
            (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
            (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
            (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
            (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
            (Os::Windows, Architecture::X8664) => "x86_64-pc-windows-msvc",
            (os, arch) => {
                return Err(format!(
                    "no prebuilt `jals` for {os:?}/{arch:?}. Install it yourself (e.g. \
                     `cargo install --path jals-cli`) or set `lsp.jals.binary.path` in your \
                     Zed settings."
                ));
            }
        };
        Ok(format!("{JALS_BINARY}-{target}"))
    }

    /// The binary's file name inside an artifact (`jals.exe` on Windows).
    pub(crate) fn binary_file_name() -> String {
        match zed::current_platform().0 {
            Os::Windows => format!("{JALS_BINARY}.exe"),
            _ => JALS_BINARY.to_owned(),
        }
    }

    /// Whether the extracted binary needs its executable bit set (every OS except Windows — the
    /// artifact zip does not preserve the bit).
    pub(crate) fn needs_executable_bit() -> bool {
        zed::current_platform().0 != Os::Windows
    }
}
