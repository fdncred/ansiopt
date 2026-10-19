//! ansiopt's differential suite with libghostty-vt as a second oracle next
//! to vt100. Building it needs Zig 0.16 on `PATH`.

#[macro_use]
#[path = "../../tests/harness/mod.rs"]
mod harness;

use harness::{Oracle, Size};
use libghostty_vt::Terminal;
use libghostty_vt::screen::CellContentTag;
use libghostty_vt::terminal::{Mode, ModeKind, Point, PointCoordinate};

/// Ghostty's terminal core. It combines bold and faint, applies origin mode
/// to VPA, erases with the background only, and models hyperlinks,
/// character sets, strikethrough and the other attributes vt100 ignores.
struct Ghostty;

/// Modes worth comparing: DEC private, then ANSI.
const DEC_MODES: &[u16] = &[
    1, 5, 6, 7, 12, 25, 45, 66, 69, 1000, 1002, 1003, 1004, 1006, 1007, 1049, 2004,
];
const ANSI_MODES: &[u16] = &[4, 20];

impl Oracle for Ghostty {
    fn name(&self) -> &'static str {
        "ghostty"
    }

    fn snapshot(&self, bytes: &[u8], size: Size, visual: bool) -> Vec<String> {
        let mut term = Terminal::new(size.cols, size.rows).unwrap();
        term.vt_write(bytes);

        let modes: Vec<_> = DEC_MODES
            .iter()
            .map(|&m| Mode::new(m, ModeKind::Dec))
            .chain(ANSI_MODES.iter().map(|&m| Mode::new(m, ModeKind::Ansi)))
            .map(|m| term.mode(m).ok())
            .collect();
        let mut lines = vec![
            format!(
                "cursor {},{} pending_wrap={} visible={}",
                term.cursor_x().unwrap(),
                term.cursor_y().unwrap(),
                term.is_cursor_pending_wrap().unwrap(),
                term.is_cursor_visible().unwrap()
            ),
            format!("pen {:?}", term.cursor_style().unwrap()),
            format!(
                "screen {:?} title {:?}",
                term.active_screen().unwrap(),
                term.title().unwrap()
            ),
            format!("modes {modes:?}"),
        ];

        for row in 0..size.rows {
            for col in 0..size.cols {
                let point = Point::Active(PointCoordinate {
                    x: col,
                    y: row.into(),
                });
                let grid = term.grid_ref(point).unwrap();
                if col == 0 {
                    lines.push(format!(
                        "wrapped {row} {}",
                        grid.row().unwrap().is_wrapped().unwrap()
                    ));
                }
                let cell = grid.cell().unwrap();
                let style = grid.style().unwrap();

                let mut chars = ['\0'; 16];
                let n = grid.graphemes(&mut chars).unwrap();
                let text: String = chars[..n].iter().collect();
                let mut uri = [0u8; 256];
                let n = grid.hyperlink_uri(&mut uri).unwrap();
                let link = String::from_utf8_lossy(&uri[..n]).into_owned();
                // Erased cells keep only a background color, stored in the
                // cell itself rather than in a style.
                let bg = match cell.content_tag().unwrap() {
                    CellContentTag::BgColorPalette => {
                        format!("{:?}", cell.bg_color_palette().unwrap())
                    }
                    CellContentTag::BgColorRgb => format!("{:?}", cell.bg_color_rgb().unwrap()),
                    _ => format!("{:?}", style.bg_color),
                };

                let blank = (text.is_empty() || text == " ") && !style.inverse;
                let desc = if visual && blank {
                    format!(
                        "{text:?} bg={bg} ul={:?} strike={} over={} hidden={} link={link:?}",
                        style.underline, style.strikethrough, style.overline, style.invisible
                    )
                } else {
                    format!(
                        "{text:?} bg={bg} {style:?} wide={:?} link={link:?}",
                        cell.wide().unwrap()
                    )
                };
                lines.push(format!("{row},{col} {desc}"));
            }
        }
        lines
    }
}

suite!(&[&harness::Vt100, &Ghostty]);
