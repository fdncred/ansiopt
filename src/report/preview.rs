//! Renders input and output with the vt100 emulator, for the HTML report's
//! side-by-side preview.
//!
//! vt100 is good enough to show that both streams produce the same screen,
//! but it is not a faithful screenshot: it has no DEC line-drawing
//! characters, strikethrough, blink or hyperlinks, and bold and faint
//! replace each other.

use std::fmt::Write as _;
use std::num::NonZeroU16;

use super::xterm_rgb;
use crate::report::html::escape;

/// Both screens as HTML, and how they compare.
pub struct Rendered {
    pub input: String,
    pub output: String,
    pub rows: u16,
    pub cols: u16,
    /// Rows shown: trailing rows blank in both screens are left out.
    pub shown_rows: u16,
    /// Cells that differ between the two screens.
    pub differences: usize,
    pub cursor_matches: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Look {
    fn of(cell: &vt100::Cell) -> Look {
        Look {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    fn css(&self) -> String {
        let color = |c: vt100::Color| match c {
            vt100::Color::Default => None,
            vt100::Color::Idx(n) => {
                let (r, g, b) = xterm_rgb(n);
                Some(format!("#{r:02x}{g:02x}{b:02x}"))
            }
            vt100::Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        };
        let (mut fg, mut bg) = (color(self.fg), color(self.bg));
        if self.inverse {
            (fg, bg) = (
                Some(bg.unwrap_or("var(--term-bg)".into())),
                Some(fg.unwrap_or("var(--term-fg)".into())),
            );
        }
        let mut css = String::new();
        if let Some(fg) = fg {
            let _ = write!(css, "color:{fg};");
        }
        if let Some(bg) = bg {
            let _ = write!(css, "background:{bg};");
        }
        if self.bold {
            css.push_str("font-weight:700;");
        }
        if self.dim {
            css.push_str("opacity:.6;");
        }
        if self.italic {
            css.push_str("font-style:italic;");
        }
        if self.underline {
            css.push_str("text-decoration:underline;");
        }
        css
    }
}

fn parse(bytes: &[u8], rows: u16, cols: u16) -> vt100::Parser {
    let mut parser = vt100::Parser::new(
        NonZeroU16::new(rows.max(1)).unwrap(),
        NonZeroU16::new(cols.max(1)).unwrap(),
        0,
    );
    // A program's output reaches the terminal through the tty driver, which
    // turns each line feed into CR LF. Captures written straight to a file
    // skip that step, so do it here; a CR before a CR LF changes nothing.
    let mut cooked = Vec::with_capacity(bytes.len() + bytes.len() / 32);
    for &b in bytes {
        if b == b'\n' {
            cooked.push(b'\r');
        }
        cooked.push(b);
    }
    parser.process(&cooked);
    parser
}

/// What a cell shows. In visual mode a blank cell is only its background
/// and underline.
fn appearance(cell: &vt100::Cell, visual: bool) -> (String, Option<Look>) {
    let text = cell.contents().to_string();
    let look = Look::of(cell);
    if visual && (text.is_empty() || text == " ") {
        let bg = if look.inverse { look.fg } else { look.bg };
        let blank = Look {
            fg: vt100::Color::Default,
            bg,
            bold: false,
            dim: false,
            italic: false,
            underline: look.underline,
            inverse: false,
        };
        return (" ".into(), Some(blank));
    }
    (text, Some(look))
}

fn blank_row(screen: &vt100::Screen, row: u16, cols: u16) -> bool {
    (0..cols).all(|col| {
        screen.cell(row, col).is_none_or(|c| {
            (c.contents().is_empty() || c.contents() == " ")
                && c.bgcolor() == vt100::Color::Default
                && !c.inverse()
                && !c.underline()
        })
    })
}

/// Where the stream last leaves the alternate screen, if it does.
pub fn alternate_screen_exit(bytes: &[u8]) -> Option<usize> {
    let mut pos = 0;
    let mut exit = None;
    for token in crate::lexer::tokenize(bytes) {
        if let crate::lexer::Token::Csi(c) = token
            && c.private == Some(b'?')
            && c.final_byte == b'l'
            && c.params
                .split(|&b| b == b';')
                .any(|p| matches!(p, b"47" | b"1047" | b"1049"))
        {
            exit = Some(pos);
        }
        pos += token.raw().len();
    }
    exit
}

/// The largest row and column the stream moves the cursor to, a hint at the
/// size of the terminal a full-screen program was drawing on.
pub fn addressed_size(bytes: &[u8]) -> (u16, u16) {
    let (mut rows, mut cols) = (0u16, 0u16);
    for token in crate::lexer::tokenize(bytes) {
        let crate::lexer::Token::Csi(c) = token else {
            continue;
        };
        if c.private.is_some() || !c.intermediates.is_empty() {
            continue;
        }
        let nums: Vec<u16> = c
            .params
            .split(|&b| b == b';')
            .map(|p| {
                std::str::from_utf8(p)
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(1)
            })
            .collect();
        let at = |i: usize| nums.get(i).copied().unwrap_or(1).min(1000);
        match c.final_byte {
            b'H' | b'f' => {
                rows = rows.max(at(0));
                cols = cols.max(at(1));
            }
            b'd' => rows = rows.max(at(0)),
            b'G' | b'`' => cols = cols.max(at(0)),
            _ => {}
        }
    }
    (rows, cols)
}

/// Renders both streams on a `cols` × `rows` screen.
pub fn render(input: &[u8], output: &[u8], cols: u16, rows: u16, visual: bool) -> Rendered {
    let (a, b) = (parse(input, rows, cols), parse(output, rows, cols));
    let (sa, sb) = (a.screen(), b.screen());
    let shown_rows = (0..rows)
        .rev()
        .find(|&r| !blank_row(sa, r, cols) || !blank_row(sb, r, cols))
        .map_or(0, |r| r + 1)
        .max(sa.cursor_position().0 + 1)
        .max(sb.cursor_position().0 + 1)
        .min(rows);

    let mut differences = 0;
    let mut differs = vec![vec![false; cols as usize]; shown_rows as usize];
    for row in 0..shown_rows {
        for col in 0..cols {
            let (Some(x), Some(y)) = (sa.cell(row, col), sb.cell(row, col)) else {
                continue;
            };
            if appearance(x, visual) != appearance(y, visual) {
                differs[row as usize][col as usize] = true;
                differences += 1;
            }
        }
    }
    Rendered {
        input: screen_html(sa, cols, shown_rows, &differs),
        output: screen_html(sb, cols, shown_rows, &differs),
        rows,
        cols,
        shown_rows,
        differences,
        cursor_matches: sa.cursor_position() == sb.cursor_position(),
    }
}

/// One `<div>` per row, one `<span>` per run of cells that look alike.
fn screen_html(screen: &vt100::Screen, cols: u16, rows: u16, differs: &[Vec<bool>]) -> String {
    let mut html = String::new();
    let cursor = screen.cursor_position();
    for row in 0..rows {
        html.push_str("<div class=\"row\">");
        let mut run: Option<(Look, bool, String)> = None;
        let flush = |run: &mut Option<(Look, bool, String)>, html: &mut String| {
            if let Some((look, diff, text)) = run.take() {
                let class = if diff { " class=\"diffcell\"" } else { "" };
                let css = look.css();
                if css.is_empty() && !diff {
                    html.push_str(&text);
                } else {
                    let _ = write!(html, "<span{class} style=\"{css}\">{text}</span>");
                }
            }
        };
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let look = Look::of(cell);
            let diff = differs[row as usize][col as usize];
            let contents = cell.contents();
            let mut text = if contents.is_empty() {
                " ".to_string()
            } else {
                escape(contents)
            };
            if (row, col) == cursor {
                flush(&mut run, &mut html);
                text = format!("<span class=\"cursor\">{text}</span>");
                let class = if diff { " class=\"diffcell\"" } else { "" };
                let _ = write!(html, "<span{class} style=\"{}\">{text}</span>", look.css());
                continue;
            }
            match &mut run {
                Some((l, d, t)) if *l == look && *d == diff => t.push_str(&text),
                _ => {
                    flush(&mut run, &mut html);
                    run = Some((look, diff, text));
                }
            }
        }
        flush(&mut run, &mut html);
        html.push_str("</div>");
    }
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_streams_render_alike() {
        let r = render(
            b"\x1b[1;31mhi\x1b[0m\n",
            b"\x1b[1;31mhi\x1b[m\n",
            10,
            3,
            false,
        );
        assert_eq!(r.differences, 0);
        assert!(r.cursor_matches);
        assert_eq!(r.input, r.output);
        assert!(r.input.contains("font-weight:700"), "{}", r.input);
    }

    #[test]
    fn differences_are_marked() {
        let r = render(b"\x1b[31mhi", b"\x1b[32mhi", 10, 2, false);
        assert_eq!(r.differences, 2);
        assert!(r.output.contains("diffcell"));
    }
}
