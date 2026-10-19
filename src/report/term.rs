//! Terminal renderings: `--diff`, `--explain` and `--hotspot`.
//!
//! Content from the stream is always shown with its escapes made visible,
//! never raw, so the stream's own colors can't fight the view's.

use std::fmt::Write as _;
use std::ops::Range;

use super::{HeatLine, Report, fate_text, heat_lines, percent_saved, rewrites, thousands};
use crate::align::token_at;
use crate::describe::{describe, visible};
use crate::trace::Fate;

/// Whether to color the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paint {
    pub color: bool,
}

impl Paint {
    fn sgr(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[m")
        } else {
            s.to_string()
        }
    }

    fn red(&self, s: &str) -> String {
        self.sgr("31", s)
    }

    fn green(&self, s: &str) -> String {
        self.sgr("32", s)
    }

    fn cyan(&self, s: &str) -> String {
        self.sgr("36", s)
    }

    fn dim(&self, s: &str) -> String {
        self.sgr("2", s)
    }

    fn bold(&self, s: &str) -> String {
        self.sgr("1", s)
    }
}

/// Sequential blue ramp for the heat map, lightest first.
const HEAT: [(u8, u8, u8); 5] = [
    (0xcd, 0xe2, 0xfb),
    (0x9e, 0xc5, 0xf4),
    (0x6d, 0xa7, 0xec),
    (0x39, 0x87, 0xe5),
    (0x1c, 0x5c, 0xab),
];

/// Upper bounds of the heat bins, in bytes.
pub const HEAT_BINS: [&str; 5] = ["1-4", "5-9", "10-19", "20-39", "40+"];

/// The heat bin for a byte count, or `None` for zero.
pub fn heat_bin(bytes: usize) -> Option<usize> {
    match bytes {
        0 => None,
        1..=4 => Some(0),
        5..=9 => Some(1),
        10..=19 => Some(2),
        20..=39 => Some(3),
        _ => Some(4),
    }
}

fn heat(paint: Paint, bytes: usize, s: &str) -> String {
    match heat_bin(bytes) {
        Some(bin) if paint.color => {
            let (r, g, b) = HEAT[bin];
            let ink = if bin >= 3 { "255;255;255" } else { "11;11;11" };
            format!("\x1b[48;2;{r};{g};{b};38;2;{ink}m{s}\x1b[m")
        }
        _ => s.to_string(),
    }
}

fn summary_line(report: &Report) -> String {
    let (a, b) = (report.input.len(), report.output.len());
    format!(
        "{} → {} bytes ({:.1}% saved)",
        thousands(a),
        thousands(b),
        percent_saved(a, b)
    )
}

fn side(bytes: &[u8], ranges: &[Range<usize>]) -> (String, String) {
    if ranges.is_empty() {
        return ("(nothing)".into(), String::new());
    }
    let chips: Vec<String> = ranges.iter().map(|r| visible(&bytes[r.clone()])).collect();
    let words: Vec<String> = ranges
        .iter()
        .map(|r| describe(&token_at(bytes, r.clone())))
        .collect();
    (chips.join(" "), words.join(" ; "))
}

/// The aligned diff: every place where the escape sequences between two
/// pieces of text changed. Changes that only spell the same thing shorter
/// are counted but not shown unless `all` is set.
pub fn diff(report: &Report, paint: Paint, all: bool) -> String {
    let mut out = String::new();
    let changed: Vec<_> = report.changed_gaps().collect();
    let _ = writeln!(
        out,
        "{} {} · {} of {} gaps changed",
        paint.bold("ansiopt diff:"),
        summary_line(report),
        thousands(changed.len()),
        thousands(report.gap_count())
    );
    if report.fell_back {
        out.push_str(
            "The optimized output would have been longer, so the input is returned unchanged.\n",
        );
        return out;
    }
    if changed.is_empty() {
        out.push_str("Nothing changed.\n");
        return out;
    }

    let respelled = changed
        .iter()
        .filter(|(_, g)| report.only_respelled(g))
        .count();
    if !all && respelled > 0 {
        let _ = writeln!(
            out,
            "({} changes that only spell the same thing shorter, like \\e[0m → \\e[m, are not shown; --all shows them)",
            thousands(respelled)
        );
    }
    let offsets = report.piece_offsets();
    for (i, gap) in &changed {
        if !all && report.only_respelled(gap) {
            continue;
        }
        let at = offsets[*i];
        let (before, after) = report.context(*i, 24);
        let before = before.map_or("⟨start⟩".to_string(), |t| {
            format!("…{}", visible(t.as_bytes()))
        });
        let after = after.map_or("⟨end⟩".to_string(), |t| {
            format!("{}…", visible(t.as_bytes()))
        });
        let _ = writeln!(
            out,
            "\n{} {}{}{}",
            paint.cyan(&format!("@@ in {} · out {} @@", at.0, at.1)),
            before,
            paint.dim("▸"),
            after
        );
        let (from, from_words) = side(report.input, &gap.input);
        let (to, to_words) = side(report.output, &gap.output);
        let width = from.chars().count().max(to.chars().count());
        for (sign, chips, words, color) in [
            (
                "-",
                &from,
                &from_words,
                Paint::red as fn(&Paint, &str) -> String,
            ),
            ("+", &to, &to_words, Paint::green),
        ] {
            let line = format!("{sign} {chips}");
            if words.is_empty() {
                let _ = writeln!(out, "{}", color(&paint, &line));
            } else if width <= 44 {
                let pad = " ".repeat(width - chips.chars().count() + 3);
                let _ = writeln!(out, "{}{pad}{}", color(&paint, &line), paint.dim(words));
            } else {
                let _ = writeln!(out, "{}", color(&paint, &line));
                let _ = writeln!(out, "    {}", paint.dim(words));
            }
        }
    }

    let top = rewrites(report, true);
    if !top.is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            paint.bold("Most common changes (numbers shown as N):")
        );
        let _ = writeln!(out, "{:>9} {:>7}  input → output", "saved", "count");
        for r in top.iter().take(10) {
            let saved = if r.saved < 0 {
                format!("-{}", thousands(r.saved.unsigned_abs()))
            } else {
                thousands(r.saved as usize)
            };
            let _ = writeln!(
                out,
                "{saved:>9} {:>7}  {} → {}",
                thousands(r.count),
                paint.red(&r.from),
                paint.green(&r.to)
            );
        }
    }
    out
}

/// What happened to every input sequence. Text is never listed, and
/// sequences kept as they were only with `all`.
pub fn explain(report: &Report, paint: Paint, all: bool) -> String {
    let mut out = String::new();
    let Some(trace) = &report.trace else {
        out.push_str("ansiopt explain: no trace was recorded\n");
        return out;
    };
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for entry in &trace.entries {
        let kind = entry.fate.kind();
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind, 1)),
        }
    }
    let summary: Vec<String> = counts
        .iter()
        .map(|(k, n)| format!("{k} {}", thousands(*n)))
        .collect();
    let _ = writeln!(
        out,
        "{} {} · {}",
        paint.bold("ansiopt explain:"),
        summary_line(report),
        summary.join(" · ")
    );
    if report.fell_back {
        out.push_str(
            "The optimized output would have been longer, so every sequence was kept as is.\n",
        );
        return out;
    }
    if !all {
        out.push_str("(sequences kept as they were are not listed; --all lists them)\n");
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "{:>8}  {:<26}  {:<30}  what happened",
        "offset", "sequence", "meaning"
    );
    for entry in &trace.entries {
        if entry.fate == Fate::Text || (!all && entry.fate == Fate::Kept) {
            continue;
        }
        let bytes = &report.input[entry.input.clone()];
        let seq = clip(&visible(bytes), 26);
        let meaning = clip(&describe(&token_at(report.input, entry.input.clone())), 30);
        let what = fate_text(report, &entry.fate, true);
        let what = match entry.fate {
            Fate::Superseded { .. } | Fate::NoEffect { .. } => paint.red(&what),
            Fate::Barrier { .. } => paint.sgr("35", &what),
            _ => paint.green(&what),
        };
        let _ = writeln!(
            out,
            "{:>8}  {seq:<26}  {}  {what}",
            entry.input.start,
            pad(&paint.dim(&meaning), meaning.chars().count(), 30)
        );
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

/// Pads already-styled text whose visible width is `width`.
fn pad(styled: &str, width: usize, to: usize) -> String {
    format!("{styled}{}", " ".repeat(to.saturating_sub(width)))
}

/// The heat map: each character shaded by how many escape bytes the input
/// spent right before it, with per-line totals in the gutter.
pub fn hotspot(report: &Report, paint: Paint) -> String {
    let lines = heat_lines(report);
    let total_in: usize = lines.iter().map(HeatLine::input).sum();
    let total_out: usize = lines.iter().map(HeatLine::output).sum();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} escape bytes spent on each run of text: input {} → output {}",
        paint.bold("ansiopt hotspot:"),
        thousands(total_in),
        thousands(total_out)
    );
    out.push_str("(line feeds and other single-byte controls are not counted)\n");
    if paint.color {
        out.push_str("key:");
        for (i, label) in HEAT_BINS.iter().enumerate() {
            let sample = [1, 5, 10, 20, 40][i];
            let _ = write!(out, " {}", heat(paint, sample, &format!(" {label} ")));
        }
        out.push_str(" bytes; ⏎ marks bytes spent at the end of a line\n");
    }
    let _ = writeln!(out, "\n{:>7} {:>7} │", "in", "out");
    for line in &lines {
        let _ = write!(
            out,
            "{:>7} {:>7} │ ",
            thousands(line.input()),
            thousands(line.output())
        );
        // Each run of text is shaded by the bytes spent before it.
        let mut current = 0;
        for cell in &line.cells {
            if cell.input > 0 || cell.output > 0 {
                current = cell.input;
            }
            out.push_str(&heat(paint, current, &cell.ch.to_string()));
        }
        if line.end_input > 0 {
            out.push_str(&heat(paint, line.end_input, "⏎"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Paint = Paint { color: false };

    #[test]
    fn diff_shows_changed_gaps() {
        let input = b"\x1b[0m\x1b[1mhi\x1b[0m\x1b[0m there\n";
        let output = b"\x1b[;1mhi\x1b[m there\n";
        let report = Report::new(input, output, false, None).unwrap();
        let text = diff(&report, PLAIN, true);
        assert!(text.contains("2 of 3 gaps changed"), "{text}");
        assert!(text.contains("@@ in 0 · out 0 @@ ⟨start⟩▸hi…"), "{text}");
        assert!(text.contains("- \\e[0m \\e[1m   reset ; bold"), "{text}");
        assert!(text.contains("+ \\e[;1m        reset · bold"), "{text}");
        assert!(text.contains("@@ in 10 · out 7 @@ …hi▸ there…"), "{text}");
    }

    #[test]
    fn diff_explains_fallback() {
        let report = Report::new(b"x", b"x", true, None).unwrap();
        assert!(diff(&report, PLAIN, false).contains("returned unchanged"));
    }

    #[test]
    fn hotspot_gutter_totals() {
        let input = b"\x1b[1mab\x1b[0m\ncd";
        let output = b"\x1b[1mab\x1b[m\ncd";
        let report = Report::new(input, output, false, None).unwrap();
        let text = hotspot(&report, PLAIN);
        assert!(text.contains("      8       7 │ ab"), "{text}");
        assert!(text.contains("      0       0 │ cd"), "{text}");
    }
}
