//! The HTML report: one self-contained page with the diff, a rendered
//! preview, the heat map, a byte breakdown, the most common changes and the
//! full explanation.
//!
//! Everything taken from the stream is HTML-escaped and no link in it is
//! made clickable: a report gets passed around, and a crafted capture must
//! not be able to inject markup or a `javascript:` URL.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::{
    HeatLine, Report, Tally, fate_text, heat_lines, percent_saved, rewrites, thousands, xterm_rgb,
};
use crate::Options;
use crate::align::{Piece, token_at};
use crate::describe::{Category, describe, visible};
use crate::lexer::Token;
use crate::report::term::heat_bin;
use crate::sgr::{Color, Pen};
use crate::trace::Fate;

/// What the report says about how it was made.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Shown as the report's title: usually the input file name.
    pub title: String,
    pub options: Options,
    /// Columns and rows for the rendered preview.
    pub width: Option<u16>,
    pub height: Option<u16>,
}

/// Escapes text for HTML content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Deduplicates strings, so repeated tooltips and styles are stored once.
#[derive(Default)]
struct Interner {
    ids: HashMap<String, usize>,
    list: Vec<String>,
}

impl Interner {
    fn id(&mut self, s: String) -> usize {
        if let Some(&id) = self.ids.get(&s) {
            return id;
        }
        let id = self.list.len();
        self.ids.insert(s.clone(), id);
        self.list.push(s);
        id
    }
}

/// Tooltips and text styles shared by the whole page.
#[derive(Default)]
struct Shared {
    tips: Interner,
    styles: Interner,
}

impl Shared {
    fn tip(&mut self, text: String) -> String {
        format!("data-t=\"{}\"", self.tips.id(text))
    }

    fn style(&mut self, css: String) -> String {
        format!("p{}", self.styles.id(css))
    }
}

/// A JSON string literal that is also safe inside a `<script>` element.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Most rows the explanation table lists.
const EXPLAIN_LIMIT: usize = 5000;
/// Most rows the preview renders when no height is given.
#[cfg(feature = "render")]
const PREVIEW_ROWS: u16 = 1000;

/// Renders the report as a self-contained HTML page.
pub fn html(report: &Report, settings: &Settings) -> String {
    let fates: HashMap<usize, &Fate> = report
        .trace
        .iter()
        .flat_map(|t| &t.entries)
        .map(|e| (e.input.start, &e.fate))
        .collect();
    let mut shared = Shared::default();
    let mut panels: Vec<(&str, &str, String)> =
        vec![("diff", "Diff", diff_panel(report, &fates, &mut shared))];
    #[cfg(feature = "render")]
    panels.push(("rendered", "Rendered", rendered_panel(report, settings)));
    panels.push(("hotspot", "Hotspot", hotspot_panel(report, &mut shared)));
    panels.push(("breakdown", "Breakdown", breakdown_panel(report)));
    panels.push(("changes", "Changes", changes_panel(report)));
    panels.push(("explain", "Explain", explain_panel(report)));

    let mut styles = String::new();
    for (i, css) in shared.styles.list.iter().enumerate() {
        let _ = writeln!(styles, ".p{i}{{{css}}}");
    }
    let mut page = String::with_capacity(report.input.len() * 4 + 64 * 1024);
    let title = escape(&settings.title);
    let _ = write!(
        page,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>ansiopt report: {title}</title>\n<style>{CSS}{styles}</style>\n</head>\n<body>\n<main>\n"
    );
    let _ = writeln!(
        page,
        "<header><h1>ansiopt report</h1><p class=\"sub\"><span class=\"file\">{title}</span> · {}</p></header>",
        escape(&describe_options(&settings.options))
    );
    page.push_str(&tiles(report));

    page.push_str("<nav class=\"tabs\" role=\"tablist\">");
    for (i, (id, label, _)) in panels.iter().enumerate() {
        let _ = write!(
            page,
            "<button role=\"tab\" data-tab=\"{id}\" aria-selected=\"{}\">{label}</button>",
            i == 0
        );
    }
    page.push_str("</nav>\n");
    for (id, _, body) in &panels {
        let _ = write!(
            page,
            "<section class=\"panel\" id=\"{id}\" role=\"tabpanel\">\n{body}\n</section>\n"
        );
    }
    let tips: Vec<String> = shared.tips.list.iter().map(|t| json_string(t)).collect();
    let _ = write!(
        page,
        "</main>\n<script>const TIPS=[{}];\n{JS}</script>\n</body>\n</html>\n",
        tips.join(",")
    );
    page
}

fn describe_options(o: &Options) -> String {
    let mut flags = Vec::new();
    if o.assume_reset {
        flags.push("--assume-reset");
    }
    if o.visual {
        flags.push("--visual");
    }
    if o.rep {
        flags.push("--rep");
    }
    if o.compat {
        flags.push("--compat");
    }
    if flags.is_empty() {
        "default options".into()
    } else {
        flags.join(" ")
    }
}

fn tiles(report: &Report) -> String {
    let (a, b) = (report.input.len(), report.output.len());
    let (ta, tb) = (Tally::of(report.input), Tally::of(report.output));
    let changed = report.changed_gaps().count();
    let tile = |label: &str, value: String, note: String| {
        format!(
            "<div class=\"tile\"><div class=\"label\">{label}</div><div class=\"value\">{value}</div><div class=\"note\">{note}</div></div>"
        )
    };
    let mut html = String::from("<section class=\"tiles\">");
    html.push_str(&tile("Input", thousands(a), "bytes".into()));
    html.push_str(&tile("Output", thousands(b), "bytes".into()));
    html.push_str(&tile(
        "Saved",
        format!("{:.1}%", percent_saved(a, b)),
        format!("{} bytes", thousands(a.saturating_sub(b))),
    ));
    html.push_str(&tile(
        "Sequences",
        format!(
            "{} → {}",
            thousands(ta.sequences()),
            thousands(tb.sequences())
        ),
        "escape sequences and controls".into(),
    ));
    html.push_str(&tile(
        "Changed spots",
        thousands(changed),
        format!(
            "of {} between pieces of text",
            thousands(report.gap_count())
        ),
    ));
    html.push_str("</section>\n");
    if report.fell_back {
        html.push_str(
            "<p class=\"notice\">The optimized output would have been longer than the input, so ansiopt returns the input unchanged.</p>\n",
        );
    }
    html
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn color_css(color: Option<Color>) -> Option<String> {
    match color? {
        Color::Default => None,
        Color::Basic(n) => Some(hex(xterm_rgb(n))),
        Color::Bright(n) => Some(hex(xterm_rgb(n + 8))),
        Color::Indexed(n) => Some(hex(xterm_rgb(n))),
        Color::Rgb(r, g, b) => Some(hex((r, g, b))),
    }
}

/// Inline CSS for text drawn with `pen`.
fn pen_css(pen: &Pen) -> String {
    let (mut fg, mut bg) = (color_css(pen.fg), color_css(pen.bg));
    if pen.inverse == Some(true) {
        (fg, bg) = (
            Some(bg.unwrap_or("var(--term-bg)".into())),
            Some(fg.unwrap_or("var(--term-fg)".into())),
        );
    }
    let mut css = String::new();
    if pen.hidden == Some(true) {
        fg = Some(bg.clone().unwrap_or("var(--term-bg)".into()));
    }
    if let Some(fg) = fg {
        let _ = write!(css, "color:{fg};");
    }
    if let Some(bg) = bg {
        let _ = write!(css, "background:{bg};");
    }
    if pen.intensity.bold == Some(true) {
        css.push_str("font-weight:700;");
    }
    if pen.intensity.dim == Some(true) {
        css.push_str("opacity:.6;");
    }
    if pen.italic == Some(true) {
        css.push_str("font-style:italic;");
    }
    let mut lines = Vec::new();
    if pen.underline == Some(true) {
        lines.push("underline");
    }
    if pen.strike == Some(true) {
        lines.push("line-through");
    }
    if pen.overline == Some(true) {
        lines.push("overline");
    }
    if !lines.is_empty() {
        let _ = write!(css, "text-decoration:{};", lines.join(" "));
    }
    css
}

fn is_line_break(token: &Token) -> bool {
    matches!(token, Token::Control(b'\n' | 0x0b | 0x0c))
}

fn chip(class: &str, bytes: &[u8], tip: &str) -> String {
    format!(
        "<span class=\"chip {class}\" {tip}>{}</span>",
        escape(&visible(bytes))
    )
}

fn diff_panel(report: &Report, fates: &HashMap<usize, &Fate>, shared: &mut Shared) -> String {
    let mut html = String::from(
        "<p class=\"intro\">The text as the input draws it. Wherever the escape sequences between two pieces of text changed, \
         <span class=\"chip del\">red</span> chips show what the input had and <span class=\"chip ins\">green</span> chips what ansiopt writes instead \
         (<span class=\"chip ins none\">∅</span> means nothing). Hover a chip to see what it does and what happened to it.</p>\n\
         <div class=\"controls\">\
         <label><input type=\"checkbox\" id=\"show-respell\"> Show changes that only shorten the spelling (RESPELL_COUNT)</label>\
         <label><input type=\"checkbox\" id=\"show-same\"> Show sequences that didn't change</label></div>\n\
         <div class=\"term\" id=\"diff-term\">",
    );
    let mut respelled = 0;
    let mut pen = Pen::DEFAULT;
    for piece in &report.alignment.pieces {
        match piece {
            Piece::Text { text, .. } => {
                let css = pen_css(&pen);
                if css.is_empty() {
                    html.push_str(&escape(text));
                } else {
                    let class = shared.style(css);
                    let _ = write!(html, "<span class=\"{class}\">{}</span>", escape(text));
                }
            }
            Piece::Gap(gap) => {
                let changed = !gap.unchanged(report.input, report.output);
                let tokens_in: Vec<_> = gap
                    .input
                    .iter()
                    .map(|r| (r.clone(), token_at(report.input, r.clone())))
                    .collect();
                let chip_in = |class: &str, html: &mut String, shared: &mut Shared| {
                    for (r, t) in &tokens_in {
                        if is_line_break(t) {
                            continue;
                        }
                        let fate = fates
                            .get(&r.start)
                            .map_or(String::new(), |f| fate_text(report, f, false));
                        let tip = shared.tip(format!(
                            "{}\n{}\n{}",
                            visible(&report.input[r.clone()]),
                            describe(t),
                            fate
                        ));
                        html.push_str(&chip(class, &report.input[r.clone()], &tip));
                    }
                };
                if changed {
                    if report.only_respelled(gap) {
                        respelled += 1;
                        html.push_str("<span class=\"change respell\">");
                    } else {
                        html.push_str("<span class=\"change\">");
                    }
                    chip_in("del", &mut html, shared);
                    html.push_str("<span class=\"arrow\">→</span>");
                    let mut wrote = false;
                    for r in &gap.output {
                        let t = token_at(report.output, r.clone());
                        if is_line_break(&t) {
                            continue;
                        }
                        let bytes = &report.output[r.clone()];
                        let tip = shared.tip(format!(
                            "{}\n{}\nwritten by ansiopt",
                            visible(bytes),
                            describe(&t)
                        ));
                        html.push_str(&chip("ins", bytes, &tip));
                        wrote = true;
                    }
                    if !wrote {
                        html.push_str(
                            "<span class=\"chip ins none\" data-tip=\"nothing is written here\">∅</span>",
                        );
                    }
                    html.push_str("</span>");
                } else {
                    chip_in("same", &mut html, shared);
                }
                for (_, t) in &tokens_in {
                    if is_line_break(t) {
                        html.push('\n');
                    }
                    if let Token::Csi(c) = t
                        && c.private.is_none()
                        && c.intermediates.is_empty()
                        && c.final_byte == b'm'
                    {
                        let mut next = pen;
                        if next.apply(c.params) {
                            pen = next;
                        }
                    }
                }
            }
        }
    }
    html.push_str("</div>");
    html.replacen("RESPELL_COUNT", &thousands(respelled), 1)
}

#[cfg(feature = "render")]
fn rendered_panel(report: &Report, settings: &Settings) -> String {
    use super::preview::{addressed_size, alternate_screen_exit, render};

    // A full-screen program that leaves the alternate screen at the end
    // would show an empty screen; show its last frame instead.
    let exits = (
        alternate_screen_exit(report.input),
        alternate_screen_exit(report.output),
    );
    let (input, output, frame_note) = match exits {
        (Some(a), Some(b)) => (
            &report.input[..a],
            &report.output[..b],
            " Showing the alternate screen just before the program left it.",
        ),
        _ => (report.input, report.output, ""),
    };
    let lines = input.iter().filter(|&&b| b == b'\n').count();
    let longest = heat_lines(report)
        .iter()
        .map(|l| l.cells.len())
        .max()
        .unwrap_or(0);
    let (addressed_rows, addressed_cols) = addressed_size(input);
    let cols = settings
        .width
        .unwrap_or_else(|| longest.max(addressed_cols.into()).clamp(80, 400) as u16);
    let rows = settings.height.unwrap_or_else(|| {
        if exits.0.is_some() {
            addressed_rows.max(24)
        } else {
            (lines + 1).clamp(24, PREVIEW_ROWS as usize) as u16
        }
    });
    let r = render(input, output, cols, rows, settings.options.visual);
    let verdict = if r.differences == 0 && r.cursor_matches {
        "<span class=\"badge good\">✓ Identical</span> Every cell and the cursor position match."
            .to_string()
    } else {
        format!(
            "<span class=\"badge bad\">✗ {} cells differ</span>{} Differing cells are outlined.",
            thousands(r.differences),
            if r.cursor_matches {
                ""
            } else {
                " The cursor ends in a different place."
            }
        )
    };
    let mut html = format!(
        "<p class=\"intro\">{verdict}</p>\n\
         <p class=\"caveat\">Rendered by the vt100 crate on a {}×{} screen{}.{frame_note} This checks that both streams draw the same thing; \
         it is not a screenshot. vt100 has no DEC line-drawing characters, strikethrough, blink or hyperlinks, and bold and faint replace each other. \
         Line feeds are treated as CR LF, as a terminal's tty delivers program output. \
         Use <code>--width</code> and <code>--height</code> to match the terminal the capture came from.{}</p>\n",
        r.cols,
        r.rows,
        if settings.height.is_none() && exits.0.is_none() && lines + 1 > PREVIEW_ROWS as usize {
            " (only the last rows fit)"
        } else {
            ""
        },
        if settings.options.visual {
            " With <code>--visual</code>, blank cells are compared only by what shows on them."
        } else {
            ""
        }
    );
    let _ = write!(
        html,
        "<div class=\"side-by-side\"><figure><figcaption>Input</figcaption><div class=\"term screen\" id=\"screen-in\">{}</div></figure>\
         <figure><figcaption>Output</figcaption><div class=\"term screen\" id=\"screen-out\">{}</div></figure></div>",
        r.input, r.output
    );
    html
}

fn heat_class(prefix: char, bytes: usize) -> String {
    match heat_bin(bytes) {
        Some(b) => format!("{prefix}{b}"),
        None => String::new(),
    }
}

fn heat_span(ch: &str, input: usize, output: usize, shared: &mut Shared) -> String {
    let classes = [
        heat_class('i', input),
        heat_class('o', output),
        heat_class('s', input.saturating_sub(output)),
    ]
    .join(" ");
    let tip = shared.tip(format!(
        "{input} bytes of escapes spent on this in the input, {output} in the output"
    ));
    format!(
        "<span class=\"{}\" {tip}>{}</span>",
        classes.trim(),
        escape(ch)
    )
}

fn run_html(text: &str, (input, output): (usize, usize), shared: &mut Shared) -> String {
    if input == 0 && output == 0 {
        escape(text)
    } else {
        heat_span(text, input, output, shared)
    }
}

fn hotspot_panel(report: &Report, shared: &mut Shared) -> String {
    let lines = heat_lines(report);
    let total_in: usize = lines.iter().map(HeatLine::input).sum();
    let total_out: usize = lines.iter().map(HeatLine::output).sum();
    let mut html = format!(
        "<p class=\"intro\">Each run of text is shaded by how many bytes of escape sequences were spent right before it, usually to color it: \
         {} in the input, {} in the output. Line feeds and other single-byte controls aren't counted, \
         and output totals per line are approximate where ansiopt merged cursor movements across lines. \
         <span class=\"eol\">⏎</span> marks bytes spent at the end of a line.</p>\n",
        thousands(total_in),
        thousands(total_out)
    );
    html.push_str(
        "<div class=\"controls\" role=\"radiogroup\" aria-label=\"Shade by\">Shade by \
         <label><input type=\"radio\" name=\"metric\" value=\"i\" checked> input</label>\
         <label><input type=\"radio\" name=\"metric\" value=\"o\"> output</label>\
         <label><input type=\"radio\" name=\"metric\" value=\"s\"> saved</label>\
         <span class=\"legend\">",
    );
    for (i, label) in super::term::HEAT_BINS.iter().enumerate() {
        let _ = write!(html, "<span class=\"swatch h{i}\">{label}</span>");
    }
    html.push_str(
        " bytes</span></div>\n<div class=\"heat\" data-metric=\"i\"><div class=\"term\">",
    );
    for line in &lines {
        let _ = write!(
            html,
            "<div class=\"hline\"><span class=\"gut\">{}</span><span class=\"gut\">{}</span><span class=\"htext\">",
            thousands(line.input()),
            thousands(line.output())
        );
        // Each run of text is shaded by what was spent to style it: the
        // bytes before its first character.
        let mut run = String::new();
        let mut heat = (0, 0);
        for cell in &line.cells {
            if (cell.input > 0 || cell.output > 0) && !run.is_empty() {
                html.push_str(&run_html(&std::mem::take(&mut run), heat, shared));
            }
            if cell.input > 0 || cell.output > 0 {
                heat = (cell.input, cell.output);
            }
            run.push(cell.ch);
        }
        html.push_str(&run_html(&run, heat, shared));
        if line.end_input > 0 || line.end_output > 0 {
            html.push_str(&heat_span("⏎", line.end_input, line.end_output, shared));
        }
        html.push_str("</span></div>");
    }
    html.push_str("</div></div>");
    html
}

fn breakdown_panel(report: &Report) -> String {
    let (ta, tb) = (Tally::of(report.input), Tally::of(report.output));
    let mut rows: Vec<(Category, usize, usize)> = Category::ALL
        .iter()
        .map(|&c| (c, ta.bytes(c), tb.bytes(c)))
        .filter(|&(_, a, b)| a > 0 || b > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.max(r.2)));
    let max = rows.iter().map(|r| r.1.max(r.2)).max().unwrap_or(1).max(1);

    let (label_w, plot_w, value_w, row_h, top) = (190.0, 380.0, 190.0, 32.0, 8.0);
    let width = label_w + plot_w + value_w;
    let height = top + row_h * rows.len() as f64 + 8.0;
    let x = |v: usize| label_w + v as f64 / max as f64 * plot_w;
    let mut svg = format!(
        "<svg class=\"dumbbell\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"Bytes per category before and after\">"
    );
    let _ = write!(
        svg,
        "<line class=\"baseline\" x1=\"{label_w}\" x2=\"{label_w}\" y1=\"{top}\" y2=\"{}\"/>",
        height - 8.0
    );
    for (i, (cat, a, b)) in rows.iter().enumerate() {
        let y = top + row_h * i as f64 + row_h / 2.0;
        let saved = percent_saved(*a, *b);
        let tip = format!(
            "{}: {} → {} bytes ({:.1}% saved)",
            cat.label(),
            thousands(*a),
            thousands(*b),
            saved
        );
        let _ = write!(
            svg,
            "<g data-tip=\"{}\"><rect class=\"hit\" x=\"0\" y=\"{}\" width=\"{width}\" height=\"{row_h}\"/>\
             <text class=\"cat\" x=\"{}\" y=\"{y}\" dy=\"0.35em\" text-anchor=\"end\">{}</text>\
             <line class=\"link\" x1=\"{}\" x2=\"{}\" y1=\"{y}\" y2=\"{y}\"/>\
             <circle class=\"dot-in\" cx=\"{}\" cy=\"{y}\" r=\"6\"/><circle class=\"dot-out\" cx=\"{}\" cy=\"{y}\" r=\"6\"/>\
             <text class=\"val\" x=\"{}\" y=\"{y}\" dy=\"0.35em\">{} → {}</text></g>",
            escape(&tip),
            y - row_h / 2.0,
            label_w - 12.0,
            escape(cat.label()),
            x(*a.min(b)),
            x(*a.max(b)),
            x(*a),
            x(*b),
            label_w + plot_w + 16.0,
            thousands(*a),
            thousands(*b),
        );
    }
    svg.push_str("</svg>");

    let mut table = String::from(
        "<table class=\"data\"><thead><tr><th>Category</th><th>Input bytes</th><th>Output bytes</th><th>Saved</th><th>Sequences</th></tr></thead><tbody>",
    );
    for (cat, a, b) in &rows {
        let seqs = if *cat == Category::Text {
            "–".to_string()
        } else {
            format!(
                "{} → {}",
                thousands(ta.count(*cat)),
                thousands(tb.count(*cat))
            )
        };
        let _ = write!(
            table,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.1}%</td><td>{seqs}</td></tr>",
            escape(cat.label()),
            thousands(*a),
            thousands(*b),
            percent_saved(*a, *b)
        );
    }
    table.push_str("</tbody></table>");
    format!(
        "<p class=\"intro\">Bytes spent on each kind of content, before and after.</p>\n\
         <div class=\"legend chart-legend\"><span class=\"key in\"></span> input <span class=\"key out\"></span> output</div>\n\
         {svg}\n{table}"
    )
}

fn changes_panel(report: &Report) -> String {
    let table = |list: &[super::Rewrite]| {
        let mut t = String::from(
            "<table class=\"data\"><thead><tr><th>Saved</th><th>Count</th><th>Input</th><th></th><th>Output</th></tr></thead><tbody>",
        );
        for r in list.iter().take(40) {
            let saved = if r.saved < 0 {
                format!("−{}", thousands(r.saved.unsigned_abs()))
            } else {
                thousands(r.saved as usize)
            };
            let to = if r.to.is_empty() { "∅" } else { &r.to };
            let _ = write!(
                t,
                "<tr><td>{saved}</td><td>{}</td><td><code class=\"del\">{}</code></td><td>→</td><td><code class=\"ins\">{}</code></td></tr>",
                thousands(r.count),
                escape(&r.from),
                escape(to)
            );
        }
        t.push_str("</tbody></table>");
        t
    };
    let general = rewrites(report, true);
    let exact = rewrites(report, false);
    if general.is_empty() {
        return "<p class=\"intro\">Nothing changed.</p>".into();
    }
    format!(
        "<p class=\"intro\">The most common changes, by bytes saved. Each row is what one spot between two pieces of text \
         had in the input and has in the output.</p>\n\
         <div class=\"controls\" role=\"radiogroup\" aria-label=\"Group\">Group \
         <label><input type=\"radio\" name=\"group\" value=\"general\" checked> numbers as N</label>\
         <label><input type=\"radio\" name=\"group\" value=\"exact\"> exactly</label></div>\n\
         <div id=\"changes-general\">{}</div><div id=\"changes-exact\" hidden>{}</div>",
        table(&general),
        table(&exact)
    )
}

fn explain_panel(report: &Report) -> String {
    let Some(trace) = &report.trace else {
        return "<p class=\"intro\">No trace was recorded.</p>".into();
    };
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for e in &trace.entries {
        let kind = e.fate.kind();
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind, 1)),
        }
    }
    let mut html = String::from(
        "<p class=\"intro\">What ansiopt did with every sequence in the input. \
         <b>Folded</b>: a color, hyperlink or character set change written later as part of the sequence shown, once text needed it. \
         <b>Merged</b>: combined with neighboring movements or mode changes. \
         <b>Dropped</b> and <b>no effect</b>: removed because nothing could see them. \
         <b>Barrier</b>: copied as is, after writing everything out, because ansiopt doesn't model it.</p>\n\
         <div class=\"controls\" id=\"fate-filters\">Show ",
    );
    for (kind, n) in &counts {
        if *kind == "text" {
            continue;
        }
        let checked = if *kind == "kept" { "" } else { " checked" };
        let _ = write!(
            html,
            "<label><input type=\"checkbox\" value=\"{}\"{checked}> {kind} ({})</label>",
            kind.replace(' ', "-"),
            thousands(*n)
        );
    }
    html.push_str("</div>\n");
    let total = trace
        .entries
        .iter()
        .filter(|e| e.fate != Fate::Text)
        .count();
    if total > EXPLAIN_LIMIT {
        let _ = writeln!(
            html,
            "<p class=\"caveat\">The counts above cover all {} sequences; the table lists the first {}. \
             <code>ansiopt --explain --all</code> prints every one.</p>",
            thousands(total),
            thousands(EXPLAIN_LIMIT)
        );
    }
    html.push_str("<table class=\"data explain hide-kept\"><thead><tr><th>Offset</th><th>Sequence</th><th>Meaning</th><th>What happened</th></tr></thead><tbody>");
    let mut listed = 0;
    for e in &trace.entries {
        if e.fate == Fate::Text {
            continue;
        }
        if listed == EXPLAIN_LIMIT {
            break;
        }
        listed += 1;
        let bytes = &report.input[e.input.clone()];
        let _ = write!(
            html,
            "<tr class=\"k-{}\"><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
            e.fate.kind().replace(' ', "-"),
            e.input.start,
            escape(&visible(bytes)),
            escape(&describe(&token_at(report.input, e.input.clone()))),
            escape(&fate_text(report, &e.fate, true))
        );
    }
    html.push_str("</tbody></table>");
    html
}

const CSS: &str = r#"
:root {
  color-scheme: light;
  --page: #f9f9f7; --surface: #fcfcfb; --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --grid: #e1e0d9; --axis: #c3c2b7; --ring: rgba(11,11,11,0.10);
  --in: #86b6ef; --out: #1c5cab;
  --del-bg: #fbe3e3; --del-ink: #9b1c1c; --ins-bg: #ddf3dd; --ins-ink: #145214;
  --good: #0ca30c; --bad: #d03b3b;
  --term-bg: #16181d; --term-fg: #e6e6e6;
  --h0: #cde2fb; --h1: #9ec5f4; --h2: #6da7ec; --h3: #3987e5; --h4: #1c5cab;
}
@media (prefers-color-scheme: dark) {
  :root:where(:not([data-theme="light"])) {
    color-scheme: dark;
    --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781;
    --grid: #2c2c2a; --axis: #383835; --ring: rgba(255,255,255,0.10);
    --in: #256abf; --out: #9ec5f4;
    --del-bg: #3d1c1c; --del-ink: #f2a3a3; --ins-bg: #183318; --ins-ink: #a8e0a8;
  }
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781;
  --grid: #2c2c2a; --axis: #383835; --ring: rgba(255,255,255,0.10);
  --in: #256abf; --out: #9ec5f4;
  --del-bg: #3d1c1c; --del-ink: #f2a3a3; --ins-bg: #183318; --ins-ink: #a8e0a8;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--page); color: var(--ink);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1200px; margin: 0 auto; padding: 24px 16px 64px; }
h1 { font-size: 22px; margin: 0; }
.sub { color: var(--ink-2); margin: 4px 0 20px; overflow-wrap: anywhere; }
.file { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
code, .chip, .term, .gut { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 12px; margin-bottom: 20px; }
.tile { background: var(--surface); border: 1px solid var(--ring); border-radius: 10px; padding: 12px 14px; }
.tile .label { color: var(--ink-2); font-size: 13px; }
.tile .value { font-size: 26px; font-weight: 650; margin: 2px 0; }
.tile .note { color: var(--muted); font-size: 12px; }
.notice { background: var(--surface); border-left: 3px solid var(--bad); padding: 8px 12px; }
.tabs { display: flex; gap: 4px; overflow-x: auto; border-bottom: 1px solid var(--grid); margin-bottom: 16px; }
.tabs button { background: none; border: 0; border-bottom: 2px solid transparent; color: var(--ink-2);
  font: inherit; padding: 8px 12px; cursor: pointer; white-space: nowrap; }
.tabs button[aria-selected="true"] { color: var(--ink); border-bottom-color: var(--ink); font-weight: 600; }
.intro, .caveat { max-width: 860px; color: var(--ink-2); }
.caveat { font-size: 13px; }
.controls, .toggle { display: flex; flex-wrap: wrap; gap: 12px; align-items: center; margin: 8px 0 12px; color: var(--ink-2); font-size: 14px; }
.term { background: var(--term-bg); color: var(--term-fg); border-radius: 8px; padding: 12px 14px;
  white-space: pre-wrap; overflow-wrap: anywhere; font-size: 13px; line-height: 1.45; overflow-x: auto; }
.chip { display: inline-block; font-size: 11px; line-height: 1.3; padding: 0 4px; margin: 0 1px; border-radius: 4px;
  vertical-align: 1px; white-space: pre; cursor: default; }
.chip.del { background: var(--del-bg); color: var(--del-ink); text-decoration: line-through; }
.chip.ins { background: var(--ins-bg); color: var(--ins-ink); }
.chip.none { text-decoration: none; }
.chip.same { display: none; background: #3a3d44; color: #b9bcc4; }
#diff-term.show-same .chip.same { display: inline-block; }
#diff-term:not(.show-respell) .change.respell { display: none; }
.change { white-space: nowrap; }
.arrow { color: #8a8f99; font-size: 11px; margin: 0 2px; }
p .chip { vertical-align: 0; }
.side-by-side { display: grid; grid-template-columns: repeat(auto-fit, minmax(420px, 1fr)); gap: 12px; }
.side-by-side figure { margin: 0; min-width: 0; }
figcaption { color: var(--ink-2); font-size: 13px; margin-bottom: 4px; }
.screen { white-space: pre; max-height: 70vh; overflow: auto; }
.screen .row { min-height: 1.45em; }
.diffcell { outline: 2px solid var(--bad); outline-offset: -1px; }
.cursor { outline: 1px solid var(--term-fg); }
.badge { display: inline-block; padding: 1px 8px; border-radius: 999px; font-weight: 600; margin-right: 6px; }
.badge.good { background: var(--ins-bg); color: var(--ins-ink); }
.badge.bad { background: var(--del-bg); color: var(--del-ink); }
.heat .term { white-space: pre; }
.hline { display: flex; }
.gut { color: #8a8f99; min-width: 6ch; text-align: right; padding-right: 1ch; flex: none; }
.gut + .gut { border-right: 1px solid #3a3d44; margin-right: 1ch; }
.htext { white-space: pre; }
.heat[data-metric="i"] .i0, .heat[data-metric="o"] .o0, .heat[data-metric="s"] .s0, .swatch.h0 { background: var(--h0); color: #0b0b0b; }
.heat[data-metric="i"] .i1, .heat[data-metric="o"] .o1, .heat[data-metric="s"] .s1, .swatch.h1 { background: var(--h1); color: #0b0b0b; }
.heat[data-metric="i"] .i2, .heat[data-metric="o"] .o2, .heat[data-metric="s"] .s2, .swatch.h2 { background: var(--h2); color: #0b0b0b; }
.heat[data-metric="i"] .i3, .heat[data-metric="o"] .o3, .heat[data-metric="s"] .s3, .swatch.h3 { background: var(--h3); color: #fff; }
.heat[data-metric="i"] .i4, .heat[data-metric="o"] .o4, .heat[data-metric="s"] .s4, .swatch.h4 { background: var(--h4); color: #fff; }
.swatch { display: inline-block; padding: 0 6px; margin-left: 2px; border-radius: 3px; font-size: 12px; }
.legend { color: var(--ink-2); font-size: 13px; }
.eol { font-family: ui-monospace, monospace; }
.dumbbell { width: 100%; max-width: 760px; height: auto; display: block; margin: 8px 0 16px; }
.dumbbell .hit { fill: transparent; }
.dumbbell g:hover .hit { fill: var(--grid); }
.dumbbell .baseline { stroke: var(--axis); stroke-width: 1; }
.dumbbell .link { stroke: var(--axis); stroke-width: 2; }
.dumbbell .dot-in { fill: var(--in); stroke: var(--surface); stroke-width: 2; }
.dumbbell .dot-out { fill: var(--out); stroke: var(--surface); stroke-width: 2; }
.dumbbell text { fill: var(--ink); font-size: 13px; }
.dumbbell .val { fill: var(--ink-2); font-variant-numeric: tabular-nums; }
.chart-legend { display: flex; gap: 6px; align-items: center; }
.key { display: inline-block; width: 12px; height: 12px; border-radius: 50%; }
.key.in { background: var(--in); } .key.out { background: var(--out); margin-left: 10px; }
table.data { border-collapse: collapse; width: 100%; font-size: 13px; background: var(--surface); }
table.data th, table.data td { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--grid); vertical-align: top; }
table.data th { color: var(--ink-2); font-weight: 600; }
table.data td:first-child, table.data td:nth-child(2) { font-variant-numeric: tabular-nums; }
table.data code { overflow-wrap: anywhere; }
code.del { color: var(--del-ink); } code.ins { color: var(--ins-ink); }
.explain.hide-kept .k-kept, .explain.hide-folded .k-folded, .explain.hide-merged .k-merged,
.explain.hide-rewritten .k-rewritten, .explain.hide-superseded .k-superseded,
.explain.hide-no-effect .k-no-effect, .explain.hide-barrier .k-barrier { display: none; }
.tip { position: fixed; z-index: 10; max-width: 420px; background: var(--ink); color: var(--page); border-radius: 6px;
  padding: 6px 9px; font-size: 12px; line-height: 1.4; white-space: pre-wrap; pointer-events: none;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; box-shadow: 0 4px 16px rgba(0,0,0,.2); }
.tabs-js .panel { display: none; } .tabs-js .panel.active { display: block; }
@media (max-width: 640px) { .tile .value { font-size: 22px; } .side-by-side { grid-template-columns: 1fr; } }
"#;

const JS: &str = r#"
(() => {
  const root = document.documentElement;
  root.classList.add('tabs-js');
  const buttons = [...document.querySelectorAll('[data-tab]')];
  const show = id => {
    for (const p of document.querySelectorAll('.panel')) p.classList.toggle('active', p.id === id);
    for (const b of buttons) b.setAttribute('aria-selected', String(b.dataset.tab === id));
    try { localStorage.setItem('ansiopt-tab', id); } catch (e) {}
    try { history.replaceState(null, '', '#tab-' + id); } catch (e) {}
  };
  let first = buttons[0] && buttons[0].dataset.tab;
  try { const saved = localStorage.getItem('ansiopt-tab'); if (saved && document.getElementById(saved)) first = saved; } catch (e) {}
  const hash = location.hash.replace(/^#tab-/, '');
  if (hash && document.getElementById(hash)) first = hash;
  for (const b of buttons) b.addEventListener('click', () => show(b.dataset.tab));
  if (first) show(first);

  const tip = document.createElement('div');
  tip.className = 'tip';
  tip.hidden = true;
  document.body.appendChild(tip);
  const place = e => {
    const pad = 14, w = tip.offsetWidth, h = tip.offsetHeight;
    let x = e.clientX + pad, y = e.clientY + pad;
    if (x + w > innerWidth - 8) x = e.clientX - w - pad;
    if (y + h > innerHeight - 8) y = e.clientY - h - pad;
    tip.style.left = Math.max(8, x) + 'px';
    tip.style.top = Math.max(8, y) + 'px';
  };
  document.addEventListener('mouseover', e => {
    const t = e.target.closest('[data-t],[data-tip]');
    if (!t) { tip.hidden = true; return; }
    tip.textContent = t.dataset.t !== undefined ? TIPS[+t.dataset.t] : t.dataset.tip;
    tip.hidden = false;
    place(e);
  });
  document.addEventListener('mousemove', e => { if (!tip.hidden) place(e); });

  for (const [box, cls] of [['show-same', 'show-same'], ['show-respell', 'show-respell']]) {
    const c = document.getElementById(box);
    if (c) c.addEventListener('change', () =>
      document.getElementById('diff-term').classList.toggle(cls, c.checked));
  }

  const heat = document.querySelector('.heat');
  for (const r of document.querySelectorAll('input[name="metric"]'))
    r.addEventListener('change', () => { if (r.checked) heat.dataset.metric = r.value; });

  for (const r of document.querySelectorAll('input[name="group"]'))
    r.addEventListener('change', () => {
      document.getElementById('changes-general').hidden = r.value !== 'general' || !r.checked;
      document.getElementById('changes-exact').hidden = r.value !== 'exact' || !r.checked;
    });

  const table = document.querySelector('table.explain');
  for (const c of document.querySelectorAll('#fate-filters input'))
    c.addEventListener('change', () => table.classList.toggle('hide-' + c.value, !c.checked));

  const a = document.getElementById('screen-in'), b = document.getElementById('screen-out');
  if (a && b) {
    let lock = false;
    const sync = (from, to) => () => {
      if (lock) return;
      lock = true; to.scrollTop = from.scrollTop; to.scrollLeft = from.scrollLeft;
      requestAnimationFrame(() => lock = false);
    };
    a.addEventListener('scroll', sync(a, b));
    b.addEventListener('scroll', sync(b, a));
  }
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            title: "<cap>.ansi".into(),
            options: Options::default(),
            width: None,
            height: None,
        }
    }

    #[test]
    fn escapes_stream_content() {
        let input = b"\x1b]0;<script>alert(1)</script>\x07\x1b]8;;javascript:alert(1)\x07<b>x</b>\x1b]8;;\x07";
        let minified = crate::minify_detailed(input, &Options::default(), true);
        let report = Report::new(input, &minified.output, false, minified.trace).unwrap();
        let page = html(&report, &settings());
        assert!(!page.contains("<script>alert"));
        assert!(!page.contains("<b>x</b>"));
        assert!(!page.contains("href"));
        assert!(page.contains("&lt;cap&gt;.ansi"));
    }

    #[test]
    fn has_every_section() {
        let input = b"\x1b[0m\x1b[1;31mhi\x1b[0m\x1b[0m\n";
        let minified = crate::minify_detailed(input, &Options::default(), true);
        let report = Report::new(input, &minified.output, false, minified.trace).unwrap();
        let page = html(&report, &settings());
        for id in ["diff", "hotspot", "breakdown", "changes", "explain"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "{id}");
        }
        #[cfg(feature = "render")]
        assert!(page.contains("✓ Identical"));
        assert!(page.contains("chip del"));
    }
}
