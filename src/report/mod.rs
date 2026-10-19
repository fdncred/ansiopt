//! Views of what the optimizer did: an aligned diff, a per-token
//! explanation, a heat map of escape overhead and an HTML report that
//! combines them.
//!
//! Everything here works from an [`Alignment`] of the input with its
//! output, so the views agree with each other by construction.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;

use crate::align::{Alignment, Gap, Mismatch, Piece, align, token_at};
use crate::describe::{Category, category, visible};
use crate::lexer::{self, Token};
use crate::trace::{Fate, Trace};

pub mod html;
#[cfg(feature = "render")]
pub mod preview;
pub mod term;

/// An input, its minified output and how they line up.
pub struct Report<'a> {
    pub input: &'a [u8],
    pub output: &'a [u8],
    /// The optimizer's output would have been longer, so the input was
    /// returned unchanged.
    pub fell_back: bool,
    pub alignment: Alignment,
    pub trace: Option<Trace>,
}

impl<'a> Report<'a> {
    pub fn new(
        input: &'a [u8],
        output: &'a [u8],
        fell_back: bool,
        trace: Option<Trace>,
    ) -> Result<Self, Mismatch> {
        Ok(Report {
            input,
            output,
            fell_back,
            alignment: align(input, output)?,
            trace,
        })
    }

    /// Gaps whose bytes differ between input and output, with the index of
    /// the piece each one is.
    pub fn changed_gaps(&self) -> impl Iterator<Item = (usize, &Gap)> {
        self.alignment
            .pieces
            .iter()
            .enumerate()
            .filter_map(move |(i, p)| match p {
                Piece::Gap(g) if !g.unchanged(self.input, self.output) => Some((i, g)),
                _ => None,
            })
    }

    /// True when a changed gap means the same on both sides and was only
    /// spelled shorter, like `ESC[0m` becoming `ESC[m`.
    pub fn only_respelled(&self, gap: &Gap) -> bool {
        let meaning = |bytes: &[u8], ranges: &[Range<usize>]| -> Vec<String> {
            ranges
                .iter()
                .flat_map(|r| {
                    crate::describe::describe(&token_at(bytes, r.clone()))
                        .split(" · ")
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        meaning(self.input, &gap.input) == meaning(self.output, &gap.output)
    }

    pub fn gap_count(&self) -> usize {
        self.alignment
            .pieces
            .iter()
            .filter(|p| matches!(p, Piece::Gap(_)))
            .count()
    }

    /// Where each piece starts in the input and in the output.
    pub fn piece_offsets(&self) -> Vec<(usize, usize)> {
        let mut at = (0, 0);
        let mut offsets = Vec::with_capacity(self.alignment.pieces.len());
        for piece in &self.alignment.pieces {
            offsets.push(at);
            match piece {
                Piece::Text { input, output, .. } => {
                    at.0 = at.0.max(input.end);
                    at.1 = at.1.max(output.end);
                }
                Piece::Gap(g) => {
                    if let Some(r) = g.input.last() {
                        at.0 = r.end;
                    }
                    if let Some(r) = g.output.last() {
                        at.1 = r.end;
                    }
                }
            }
        }
        offsets
    }

    /// Up to `max` characters of text before and after piece `i`.
    pub fn context(&self, i: usize, max: usize) -> (Option<String>, Option<String>) {
        let pieces = &self.alignment.pieces;
        let before = pieces[..i].iter().rev().find_map(|p| match p {
            Piece::Text { text, .. } => {
                let chars: Vec<char> = text.chars().collect();
                Some(chars[chars.len().saturating_sub(max)..].iter().collect())
            }
            _ => None,
        });
        let after = pieces[i + 1..].iter().find_map(|p| match p {
            Piece::Text { text, .. } => Some(text.chars().take(max).collect()),
            _ => None,
        });
        (before, after)
    }
}

/// Byte and sequence counts per category for one stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub bytes: HashMap<Category, usize>,
    pub count: HashMap<Category, usize>,
}

impl Tally {
    pub fn of(bytes: &[u8]) -> Tally {
        let mut tally = Tally::default();
        for token in lexer::tokenize(bytes) {
            let cat = category(&token);
            *tally.bytes.entry(cat).or_default() += token.raw().len();
            if !matches!(token, Token::Text(_)) {
                *tally.count.entry(cat).or_default() += 1;
            }
        }
        tally
    }

    pub fn bytes(&self, cat: Category) -> usize {
        self.bytes.get(&cat).copied().unwrap_or(0)
    }

    pub fn count(&self, cat: Category) -> usize {
        self.count.get(&cat).copied().unwrap_or(0)
    }

    /// Number of escape sequences and controls.
    pub fn sequences(&self) -> usize {
        self.count.values().sum()
    }
}

/// One kind of change, with how often it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    /// What the input had between two pieces of text, as visible text.
    pub from: String,
    pub to: String,
    pub count: usize,
    /// Bytes saved in total; negative when the output is longer.
    pub saved: isize,
}

/// The changes made, grouped by what they turned into what. With
/// `generalize`, numbers are replaced by `N` so `38;5;196` and `38;5;197`
/// group together.
pub fn rewrites(report: &Report, generalize: bool) -> Vec<Rewrite> {
    let mut groups: HashMap<(String, String), (usize, isize)> = HashMap::new();
    for (_, gap) in report.changed_gaps() {
        let side = |bytes: &[u8], ranges: &[Range<usize>]| {
            let joined: Vec<u8> = ranges
                .iter()
                .flat_map(|r| bytes[r.clone()].iter().copied())
                .collect();
            let text = visible(&joined);
            if generalize {
                numbers_to_n(&text)
            } else {
                text
            }
        };
        let key = (
            side(report.input, &gap.input),
            side(report.output, &gap.output),
        );
        let entry = groups.entry(key).or_default();
        entry.0 += 1;
        entry.1 += gap.input_bytes() as isize - gap.output_bytes() as isize;
    }
    let mut list: Vec<Rewrite> = groups
        .into_iter()
        .map(|((from, to), (count, saved))| Rewrite {
            from,
            to,
            count,
            saved,
        })
        .collect();
    list.sort_by(|a, b| b.saved.cmp(&a.saved).then(b.count.cmp(&a.count)));
    list
}

fn numbers_to_n(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_number = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('N');
            }
            in_number = true;
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
}

/// A character and the escape bytes each stream spent right before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatCell {
    pub ch: char,
    pub input: usize,
    pub output: usize,
}

/// One line of text with its escape overhead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeatLine {
    pub cells: Vec<HeatCell>,
    /// Escape bytes after the last character, before the line ended.
    pub end_input: usize,
    pub end_output: usize,
}

impl HeatLine {
    pub fn input(&self) -> usize {
        self.cells.iter().map(|c| c.input).sum::<usize>() + self.end_input
    }

    pub fn output(&self) -> usize {
        self.cells.iter().map(|c| c.output).sum::<usize>() + self.end_output
    }
}

/// True for tokens that start a new display line: line feeds and movements
/// that may change the row.
fn breaks_line(token: &Token) -> bool {
    match token {
        Token::Control(b) => matches!(b, b'\n' | 0x0b | 0x0c),
        Token::Esc(e) => e.intermediates.is_empty() && matches!(e.final_byte, b'D' | b'E' | b'M'),
        Token::Csi(c) => {
            c.private.is_none()
                && c.intermediates.is_empty()
                && matches!(
                    c.final_byte,
                    b'A' | b'B' | b'E' | b'F' | b'H' | b'f' | b'd' | b'e'
                )
        }
        _ => false,
    }
}

/// Escape bytes that count as overhead: everything but single-byte
/// controls, which every line needs anyway.
fn overhead(token: &Token) -> usize {
    match token {
        Token::Control(_) => 0,
        t => t.raw().len(),
    }
}

/// Splits a gap's bytes at its last line break: overhead before the break
/// (or all of it, when there is none) and after it, plus the number of
/// line breaks.
fn split_gap(bytes: &[u8], ranges: &[Range<usize>]) -> (usize, usize, usize) {
    let tokens: Vec<Token> = ranges.iter().map(|r| token_at(bytes, r.clone())).collect();
    let breaks = tokens.iter().filter(|t| breaks_line(t)).count();
    let last_break = tokens.iter().rposition(breaks_line);
    let (mut before, mut after) = (0, 0);
    for (i, t) in tokens.iter().enumerate() {
        if last_break.is_some_and(|b| i <= b) {
            before += overhead(t);
        } else {
            after += overhead(t);
        }
    }
    (before, after, breaks)
}

/// Lines of text with the escape overhead spent on each character, split
/// where the input moves to a new line.
pub fn heat_lines(report: &Report) -> Vec<HeatLine> {
    let mut lines = vec![HeatLine::default()];
    let (mut pending_in, mut pending_out) = (0, 0);
    for piece in &report.alignment.pieces {
        match piece {
            Piece::Gap(gap) => {
                let (in_before, in_after, breaks) = split_gap(report.input, &gap.input);
                let (out_before, out_after, out_breaks) = split_gap(report.output, &gap.output);
                if breaks == 0 {
                    pending_in += in_before + in_after;
                    pending_out += out_before + out_after;
                    continue;
                }
                let line = lines.last_mut().unwrap();
                line.end_input += pending_in + in_before;
                // The output may move the cursor differently; whatever it
                // spends before its own last line break belongs here.
                if out_breaks > 0 {
                    line.end_output += pending_out + out_before;
                    pending_out = out_after;
                } else {
                    pending_out += out_before + out_after;
                }
                pending_in = in_after;
                for _ in 0..breaks {
                    lines.push(HeatLine::default());
                }
            }
            Piece::Text { text, .. } => {
                let line = lines.last_mut().unwrap();
                for ch in text.chars() {
                    line.cells.push(HeatCell {
                        ch,
                        input: std::mem::take(&mut pending_in),
                        output: std::mem::take(&mut pending_out),
                    });
                }
            }
        }
    }
    let line = lines.last_mut().unwrap();
    line.end_input += pending_in;
    line.end_output += pending_out;
    if lines.len() > 1 && lines.last() == Some(&HeatLine::default()) {
        lines.pop();
    }
    lines
}

/// Describes a fate in a few words, quoting output it points at and, with
/// `offsets`, where that output is.
pub fn fate_text(report: &Report, fate: &Fate, offsets: bool) -> String {
    let quote = |r: &Range<usize>| {
        let text = visible(&report.output[r.clone()]);
        if offsets {
            format!("{text} at {}", r.start)
        } else {
            text
        }
    };
    match fate {
        Fate::Pending => "undecided".into(),
        Fate::Text => "text".into(),
        Fate::Kept => "kept".into(),
        Fate::Rewritten { output, why } => {
            format!("rewritten as {} ({why})", quote(output))
        }
        Fate::Folded { outputs } => {
            let mut text = format!("folded into {}", quote(&outputs[0]));
            if outputs.len() > 1 {
                let _ = write!(text, " and {} more", outputs.len() - 1);
            }
            text
        }
        Fate::NoEffect { why } => format!("no effect: {why}"),
        Fate::Merged { output } => {
            format!("merged into {}", quote(output))
        }
        Fate::Superseded { why } => format!("dropped: {why}"),
        Fate::Barrier { why } => format!("barrier: {why}"),
    }
}

/// The xterm default RGB value of palette color `n`.
pub fn xterm_rgb(n: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match n {
        0..=15 => BASE[n as usize],
        16..=231 => {
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let n = n - 16;
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

/// Formats a number with thousands separators.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Percentage of `input` that was saved.
pub fn percent_saved(input: usize, output: usize) -> f64 {
    if input == 0 {
        0.0
    } else {
        (input as f64 - output as f64) * 100.0 / input as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report<'a>(input: &'a [u8], output: &'a [u8]) -> Report<'a> {
        Report::new(input, output, false, None).unwrap()
    }

    #[test]
    fn groups_rewrites() {
        let input = b"\x1b[0m\x1b[38;5;1ma\x1b[0m\x1b[38;5;2mb";
        let output = b"\x1b[;38;5;1ma\x1b[;38;5;2mb";
        let r = report(input, output);
        let exact = rewrites(&r, false);
        assert_eq!(exact.len(), 2);
        let general = rewrites(&r, true);
        assert_eq!(
            general,
            [Rewrite {
                from: "\\e[Nm\\e[N;N;Nm".into(),
                to: "\\e[;N;N;Nm".into(),
                count: 2,
                saved: 6,
            }]
        );
    }

    #[test]
    fn heat_lines_follow_input_lines() {
        let input = b"\x1b[1mab\x1b[0m\n\x1b[31mc";
        let output = b"\x1b[1mab\x1b[m\n\x1b[31mc";
        let lines = heat_lines(&report(input, output));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].cells[0].input, 4);
        assert_eq!(lines[0].end_input, 4);
        assert_eq!(lines[0].end_output, 3);
        assert_eq!(lines[1].cells[0].input, 5);
        assert_eq!(lines[0].input() + lines[1].input(), 13);
    }

    #[test]
    fn tallies_by_category() {
        let t = Tally::of(b"\x1b[1mhi\x1b[2J\n");
        assert_eq!(t.bytes(Category::Sgr), 4);
        assert_eq!(t.bytes(Category::Text), 2);
        assert_eq!(t.count(Category::Edit), 1);
        assert_eq!(t.sequences(), 3);
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
