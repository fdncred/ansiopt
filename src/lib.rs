//! Shrinks strings containing ANSI escape sequences without changing what a
//! terminal shows.
//!
//! The input is split into tokens with their raw bytes ([`lexer`]). Graphic
//! rendition, hyperlinks and character sets are tracked as state and only
//! written when something visible depends on them, using the shortest
//! encoding of the difference ([`sgr`]). Cursor movements are merged, mode
//! changes batched, defaults dropped. Everything else passes through byte for
//! byte.
//!
//! ```
//! let out = ansiopt::minify(b"\x1b[0m\x1b[1m\x1b[31mhi\x1b[0m\x1b[0m", &Default::default());
//! assert_eq!(out, b"\x1b[;1;31mhi\x1b[m");
//! ```

pub mod lexer;
mod optimizer;
pub mod sgr;

pub use optimizer::Minifier;

/// Controls which transformations are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Assume the terminal starts with default attributes, no hyperlink and
    /// the default character sets. Without this the starting state is
    /// treated as unknown, so the output behaves like the input whatever was
    /// printed before it.
    pub assume_reset: bool,
    /// Preserve only what is visible: attributes that cannot show on blank
    /// cells (foreground color, bold, italic, ...) are not refreshed for
    /// spaces, newlines and erased areas unless reverse video is involved.
    pub visual: bool,
    /// Replace runs of a repeated character with REP (`CSI n b`). Most modern
    /// terminals support it, but not all.
    pub rep: bool,
    /// Avoid shorthand that very old terminals may not accept: empty
    /// parameters such as `ESC[;1m`, and BEL in place of ST.
    pub compat: bool,
    /// Maximum parameters per CSI sequence the optimizer writes. Longer
    /// sequences are split. Sequences in the input that are longer raise the
    /// limit, since the target terminal evidently accepts them.
    pub max_params: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            assume_reset: false,
            visual: false,
            rep: false,
            compat: false,
            max_params: 16,
        }
    }
}

/// Minifies a complete stream.
///
/// The result is never longer than the input; if optimizing would not help,
/// the input is returned unchanged.
pub fn minify(input: &[u8], options: &Options) -> Vec<u8> {
    let mut minifier = Minifier::new(*options);
    let mut out = minifier.feed(input);
    out.extend(minifier.finish());
    if out.len() > input.len() {
        return input.to_vec();
    }
    out
}

/// [`minify`] for string input. The output of valid UTF-8 input is valid
/// UTF-8.
pub fn minify_str(input: &str, options: &Options) -> String {
    String::from_utf8(minify(input.as_bytes(), options))
        .expect("the optimizer never splits UTF-8 sequences")
}
