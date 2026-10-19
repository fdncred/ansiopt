//! Plain-English descriptions of tokens, for the diff, explain and HTML
//! views.

use crate::lexer::{Csi, Esc, Osc, StrKind, Token};

/// What kind of thing a token does, for grouping byte counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Printed characters, including REP.
    Text,
    /// SGR: colors and styles.
    Sgr,
    Cursor,
    /// Erasing, inserting, deleting and scrolling.
    Edit,
    Mode,
    Title,
    Hyperlink,
    /// OSC strings other than titles and hyperlinks.
    Osc,
    Charset,
    /// Line feeds and other C0 controls that don't move the cursor
    /// sideways.
    Control,
    /// DCS, APC, PM and SOS strings.
    String,
    /// Malformed or unrecognized bytes.
    Other,
}

impl Category {
    pub const ALL: [Category; 12] = [
        Category::Text,
        Category::Sgr,
        Category::Cursor,
        Category::Edit,
        Category::Mode,
        Category::Title,
        Category::Hyperlink,
        Category::Osc,
        Category::Charset,
        Category::Control,
        Category::String,
        Category::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Text => "text",
            Category::Sgr => "colors and styles (SGR)",
            Category::Cursor => "cursor movement",
            Category::Edit => "erase, insert, scroll",
            Category::Mode => "modes",
            Category::Title => "window titles",
            Category::Hyperlink => "hyperlinks",
            Category::Osc => "other OSC",
            Category::Charset => "character sets",
            Category::Control => "line feeds and controls",
            Category::String => "DCS/APC strings",
            Category::Other => "unrecognized",
        }
    }
}

/// The category of a token.
pub fn category(token: &Token) -> Category {
    match token {
        Token::Text(_) => Category::Text,
        Token::Control(b) => match b {
            b'\r' | 0x08 | b'\t' => Category::Cursor,
            0x0e | 0x0f => Category::Charset,
            _ => Category::Control,
        },
        Token::Csi(c) => csi_category(c),
        Token::Esc(e) => match (e.intermediates, e.final_byte) {
            (b"", b'7' | b'8' | b'D' | b'E' | b'M' | b'H') => Category::Cursor,
            (b"", b'n' | b'o' | b'~' | b'}' | b'|' | b'N' | b'O') => Category::Charset,
            ([first, ..], _) if b"()*+-./".contains(first) => Category::Charset,
            (b"", b'=' | b'>') => Category::Mode,
            (b"#", b'8') => Category::Edit,
            _ => Category::Other,
        },
        Token::Osc(o) => match osc_command(o.payload) {
            b"0" | b"1" | b"2" => Category::Title,
            b"8" => Category::Hyperlink,
            _ => Category::Osc,
        },
        Token::Str { .. } => Category::String,
        Token::Opaque { .. } => Category::Other,
    }
}

fn csi_category(c: &Csi) -> Category {
    match (c.private, c.intermediates, c.final_byte) {
        (None, b"", b'm') => Category::Sgr,
        (None, b"", b'b') => Category::Text,
        (None, b"", b'A'..=b'H' | b'`' | b'a' | b'd' | b'e' | b'f' | b'I' | b'Z' | b's' | b'u') => {
            Category::Cursor
        }
        (None | Some(b'?'), b"", b'J' | b'K')
        | (None, b"", b'@' | b'P' | b'L' | b'M' | b'X' | b'S' | b'T' | b'r') => Category::Edit,
        (_, b"", b'h' | b'l') | (None, b"!", b'p') | (None, b" ", b'q') => Category::Mode,
        _ => Category::Other,
    }
}

/// Describes a token in a few words.
pub fn describe(token: &Token) -> String {
    match token {
        Token::Text(s) => format!("text {:?}", truncate(s, 24)),
        Token::Control(b) => control_name(*b).to_string(),
        Token::Csi(c) => describe_csi(c),
        Token::Esc(e) => describe_esc(e),
        Token::Osc(o) => describe_osc(o),
        Token::Str { kind, raw } => describe_string(*kind, raw),
        Token::Opaque { raw, open_string } => {
            if *open_string {
                "string cut short by the next escape".into()
            } else if raw.first() == Some(&0x1b) {
                "malformed or cancelled sequence".into()
            } else {
                "C1 control or invalid UTF-8".into()
            }
        }
    }
}

/// Bytes as printable text: `ESC` becomes `\e`, other controls `\r`, `\n`,
/// `\t`, `\b` or `\xNN`, and backslashes are doubled.
pub fn visible(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 8);
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\x1b' => out.push_str("\\e"),
                '\r' => out.push_str("\\r"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\x08' => out.push_str("\\b"),
                '\\' => out.push_str("\\\\"),
                '\0'..='\x1f' | '\x7f' | '\u{80}'..='\u{9f}' => {
                    out.push_str(&format!("\\x{:02x}", u32::from(c)));
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

fn lossy(bytes: &[u8], max: usize) -> String {
    truncate(&String::from_utf8_lossy(bytes), max)
}

fn control_name(b: u8) -> &'static str {
    match b {
        0x00 => "NUL",
        0x07 => "bell",
        0x08 => "backspace",
        0x09 => "tab",
        0x0a => "line feed",
        0x0b => "vertical tab",
        0x0c => "form feed",
        0x0d => "carriage return",
        0x0e => "shift out (G1 into GL)",
        0x0f => "shift in (G0 into GL)",
        0x18 => "cancel",
        0x1a => "substitute",
        0x7f => "DEL",
        _ => "control character",
    }
}

/// Parameters as numbers, with an empty parameter as `None`.
fn params(c: &Csi) -> Vec<Option<u32>> {
    if c.params.is_empty() {
        return Vec::new();
    }
    c.params
        .split(|&b| b == b';')
        .map(|p| std::str::from_utf8(p).ok().and_then(|p| p.parse().ok()))
        .collect()
}

fn describe_csi(c: &Csi) -> String {
    let p = params(c);
    let n = |i: usize| p.get(i).copied().flatten().unwrap_or(0).max(1);
    let raw0 = p.first().copied().flatten().unwrap_or(0);
    match (c.private, c.intermediates, c.final_byte) {
        (None, b"", b'm') => describe_sgr(c.params),
        (None, b"", b'A') => format!("cursor up {}", n(0)),
        (None, b"", b'B') => format!("cursor down {}", n(0)),
        (None, b"", b'C') => format!("cursor right {}", n(0)),
        (None, b"", b'D') => format!("cursor left {}", n(0)),
        (None, b"", b'E') => format!("down {}, to column 1", n(0)),
        (None, b"", b'F') => format!("up {}, to column 1", n(0)),
        (None, b"", b'G' | b'`') => format!("column {}", n(0)),
        (None, b"", b'd') => format!("row {}", n(0)),
        (None, b"", b'H' | b'f') => format!("move to row {}, column {}", n(0), n(1)),
        (None, b"", b'a') => format!("cursor right {}", n(0)),
        (None, b"", b'e') => format!("cursor down {}", n(0)),
        (None, b"", b'I') => format!("forward {} tab stops", n(0)),
        (None, b"", b'Z') => format!("back {} tab stops", n(0)),
        (None, b"", b'J') => match raw0 {
            0 => "erase below".into(),
            1 => "erase above".into(),
            2 => "erase screen".into(),
            3 => "erase scrollback".into(),
            _ => "erase display".into(),
        },
        (Some(b'?'), b"", b'J') => "selective erase in display".into(),
        (None, b"", b'K') => match raw0 {
            0 => "erase to end of line".into(),
            1 => "erase to start of line".into(),
            2 => "erase line".into(),
            _ => "erase in line".into(),
        },
        (Some(b'?'), b"", b'K') => "selective erase in line".into(),
        (None, b"", b'@') => format!("insert {} blanks", n(0)),
        (None, b"", b'P') => format!("delete {} characters", n(0)),
        (None, b"", b'L') => format!("insert {} lines", n(0)),
        (None, b"", b'M') => format!("delete {} lines", n(0)),
        (None, b"", b'X') => format!("erase {} characters", n(0)),
        (None, b"", b'S') => format!("scroll up {}", n(0)),
        (None, b"", b'T') => format!("scroll down {}", n(0)),
        (None, b"", b'b') => format!("repeat previous character {}×", n(0)),
        (None, b"", b'r') if p.is_empty() => "scroll region: whole screen".into(),
        (None, b"", b'r') => format!("scroll region rows {}-{}", n(0), n(1)),
        (None, b"", b's') if p.is_empty() => "save cursor".into(),
        (None, b"", b's') => "set left/right margins".into(),
        (None, b"", b'u') if p.is_empty() => "restore cursor".into(),
        (Some(b'?'), b"", fin @ (b'h' | b'l')) => {
            let on = fin == b'h';
            let names: Vec<String> = p
                .iter()
                .map(|m| dec_mode_name(m.unwrap_or(0), on))
                .collect();
            names.join(" · ")
        }
        (None, b"", fin @ (b'h' | b'l')) => {
            let state = if fin == b'h' { "on" } else { "off" };
            let names: Vec<String> = p
                .iter()
                .map(|m| match m.unwrap_or(0) {
                    4 => format!("insert mode {state}"),
                    20 => format!("newline mode {state}"),
                    other => format!("mode {other} {state}"),
                })
                .collect();
            names.join(" · ")
        }
        (None, b"!", b'p') => "soft reset".into(),
        (None, b" ", b'q') => "cursor shape".into(),
        (None, b"", b't') => "window operation".into(),
        (None | Some(b'?'), b"", b'n') => "status report request".into(),
        (_, b"", b'c') => "device attributes request".into(),
        (Some(b'>'), b"", b'm') => "key modifier options".into(),
        (_, b"", b'u') => "keyboard protocol".into(),
        _ => format!("unrecognized CSI {}", crate::describe::visible(&c.raw[2..])),
    }
}

fn dec_mode_name(mode: u32, on: bool) -> String {
    let state = if on { "on" } else { "off" };
    match mode {
        1 => format!("application cursor keys {state}"),
        3 => format!("132 columns {state}"),
        5 => format!("reverse video {state}"),
        6 => format!("origin mode {state}"),
        7 => format!("autowrap {state}"),
        12 => format!("cursor blink {state}"),
        25 if on => "show cursor".into(),
        25 => "hide cursor".into(),
        45 => format!("reverse wraparound {state}"),
        47 | 1047 => format!("alternate screen {state}"),
        69 => format!("left/right margins {state}"),
        1000..=1003 => format!("mouse tracking {state}"),
        1004 => format!("focus events {state}"),
        1005 | 1006 | 1015 | 1016 => format!("mouse encoding {state}"),
        1048 if on => "save cursor".into(),
        1048 => "restore cursor".into(),
        1049 => format!("alternate screen (saving cursor) {state}"),
        2004 => format!("bracketed paste {state}"),
        2026 => format!("synchronized output {state}"),
        other => format!("mode ?{other} {state}"),
    }
}

const COLOR_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// Describes SGR parameters, including the ones the optimizer doesn't model.
pub fn describe_sgr(params: &[u8]) -> String {
    if params.is_empty() {
        return "reset".into();
    }
    let parts: Vec<&[u8]> = params.split(|&b| b == b';').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let part = parts[i];
        i += 1;
        if part.contains(&b':') {
            out.push(describe_sgr_subparams(part));
            continue;
        }
        let Some(code) = std::str::from_utf8(part).ok().and_then(|s| {
            if s.is_empty() {
                Some(0)
            } else {
                s.parse().ok()
            }
        }) else {
            out.push(format!("SGR {}", String::from_utf8_lossy(part)));
            continue;
        };
        let word = match code {
            0 => "reset".to_string(),
            1 => "bold".into(),
            2 => "faint".into(),
            3 => "italic".into(),
            4 => "underline".into(),
            5 => "blink".into(),
            6 => "rapid blink".into(),
            7 => "reverse".into(),
            8 => "hidden".into(),
            9 => "strikethrough".into(),
            10 => "primary font".into(),
            11..=19 => format!("font {}", code - 10),
            20 => "fraktur".into(),
            21 => "double underline".into(),
            22 => "normal intensity".into(),
            23 => "no italic".into(),
            24 => "no underline".into(),
            25 => "no blink".into(),
            26 => "proportional".into(),
            27 => "no reverse".into(),
            28 => "not hidden".into(),
            29 => "no strikethrough".into(),
            30..=37 => format!("fg {}", COLOR_NAMES[(code - 30) as usize]),
            39 => "default fg".into(),
            40..=47 => format!("bg {}", COLOR_NAMES[(code - 40) as usize]),
            49 => "default bg".into(),
            50 => "not proportional".into(),
            51 => "framed".into(),
            52 => "encircled".into(),
            53 => "overline".into(),
            54 => "not framed".into(),
            55 => "no overline".into(),
            59 => "default underline color".into(),
            73 => "superscript".into(),
            74 => "subscript".into(),
            75 => "no super/subscript".into(),
            90..=97 => format!("fg bright {}", COLOR_NAMES[(code - 90) as usize]),
            100..=107 => format!("bg bright {}", COLOR_NAMES[(code - 100) as usize]),
            38 | 48 | 58 => {
                let what = match code {
                    38 => "fg",
                    48 => "bg",
                    _ => "underline color",
                };
                let arg = |k: usize| {
                    parts
                        .get(i + k)
                        .and_then(|p| std::str::from_utf8(p).ok())
                        .and_then(|p| p.parse::<u32>().ok())
                };
                match arg(0) {
                    Some(5) => {
                        let index = arg(1);
                        i += 2;
                        match index {
                            Some(n) => format!("{what} {}", indexed_color_name(n)),
                            None => format!("{what} (malformed)"),
                        }
                    }
                    Some(2) => {
                        let rgb = (arg(1), arg(2), arg(3));
                        i += 4;
                        match rgb {
                            (Some(r), Some(g), Some(b)) => {
                                format!("{what} #{r:02x}{g:02x}{b:02x}")
                            }
                            _ => format!("{what} (malformed)"),
                        }
                    }
                    _ => format!("{what} (unrecognized form)"),
                }
            }
            other => format!("SGR {other}"),
        };
        out.push(word);
    }
    out.join(" · ")
}

fn describe_sgr_subparams(part: &[u8]) -> String {
    let subs: Vec<Option<u32>> = part
        .split(|&b| b == b':')
        .map(|p| std::str::from_utf8(p).ok().and_then(|p| p.parse().ok()))
        .collect();
    match subs.as_slice() {
        [Some(4), Some(0)] => "no underline".into(),
        [Some(4), Some(1)] => "underline".into(),
        [Some(4), Some(2)] => "double underline".into(),
        [Some(4), Some(3)] => "curly underline".into(),
        [Some(4), Some(4)] => "dotted underline".into(),
        [Some(4), Some(5)] => "dashed underline".into(),
        [Some(code @ (38 | 48 | 58)), rest @ ..] => {
            let what = match code {
                38 => "fg",
                48 => "bg",
                _ => "underline color",
            };
            match rest {
                [Some(5), Some(n)] => format!("{what} {}", indexed_color_name(*n)),
                [Some(2), .., Some(r), Some(g), Some(b)] => {
                    format!("{what} #{r:02x}{g:02x}{b:02x}")
                }
                _ => format!("{what} (colon form)"),
            }
        }
        _ => format!("SGR {}", String::from_utf8_lossy(part)),
    }
}

fn indexed_color_name(n: u32) -> String {
    match n {
        0..=7 => COLOR_NAMES[n as usize].to_string(),
        8..=15 => format!("bright {}", COLOR_NAMES[(n - 8) as usize]),
        _ => format!("color {n}"),
    }
}

fn charset_name(final_byte: u8) -> &'static str {
    match final_byte {
        b'B' => "ASCII",
        b'0' => "DEC line drawing",
        b'A' => "UK",
        b'<' => "DEC supplemental",
        b'>' => "DEC technical",
        _ => "another set",
    }
}

fn describe_esc(e: &Esc) -> String {
    match (e.intermediates, e.final_byte) {
        (b"", b'7') => "save cursor".into(),
        (b"", b'8') => "restore cursor".into(),
        (b"", b'c') => "full reset".into(),
        (b"", b'D') => "index (down, scrolling)".into(),
        (b"", b'E') => "next line".into(),
        (b"", b'M') => "reverse index (up, scrolling)".into(),
        (b"", b'H') => "set tab stop".into(),
        (b"", b'=') => "application keypad".into(),
        (b"", b'>') => "numeric keypad".into(),
        (b"", b'\\') => "string terminator".into(),
        (b"", b'N') => "single shift G2".into(),
        (b"", b'O') => "single shift G3".into(),
        (b"", b'n') => "G2 into GL".into(),
        (b"", b'o') => "G3 into GL".into(),
        (b"", b'~') => "G1 into GR".into(),
        (b"", b'}') => "G2 into GR".into(),
        (b"", b'|') => "G3 into GR".into(),
        (b"#", b'8') => "screen alignment test".into(),
        (b"#", b'3'..=b'6') => "line size".into(),
        ([first, ..], fin) if b"()*+-./".contains(first) => {
            let slot = match first {
                b'(' => 0,
                b')' | b'-' => 1,
                b'*' | b'.' => 2,
                _ => 3,
            };
            format!("G{slot} = {}", charset_name(fin))
        }
        _ => format!("unrecognized ESC {}", visible(&e.raw[1..])),
    }
}

fn osc_command(payload: &[u8]) -> &[u8] {
    match payload.iter().position(|&b| b == b';') {
        Some(i) => &payload[..i],
        None => payload,
    }
}

fn describe_osc(o: &Osc) -> String {
    let command = osc_command(o.payload);
    let rest = o.payload.get(command.len() + 1..).unwrap_or(b"");
    match command {
        b"0" => format!("title and icon: {:?}", lossy(rest, 40)),
        b"1" => format!("icon name: {:?}", lossy(rest, 40)),
        b"2" => format!("title: {:?}", lossy(rest, 40)),
        b"8" => {
            let uri = rest
                .iter()
                .position(|&b| b == b';')
                .map_or(&b""[..], |i| &rest[i + 1..]);
            if uri.is_empty() {
                "end hyperlink".into()
            } else {
                format!("hyperlink to {}", lossy(uri, 60))
            }
        }
        b"4" => "set palette color".into(),
        b"7" => format!("working directory {}", lossy(rest, 60)),
        b"9" => "notification or progress".into(),
        b"10" => "default foreground color".into(),
        b"11" => "default background color".into(),
        b"12" => "cursor color".into(),
        b"52" => "clipboard".into(),
        b"104" => "reset palette color".into(),
        b"133" => format!("shell integration mark {}", lossy(rest, 8)),
        b"633" => "VS Code shell integration".into(),
        b"1337" => "iTerm2 extension".into(),
        _ => format!("OSC {}", lossy(command, 12)),
    }
}

fn describe_string(kind: StrKind, raw: &[u8]) -> String {
    let body = &raw[2..];
    match kind {
        StrKind::Dcs if body.starts_with(b"tmux;") => "tmux passthrough".into(),
        StrKind::Dcs if body.starts_with(b"$q") => "setting query (DECRQSS)".into(),
        StrKind::Dcs if body.starts_with(b"+q") => "terminfo query (XTGETTCAP)".into(),
        StrKind::Dcs => {
            let header_end = body
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
                .unwrap_or(0);
            if body.get(header_end) == Some(&b'q') && !body[..header_end].contains(&b'$') {
                "sixel image".into()
            } else {
                "DCS string".into()
            }
        }
        StrKind::Apc if body.starts_with(b"G") => "kitty graphics".into(),
        StrKind::Apc => "APC string".into(),
        StrKind::Pm => "privacy message".into(),
        StrKind::Sos => "SOS string".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn desc(bytes: &[u8]) -> Vec<String> {
        tokenize(bytes).iter().map(describe).collect()
    }

    #[test]
    fn describes_sgr() {
        assert_eq!(describe_sgr(b""), "reset");
        assert_eq!(describe_sgr(b";1;31"), "reset · bold · fg red");
        assert_eq!(
            describe_sgr(b"38;5;196;48;2;0;16;255"),
            "fg color 196 · bg #0010ff"
        );
        assert_eq!(
            describe_sgr(b"4:3;58:2::1:2:3"),
            "curly underline · underline color #010203"
        );
        assert_eq!(
            describe_sgr(b"93;100"),
            "fg bright yellow · bg bright black"
        );
    }

    #[test]
    fn describes_other_sequences() {
        assert_eq!(
            desc(b"\x1b[H\x1b[5;3H\x1b[K\x1b[?25l\x1b[?1049;2004h\r\n"),
            [
                "move to row 1, column 1",
                "move to row 5, column 3",
                "erase to end of line",
                "hide cursor",
                "alternate screen (saving cursor) on · bracketed paste on",
                "carriage return",
                "line feed"
            ]
        );
        assert_eq!(
            desc(b"\x1b]0;hi\x07\x1b]8;;http://x\x1b\\\x1b]8;;\x07\x1b(0"),
            [
                "title and icon: \"hi\"",
                "hyperlink to http://x",
                "end hyperlink",
                "G0 = DEC line drawing"
            ]
        );
    }

    #[test]
    fn categories() {
        let cats: Vec<Category> = tokenize(b"a\x1b[1m\x1b[2J\r\n\x1b[5b\x1b]2;t\x07")
            .iter()
            .map(category)
            .collect();
        assert_eq!(
            cats,
            [
                Category::Text,
                Category::Sgr,
                Category::Edit,
                Category::Cursor,
                Category::Control,
                Category::Text,
                Category::Title
            ]
        );
    }

    #[test]
    fn visible_escapes() {
        assert_eq!(
            visible(b"\x1b[1m\\x\r\n\x07\xff"),
            "\\e[1m\\\\x\\r\\n\\x07\\xff"
        );
    }
}
