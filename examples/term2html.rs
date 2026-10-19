//! Renders terminal output read from stdin as an HTML page that looks like
//! a terminal window, for the README screenshots (see
//! `docs/screenshots.nu`).
//!
//! Usage: `term2html <title> [columns]`. Lines longer than `columns`
//! (default 160) wrap. Prints the page to stdout and the window size in
//! pixels (`WIDTH HEIGHT`) to stderr.

use std::io::Read;

use ansiopt::lexer::{Token, tokenize};
use ansiopt::report::html::escape;
use ansiopt::report::preview;

/// Width of one cell of the 14px monospace font, in pixels.
const CELL_W: f64 = 8.43;
/// Height of one row, in pixels.
const CELL_H: f64 = 20.3;

fn main() {
    let title = std::env::args().nth(1).unwrap_or_default();
    let max_cols: usize = std::env::args()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or(160);
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();

    // Size the screen to the output: as many columns as the widest line,
    // and enough rows for the lines that wrap.
    let (mut cols, mut width, mut rows) = (0, 0, 1);
    let mut wrapped = 0;
    for token in tokenize(&input) {
        match token {
            Token::Text(s) => width += s.chars().count(),
            Token::Control(b'\n') => {
                cols = cols.max(width);
                wrapped += width.saturating_sub(1) / max_cols;
                width = 0;
                rows += 1;
            }
            _ => {}
        }
    }
    let cols = cols.max(width).clamp(20, max_cols) as u16;
    let rendered = preview::render(&input, &input, cols, rows + wrapped as u16, false);

    print!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
:root {{ --term-bg: #16181d; --term-fg: #e6e6e6; }}
html, body {{ margin: 0; background: transparent; }}
body {{ padding: 14px; }}
.win {{ display: inline-block; background: var(--term-bg); color: var(--term-fg); border-radius: 10px;
  box-shadow: 0 6px 20px rgba(0,0,0,.28); overflow: hidden;
  font: 14px/1.45 Menlo, ui-monospace, monospace; }}
.bar {{ display: flex; align-items: center; gap: 8px; padding: 10px 14px; background: #24272e;
  color: #a7adb8; font-size: 13px; }}
.dot {{ width: 12px; height: 12px; border-radius: 50%; flex: none; }}
.r {{ background: #ff5f57; }} .y {{ background: #febc2e; }} .g {{ background: #28c840; }}
.title {{ margin-left: 8px; white-space: pre; }}
.screen {{ padding: 12px 16px; white-space: pre; }}
.row {{ height: 1.45em; }}
</style></head><body><div class="win"><div class="bar"><span class="dot r"></span><span class="dot y"></span><span class="dot g"></span><span class="title">{}</span></div><div class="screen">{}</div></div></body></html>"#,
        escape(&title),
        rendered.input
    );
    let title_w = title.chars().count() as f64 * 7.8 + 110.0;
    let w = (f64::from(cols) * CELL_W + 32.0).max(title_w) + 28.0 + 4.0;
    let h = f64::from(rendered.shown_rows) * CELL_H + 37.0 + 24.0 + 28.0 + 12.0;
    eprintln!("{} {}", w.ceil(), h.ceil());
}
