//! Formatting configuration, deserialized from `jalsfmt.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. Keys use
//! kebab-case (e.g. `indent-style`, `max-blank-lines`).

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// How to render a single indentation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndentStyle {
    /// Indent with spaces (`indent-width` spaces per level).
    Space,
    /// Indent with a single tab per level.
    Tab,
}

/// The line terminator emitted by the formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
}

impl LineEnding {
    /// The terminator string for this line ending.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

/// Formatter style settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Spaces vs. tab for indentation.
    pub indent_style: IndentStyle,
    /// Number of columns per indentation level (and spaces emitted when `indent_style` is `Space`).
    pub indent_width: usize,
    /// Runs of blank lines are collapsed down to at most this many.
    pub max_blank_lines: usize,
    /// Line terminator to emit.
    pub line_ending: LineEnding,
    /// Ensure the output ends with exactly one newline.
    pub insert_final_newline: bool,
    /// Target line width for wrapping code.
    pub max_width: usize,
    /// Reflow comments so no line exceeds [`comment_width`](Config::comment_width).
    /// Off by default; [`comment_width`](Config::comment_width) has no effect unless this
    /// is enabled (mirrors rustfmt's `wrap_comments`).
    pub wrap_comments: bool,
    /// Target line width for reflowing comment / Javadoc prose, including indentation.
    /// Only consulted when [`wrap_comments`](Config::wrap_comments) is enabled.
    pub comment_width: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            indent_style: IndentStyle::Space,
            indent_width: 4,
            max_blank_lines: 1,
            line_ending: LineEnding::Lf,
            insert_final_newline: true,
            max_width: 100,
            wrap_comments: false,
            comment_width: 80,
        }
    }
}

impl Config {
    /// One indentation level rendered as a string.
    pub(crate) fn indent_unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Tab => "\t".to_string(),
            IndentStyle::Space => " ".repeat(self.indent_width),
        }
    }

    /// The number of display columns one indentation level occupies.
    pub(crate) fn indent_cols(&self) -> usize {
        self.indent_width.max(1)
    }

    /// The configured line terminator.
    pub(crate) fn newline(&self) -> &'static str {
        self.line_ending.as_str()
    }

    /// Load and parse a specific `jalsfmt.toml` file.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Search upward from `start_dir` for `jalsfmt.toml`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(start_dir: &Path) -> Result<Config, ConfigError> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = d.join("jalsfmt.toml");
            if candidate.is_file() {
                return Config::from_file(&candidate);
            }
            dir = d.parent();
        }
        Ok(Config::default())
    }
}

/// An error loading or parsing a config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read.
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The file contained invalid TOML.
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The underlying parse error.
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "failed to read config {}: {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse config {}: {source}", path.display())
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.indent_width, 4);
        assert_eq!(c.max_width, 100);
        assert_eq!(c.comment_width, 80);
        assert_eq!(c.max_blank_lines, 1);
        assert!(c.insert_final_newline);
        // Comment reflow is opt-in, mirroring rustfmt's `wrap_comments`.
        assert!(!c.wrap_comments);
    }

    #[test]
    fn wrap_comments_parses() {
        let c: Config = toml::from_str("wrap-comments = true\ncomment-width = 60\n").unwrap();
        assert!(c.wrap_comments);
        assert_eq!(c.comment_width, 60);
    }

    #[test]
    fn max_blank_lines_parses_kebab_key() {
        let c: Config = toml::from_str("max-blank-lines = 2\n").unwrap();
        assert_eq!(c.max_blank_lines, 2);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        let c: Config = toml::from_str("indent-width = 2\n").unwrap();
        assert_eq!(c.indent_width, 2);
        // untouched keys keep defaults
        assert_eq!(c.max_width, 100);
        assert_eq!(c.indent_style, IndentStyle::Space);
    }

    #[test]
    fn enums_parse_kebab_values() {
        let c: Config = toml::from_str("indent-style = \"tab\"\nline-ending = \"crlf\"\n").unwrap();
        assert_eq!(c.indent_style, IndentStyle::Tab);
        assert_eq!(c.line_ending, LineEnding::Crlf);
        assert_eq!(c.indent_unit(), "\t");
        assert_eq!(c.newline(), "\r\n");
    }

    #[test]
    fn space_indent_unit() {
        let c = Config {
            indent_width: 2,
            ..Config::default()
        };
        assert_eq!(c.indent_unit(), "  ");
    }
}
