//! Splits a byte stream into the units a terminal parser sees.
//!
//! The lexer follows the DEC/ECMA-48 state machine that xterm, VTE, kitty and
//! alacritty implement, but unlike `vte` it keeps the raw bytes of every
//! token. The optimizer relies on that: anything it does not fully understand
//! is written back out byte for byte.
//!
//! Input is treated as UTF-8. 8-bit C1 controls (0x80..=0x9F) are not
//! recognized as introducers because in UTF-8 those bytes are continuation
//! bytes; stray ones are reported as [`Token::Opaque`] so the optimizer treats
//! them as barriers.

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const DEL: u8 = 0x7f;

/// Every byte value, so [`Token::raw`] can return a control as a slice.
static BYTES: [u8; 256] = {
    let mut bytes = [0; 256];
    let mut i = 0;
    while i < 256 {
        bytes[i] = i as u8;
        i += 1;
    }
    bytes
};

/// The kind of a control string other than OSC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrKind {
    Dcs,
    Sos,
    Pm,
    Apc,
}

/// How an OSC string was terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    Bel,
    St,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Csi<'a> {
    pub raw: &'a [u8],
    /// Private marker (`<`, `=`, `>` or `?`) when present.
    pub private: Option<u8>,
    /// Parameter bytes after the private marker.
    pub params: &'a [u8],
    pub intermediates: &'a [u8],
    pub final_byte: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esc<'a> {
    pub raw: &'a [u8],
    pub intermediates: &'a [u8],
    pub final_byte: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc<'a> {
    pub raw: &'a [u8],
    /// Bytes between `ESC ]` and the terminator.
    pub payload: &'a [u8],
    pub terminator: Terminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token<'a> {
    /// Printable text: valid UTF-8 without control characters.
    Text(&'a str),
    /// A single C0 control byte or DEL.
    Control(u8),
    Csi(Csi<'a>),
    Esc(Esc<'a>),
    Osc(Osc<'a>),
    /// A DCS, SOS, PM or APC string terminated by ST.
    Str {
        kind: StrKind,
        raw: &'a [u8],
    },
    /// Bytes whose meaning we do not model: malformed or cancelled
    /// sequences, sequences with controls embedded in them, invalid UTF-8
    /// and C1 code points.
    Opaque {
        raw: &'a [u8],
        /// The bytes opened a control string that only the following `ESC`
        /// closed. Whatever is written next must start with `ESC`, or the
        /// terminal would swallow it into the string.
        open_string: bool,
    },
}

impl<'a> Token<'a> {
    pub fn raw(&self) -> &'a [u8] {
        match *self {
            Token::Text(s) => s.as_bytes(),
            Token::Control(b) => std::slice::from_ref(&BYTES[usize::from(b)]),
            Token::Csi(c) => c.raw,
            Token::Esc(e) => e.raw,
            Token::Osc(o) => o.raw,
            Token::Str { raw, .. } | Token::Opaque { raw, .. } => raw,
        }
    }
}

/// Result of scanning one token.
#[derive(Debug, PartialEq, Eq)]
pub enum Scan<'a> {
    Token(Token<'a>, usize),
    /// The input ends in the middle of a token. Only returned when the caller
    /// said more input may follow.
    NeedMore,
}

/// Scans the token at the start of `input`, which must not be empty.
///
/// When `eof` is false a token cut off by the end of `input` yields
/// [`Scan::NeedMore`]; when it is true the remainder becomes an opaque token.
pub fn scan(input: &[u8], eof: bool) -> Scan<'_> {
    debug_assert!(!input.is_empty());
    let b = input[0];
    match b {
        ESC => scan_escape(input, eof),
        0x00..=0x1f | DEL => Scan::Token(Token::Control(b), 1),
        _ => scan_text(input, eof),
    }
}

/// Returns every token in `input`, treating its end as the end of the stream.
pub fn tokenize(input: &[u8]) -> Vec<Token<'_>> {
    let mut tokens = Vec::with_capacity(estimate_tokens(input));
    let mut pos = 0;
    while pos < input.len() {
        match scan(&input[pos..], true) {
            Scan::Token(token, len) => {
                tokens.push(token);
                pos += len;
            }
            Scan::NeedMore => unreachable!("eof was set"),
        }
    }
    tokens
}

/// A close estimate of how many tokens `input` holds, for sizing vectors
/// up front instead of letting them grow and copy repeatedly. Text and
/// controls mostly sit between escape sequences, so it is twice the number
/// of ESC bytes (counted with SIMD).
pub(crate) fn estimate_tokens(input: &[u8]) -> usize {
    2 * memchr::memchr_iter(ESC, input).count() + 1
}

/// Splits `input` into the raw bytes of each token.
pub fn spans(input: &[u8]) -> Vec<&[u8]> {
    let mut spans = Vec::new();
    let mut pos = 0;
    while pos < input.len() {
        match scan(&input[pos..], true) {
            Scan::Token(_, len) => {
                spans.push(&input[pos..pos + len]);
                pos += len;
            }
            Scan::NeedMore => unreachable!("eof was set"),
        }
    }
    spans
}

fn opaque(raw: &[u8], open_string: bool) -> Scan<'_> {
    Scan::Token(Token::Opaque { raw, open_string }, raw.len())
}

fn truncated(input: &[u8], eof: bool) -> Scan<'_> {
    if eof {
        opaque(input, false)
    } else {
        Scan::NeedMore
    }
}

/// Bytes tested at once by [`find_byte`] and [`text_end`].
const BLOCK: usize = 16;

/// `position` for the first byte matching `pred`, fast on long inputs:
/// whole blocks are tested without an early exit, which the compiler turns
/// into SIMD, and only the block with a match is searched byte by byte.
fn find_byte(bytes: &[u8], pred: impl Fn(u8) -> bool) -> Option<usize> {
    let mut offset = 0;
    // `as_chunks` views the slice as fixed-size arrays plus a remainder.
    for block in bytes.as_chunks::<BLOCK>().0 {
        if block.iter().fold(false, |found, &b| found | pred(b)) {
            break;
        }
        offset += BLOCK;
    }
    bytes[offset..]
        .iter()
        .position(|&b| pred(b))
        .map(|i| offset + i)
}

/// Offset of the first C0 control or DEL in `input` (or its length), and
/// whether any byte before it is outside ASCII.
fn text_end(input: &[u8]) -> (usize, bool) {
    let is_control = |b: u8| b < 0x20 || b == DEL;
    // Like `find_byte`, also noting non-ASCII bytes in the same pass.
    let mut offset = 0;
    let mut non_ascii = false;
    for block in input.as_chunks::<BLOCK>().0 {
        let (control, high) = block.iter().fold((false, false), |(c, h), &b| {
            (c | is_control(b), h | !b.is_ascii())
        });
        if control {
            break;
        }
        non_ascii |= high;
        offset += BLOCK;
    }
    for (i, &b) in input[offset..].iter().enumerate() {
        if is_control(b) {
            return (offset + i, non_ascii);
        }
        non_ascii |= !b.is_ascii();
    }
    (input.len(), non_ascii)
}

/// Offset of the first C1 code point (C2 80 to C2 9F) in `text`.
fn find_c1(text: &[u8]) -> Option<usize> {
    memchr::memchr_iter(0xc2, text)
        .find(|&i| text.get(i + 1).is_some_and(|b| (0x80..=0x9f).contains(b)))
}

fn scan_text(input: &[u8], eof: bool) -> Scan<'_> {
    let (end, non_ascii) = text_end(input);
    let run = &input[..end];
    if !non_ascii {
        // Printable ASCII is valid UTF-8 without C1 code points.
        let text = std::str::from_utf8(run).expect("ASCII is UTF-8");
        return Scan::Token(Token::Text(text), end);
    }
    // `compat` reports where validation failed the way `std` does.
    let (valid, invalid_len) = match simdutf8::compat::from_utf8(run) {
        Ok(s) => (s, 0),
        Err(e) => {
            let valid = std::str::from_utf8(&run[..e.valid_up_to()]).expect("checked by from_utf8");
            match e.error_len() {
                Some(len) => (valid, len),
                // The run ends inside a multi-byte character.
                None if end < input.len() || eof => (valid, run.len() - e.valid_up_to()),
                None if valid.is_empty() => return Scan::NeedMore,
                None => (valid, 0),
            }
        }
    };

    // C1 code points are controls in some terminals; never treat them as text.
    // In UTF-8 they are C2 80 to C2 9F, and C2 never continues a character.
    match find_c1(valid.as_bytes()) {
        Some(0) => opaque(&input[..2], false),
        Some(i) => Scan::Token(Token::Text(&valid[..i]), i),
        None if valid.is_empty() => opaque(&input[..invalid_len], false),
        None => Scan::Token(Token::Text(valid), valid.len()),
    }
}

fn scan_escape(input: &[u8], eof: bool) -> Scan<'_> {
    let Some(&next) = input.get(1) else {
        return truncated(input, eof);
    };
    match next {
        b'[' => scan_csi(input, eof),
        b']' => scan_osc(input, eof),
        b'P' => scan_string(input, eof, StrKind::Dcs),
        b'X' => scan_string(input, eof, StrKind::Sos),
        b'^' => scan_string(input, eof, StrKind::Pm),
        b'_' => scan_string(input, eof, StrKind::Apc),
        _ => scan_esc(input, eof),
    }
}

fn scan_esc(input: &[u8], eof: bool) -> Scan<'_> {
    let mut i = 1;
    while i < input.len() {
        match input[i] {
            0x20..=0x2f => i += 1,
            0x30..=0x7e => {
                let raw = &input[..=i];
                let esc = Esc {
                    raw,
                    intermediates: &input[1..i],
                    final_byte: input[i],
                };
                return Scan::Token(Token::Esc(esc), raw.len());
            }
            // ESC restarts the sequence; the bytes so far are discarded.
            ESC => return opaque(&input[..i], false),
            // CAN and SUB cancel the sequence.
            CAN | SUB => return opaque(&input[..=i], false),
            // Controls inside a sequence execute immediately, and DEL is
            // ignored. Keep the bytes together but don't interpret them.
            0x00..=0x1f | DEL => return scan_until_esc_final(input, i, eof),
            // A non-ASCII byte cannot continue an escape sequence.
            _ => return opaque(&input[..i], false),
        }
    }
    truncated(input, eof)
}

/// Finishes an escape sequence that contains embedded controls.
fn scan_until_esc_final(input: &[u8], from: usize, eof: bool) -> Scan<'_> {
    for i in from..input.len() {
        match input[i] {
            0x30..=0x7e => return opaque(&input[..=i], false),
            ESC => return opaque(&input[..i], false),
            CAN | SUB => return opaque(&input[..=i], false),
            0x80..=0xff => return opaque(&input[..i], false),
            _ => {}
        }
    }
    truncated(input, eof)
}

fn scan_csi(input: &[u8], eof: bool) -> Scan<'_> {
    let mut i = 2;
    let mut private = None;
    if let Some(&b @ b'<'..=b'?') = input.get(i) {
        private = Some(b);
        i += 1;
    }
    let params_start = i;
    // Plain parameter bytes are by far the most common; skip them in a tight
    // loop before handling the rest byte by byte.
    i += input[i..]
        .iter()
        .position(|b| !matches!(b, b'0'..=b';'))
        .unwrap_or(input.len() - i);
    // Set when the sequence is malformed or has controls embedded in it.
    let mut odd = false;
    let mut intermediates_start = None;
    while i < input.len() {
        match input[i] {
            b'0'..=b';' if intermediates_start.is_none() => {}
            b'<'..=b'?' if intermediates_start.is_none() => odd = true,
            0x30..=0x3f => odd = true,
            0x20..=0x2f => {
                intermediates_start.get_or_insert(i);
            }
            0x40..=0x7e => {
                let raw = &input[..=i];
                if odd {
                    return opaque(raw, false);
                }
                let params_end = intermediates_start.unwrap_or(i);
                let csi = Csi {
                    raw,
                    private,
                    params: &input[params_start..params_end],
                    intermediates: &input[params_end..i],
                    final_byte: input[i],
                };
                return Scan::Token(Token::Csi(csi), raw.len());
            }
            ESC => return opaque(&input[..i], false),
            CAN | SUB => return opaque(&input[..=i], false),
            0x00..=0x1f | DEL | 0x80..=0xff => odd = true,
        }
        i += 1;
    }
    truncated(input, eof)
}

fn scan_osc(input: &[u8], eof: bool) -> Scan<'_> {
    let mut odd = false;
    let mut i = 2;
    // Every byte that ends or spoils an OSC string is a C0 control, so skip
    // straight to the next one.
    while let Some(skip) = find_byte(&input[i..], |b| b < 0x20) {
        i += skip;
        match input[i] {
            BEL => {
                let raw = &input[..=i];
                if odd {
                    return opaque(raw, false);
                }
                let osc = Osc {
                    raw,
                    payload: &input[2..i],
                    terminator: Terminator::Bel,
                };
                return Scan::Token(Token::Osc(osc), raw.len());
            }
            ESC => match input.get(i + 1) {
                Some(b'\\') => {
                    let raw = &input[..i + 2];
                    if odd {
                        return opaque(raw, false);
                    }
                    let osc = Osc {
                        raw,
                        payload: &input[2..i],
                        terminator: Terminator::St,
                    };
                    return Scan::Token(Token::Osc(osc), raw.len());
                }
                Some(_) => return opaque(&input[..i], true),
                None => return truncated(input, eof),
            },
            CAN | SUB => return opaque(&input[..=i], false),
            // Terminals ignore other controls inside OSC strings.
            _ => odd = true,
        }
        i += 1;
    }
    truncated(input, eof)
}

fn scan_string(input: &[u8], eof: bool, kind: StrKind) -> Scan<'_> {
    // tmux passthrough doubles every ESC in its payload; only `ESC \` ends it.
    let tmux = kind == StrKind::Dcs && input[2..].starts_with(b"tmux;");
    let mut i = 2;
    // Only these bytes end or cancel the string; skip to the next one.
    while let Some(skip) = memchr::memchr3(ESC, CAN, SUB, &input[i..]) {
        i += skip;
        match input[i] {
            ESC => match input.get(i + 1) {
                Some(b'\\') => {
                    let raw = &input[..i + 2];
                    return Scan::Token(Token::Str { kind, raw }, raw.len());
                }
                Some(&ESC) if tmux => i += 1,
                Some(_) => return opaque(&input[..i], true),
                None => return truncated(input, eof),
            },
            _ => return opaque(&input[..=i], false),
        }
        i += 1;
    }
    truncated(input, eof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &[u8]) -> Vec<String> {
        tokenize(input)
            .iter()
            .map(|t| match t {
                Token::Text(s) => format!("text:{s}"),
                Token::Control(b) => format!("ctl:{b:02x}"),
                Token::Csi(c) => format!(
                    "csi:{}{}{}{}",
                    c.private
                        .map(|p| p as char)
                        .map(String::from)
                        .unwrap_or_default(),
                    String::from_utf8_lossy(c.params),
                    String::from_utf8_lossy(c.intermediates),
                    c.final_byte as char
                ),
                Token::Esc(e) => format!("esc:{}", String::from_utf8_lossy(&e.raw[1..])),
                Token::Osc(o) => format!(
                    "osc:{}:{:?}",
                    String::from_utf8_lossy(o.payload),
                    o.terminator
                ),
                Token::Str { kind, .. } => format!("str:{kind:?}"),
                Token::Opaque { raw, open_string } => {
                    format!(
                        "opaque:{}:{open_string}",
                        String::from_utf8_lossy(raw).escape_debug()
                    )
                }
            })
            .collect()
    }

    #[test]
    fn splits_common_sequences() {
        assert_eq!(
            kinds(b"a\x1b[1;31mb\x1b[?25l\x1b]0;t\x07\x1b(B\r\n"),
            [
                "text:a",
                "csi:1;31m",
                "text:b",
                "csi:?25l",
                "osc:0;t:Bel",
                "esc:(B",
                "ctl:0d",
                "ctl:0a"
            ]
        );
    }

    #[test]
    fn string_terminators() {
        assert_eq!(kinds(b"\x1b]8;;x\x1b\\"), ["osc:8;;x:St"]);
        assert_eq!(kinds(b"\x1bP1$r0m\x1b\\"), ["str:Dcs"]);
        assert_eq!(
            kinds(b"\x1b]0;t\x1b[m"),
            ["opaque:\\u{1b}]0;t:true", "csi:m"]
        );
        assert_eq!(
            kinds(b"\x1bPtmux;\x1b\x1b[1m\x1b\\x"),
            ["str:Dcs", "text:x"]
        );
    }

    #[test]
    fn malformed_sequences_are_opaque() {
        assert_eq!(kinds(b"\x1b[1\n2m"), ["opaque:\\u{1b}[1\\n2m:false"]);
        assert_eq!(
            kinds(b"\x1b[1\x18x"),
            ["opaque:\\u{1b}[1\\u{18}:false", "text:x"]
        );
        assert_eq!(kinds(b"\x1b[1?m"), ["opaque:\\u{1b}[1?m:false"]);
        assert_eq!(kinds(b"\x1b[1"), ["opaque:\\u{1b}[1:false"]);
        assert_eq!(kinds(b"\x1b\x1b[m"), ["opaque:\\u{1b}:false", "csi:m"]);
    }

    #[test]
    fn utf8_and_c1() {
        assert_eq!(
            kinds("é─\u{9b}x".as_bytes()),
            ["text:é─", "opaque:\\u{9b}:false", "text:x"]
        );
        assert_eq!(
            kinds(b"a\xffb"),
            ["text:a", "opaque:\u{fffd}:false", "text:b"]
        );
        assert_eq!(scan("é".as_bytes().split_at(1).0, false), Scan::NeedMore);
    }

    #[test]
    fn finds_bytes_across_blocks() {
        let mut bytes = [b'a'; 40];
        assert_eq!(find_byte(&bytes, |b| b == b'x'), None);
        for at in [0, 15, 16, 17, 31, 32, 39] {
            bytes[at] = b'x';
            assert_eq!(find_byte(&bytes, |b| b == b'x'), Some(at));
            bytes[at] = b'a';
        }
    }

    #[test]
    fn text_end_notes_non_ascii() {
        let long = "a".repeat(20);
        assert_eq!(text_end(long.as_bytes()), (20, false));
        assert_eq!(text_end(format!("{long}\x1b[m").as_bytes()), (20, false));
        assert_eq!(text_end(format!("{long}─\n").as_bytes()), (23, true));
        // A non-ASCII byte after the control does not count.
        assert_eq!(text_end(format!("{long}\n─").as_bytes()), (20, false));
    }

    #[test]
    fn long_strings_skip_to_their_end() {
        let uri = "file:///home/user/a/long/path/to/some/file.rs";
        assert_eq!(
            kinds(format!("\x1b]8;;{uri}\x1b\\x").as_bytes()),
            [format!("osc:8;;{uri}:St"), "text:x".to_string()]
        );
        // A control deep inside makes it opaque; CAN cancels it.
        let odd = format!("\x1b]0;{uri}\t{uri}\x07");
        assert_eq!(kinds(odd.as_bytes()).len(), 1);
        assert!(kinds(odd.as_bytes())[0].starts_with("opaque:"));
        assert_eq!(kinds(format!("\x1bP{uri}\x18x").as_bytes())[1], "text:x");
        assert_eq!(kinds(format!("\x1b_G{uri}\x1b\\").as_bytes()), ["str:Apc"]);
    }

    #[test]
    fn estimates_tokens_from_escapes() {
        assert_eq!(estimate_tokens(b"plain"), 1);
        let input = b"\x1b[1mhi\x1b[m\n";
        assert_eq!(estimate_tokens(input), 5);
        assert_eq!(tokenize(input).len(), 4);
    }
}
