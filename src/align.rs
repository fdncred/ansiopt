//! Lines up an input stream with its minified output.
//!
//! The optimizer never changes, drops or reorders printed characters; only
//! the escape sequences between them change (REP aside, which this module
//! expands). So both streams are split at their characters, and the
//! sequences between the n-th and (n+1)-th character of each are compared
//! directly. That gives a precise "this is what the input said here, and
//! this is what the output says instead" without a byte-level diff, which
//! would be noise for a stream whose sequences get merged and moved.

use std::ops::Range;

use crate::lexer::{self, Token};

/// Upper bound on characters produced by expanding one REP, so a crafted
/// `ESC[65535b` can't blow up memory.
const MAX_REP: usize = 65_535;

/// One piece of the aligned streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Characters printed by both streams with no sequence in between.
    Text {
        text: String,
        /// Bytes of the input that printed these characters. Characters
        /// repeated by REP have no bytes of their own.
        input: Range<usize>,
        output: Range<usize>,
    },
    /// The sequences between two runs of text.
    Gap(Gap),
}

/// The tokens between two characters, as byte ranges into each stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gap {
    pub input: Vec<Range<usize>>,
    pub output: Vec<Range<usize>>,
}

impl Gap {
    /// True when both streams have the same bytes here.
    pub fn unchanged(&self, input: &[u8], output: &[u8]) -> bool {
        let a = self.input.iter().flat_map(|r| &input[r.clone()]);
        let b = self.output.iter().flat_map(|r| &output[r.clone()]);
        a.eq(b)
    }

    pub fn input_bytes(&self) -> usize {
        self.input.iter().map(ExactSizeIterator::len).sum()
    }

    pub fn output_bytes(&self) -> usize {
        self.output.iter().map(ExactSizeIterator::len).sum()
    }
}

/// Input and output lined up on their text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Alignment {
    pub pieces: Vec<Piece>,
}

/// The streams don't print the same characters, so they can't be lined up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub input_offset: usize,
    pub output_offset: usize,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "input and output print different text (input byte {}, output byte {})",
            self.input_offset, self.output_offset
        )
    }
}

impl std::error::Error for Mismatch {}

enum Event {
    Char { c: char, bytes: Range<usize> },
    Seq(Range<usize>),
}

/// Splits a stream into characters and sequences, expanding REP into the
/// characters it repeats.
fn events(bytes: &[u8]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut pos = 0;
    let mut last = None;
    for token in lexer::tokenize(bytes) {
        let len = token.raw().len();
        let range = pos..pos + len;
        match token {
            Token::Text(s) => {
                let mut at = pos;
                for c in s.chars() {
                    events.push(Event::Char {
                        c,
                        bytes: at..at + c.len_utf8(),
                    });
                    at += c.len_utf8();
                    last = Some(c);
                }
            }
            Token::Csi(c) if rep_count(&c).is_some() => {
                events.push(Event::Seq(range.clone()));
                if let (Some(n), Some(ch)) = (rep_count(&c), last) {
                    for _ in 0..n {
                        events.push(Event::Char {
                            c: ch,
                            bytes: range.end..range.end,
                        });
                    }
                }
            }
            _ => events.push(Event::Seq(range)),
        }
        pos += len;
    }
    events
}

/// The repeat count of a REP sequence, or `None` for anything else.
fn rep_count(c: &lexer::Csi) -> Option<usize> {
    if c.private.is_some() || !c.intermediates.is_empty() || c.final_byte != b'b' {
        return None;
    }
    if c.params.is_empty() {
        return Some(1);
    }
    let n: usize = std::str::from_utf8(c.params).ok()?.parse().ok()?;
    Some(n.clamp(1, MAX_REP))
}

/// Lines up `input` with `output`, which must print the same characters.
pub fn align(input: &[u8], output: &[u8]) -> Result<Alignment, Mismatch> {
    let a = events(input);
    let b = events(output);
    let (mut i, mut j) = (0, 0);
    let mut pieces: Vec<Piece> = Vec::new();
    loop {
        let mut gap = Gap::default();
        while let Some(Event::Seq(r)) = a.get(i) {
            gap.input.push(r.clone());
            i += 1;
        }
        while let Some(Event::Seq(r)) = b.get(j) {
            gap.output.push(r.clone());
            j += 1;
        }
        let had_gap = !gap.input.is_empty() || !gap.output.is_empty();
        if had_gap {
            pieces.push(Piece::Gap(gap));
        }
        match (a.get(i), b.get(j)) {
            (None, None) => break,
            (Some(Event::Char { c: x, bytes: bx }), Some(Event::Char { c: y, bytes: by }))
                if x == y =>
            {
                match pieces.last_mut() {
                    Some(Piece::Text {
                        text,
                        input,
                        output,
                    }) if !had_gap => {
                        text.push(*x);
                        // Characters repeated by REP have no bytes to add.
                        if !bx.is_empty() {
                            input.end = bx.end;
                        }
                        if !by.is_empty() {
                            output.end = by.end;
                        }
                    }
                    _ => pieces.push(Piece::Text {
                        text: x.to_string(),
                        input: bx.clone(),
                        output: by.clone(),
                    }),
                }
                i += 1;
                j += 1;
            }
            (x, y) => {
                let offset = |e: Option<&Event>, total: usize| match e {
                    Some(Event::Char { bytes, .. }) => bytes.start,
                    _ => total,
                };
                return Err(Mismatch {
                    input_offset: offset(x, input.len()),
                    output_offset: offset(y, output.len()),
                });
            }
        }
    }
    Ok(Alignment { pieces })
}

/// The token in `bytes` at `range`, which must be a range from an
/// [`Alignment`] of those bytes.
pub fn token_at(bytes: &[u8], range: Range<usize>) -> Token<'_> {
    match lexer::scan(&bytes[range.clone()], true) {
        lexer::Scan::Token(token, _) => token,
        lexer::Scan::NeedMore => unreachable!("eof was set"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(input: &[u8], output: &[u8]) -> Vec<String> {
        align(input, output)
            .unwrap()
            .pieces
            .iter()
            .map(|p| match p {
                Piece::Text { text, .. } => format!("text {text:?}"),
                Piece::Gap(g) => {
                    let side = |bytes: &[u8], rs: &[Range<usize>]| {
                        rs.iter()
                            .map(|r| crate::describe::visible(&bytes[r.clone()]))
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    format!(
                        "gap [{}] -> [{}]",
                        side(input, &g.input),
                        side(output, &g.output)
                    )
                }
            })
            .collect()
    }

    #[test]
    fn pairs_sequences_between_characters() {
        let input = b"\x1b[0m\x1b[1mhi\x1b[0m\x1b[0m there\n";
        let output = b"\x1b[;1mhi\x1b[m there\n";
        assert_eq!(
            summary(input, output),
            [
                "gap [\\e[0m \\e[1m] -> [\\e[;1m]",
                "text \"hi\"",
                "gap [\\e[0m \\e[0m] -> [\\e[m]",
                "text \" there\"",
                "gap [\\n] -> [\\n]"
            ]
        );
    }

    #[test]
    fn expands_rep() {
        assert_eq!(
            summary(b"aaaaaa", b"a\x1b[5b"),
            ["text \"a\"", "gap [] -> [\\e[5b]", "text \"aaaaa\""]
        );
    }

    #[test]
    fn reports_mismatches() {
        assert_eq!(
            align(b"ab", b"ax"),
            Err(Mismatch {
                input_offset: 1,
                output_offset: 1
            })
        );
        assert!(align(b"ab", b"a").is_err());
    }
}
