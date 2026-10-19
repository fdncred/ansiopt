//! A record of what the optimizer did with each input token, for
//! `--explain` and the HTML report.

use std::ops::Range;

use crate::lexer::{Token, tokenize};

/// One entry per input token, in input order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trace {
    pub entries: Vec<Entry>,
}

impl Trace {
    /// The trace of a stream passed through unchanged.
    pub fn unchanged(input: &[u8]) -> Trace {
        let mut pos = 0;
        let entries = tokenize(input)
            .iter()
            .map(|t| {
                let len = t.raw().len();
                let entry = Entry {
                    input: pos..pos + len,
                    fate: if matches!(t, Token::Text(_)) {
                        Fate::Text
                    } else {
                        Fate::Kept
                    },
                };
                pos += len;
                entry
            })
            .collect();
        Trace { entries }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The token's bytes in the input.
    pub input: Range<usize>,
    pub fate: Fate,
}

/// What became of a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// Not decided yet. Never left in a finished trace.
    Pending,
    /// Printed text. With REP the output may spell it shorter.
    Text,
    /// Written unchanged.
    Kept,
    /// Written in an equivalent, shorter form.
    Rewritten {
        output: Range<usize>,
        why: &'static str,
    },
    /// A change of colors, hyperlink or character set that was written as
    /// part of these output sequences, once something needed it.
    Folded { outputs: Vec<Range<usize>> },
    /// Changed nothing anything ever used.
    NoEffect { why: &'static str },
    /// Combined with neighboring movements or mode changes into one output
    /// sequence.
    Merged { output: Range<usize> },
    /// Made pointless by a later sequence before it mattered.
    Superseded { why: &'static str },
    /// Copied verbatim as a barrier: the optimizer synchronized everything
    /// first and stopped assuming whatever it might change.
    Barrier { why: &'static str },
}

impl Fate {
    /// A short name for grouping.
    pub fn kind(&self) -> &'static str {
        match self {
            Fate::Pending => "pending",
            Fate::Text => "text",
            Fate::Kept => "kept",
            Fate::Rewritten { .. } => "rewritten",
            Fate::Folded { .. } => "folded",
            Fate::NoEffect { .. } => "no effect",
            Fate::Merged { .. } => "merged",
            Fate::Superseded { .. } => "superseded",
            Fate::Barrier { .. } => "barrier",
        }
    }
}
