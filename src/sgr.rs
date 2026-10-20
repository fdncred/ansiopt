//! The SGR (Select Graphic Rendition) model: parsing parameters into a pen
//! state and encoding the shortest transition between two pen states.
//!
//! Every attribute is tri-state: `None` means "unknown", which is how the
//! stream starts unless the caller promises a reset terminal. An unknown
//! attribute is never touched by the encoder, so whatever the terminal had
//! before the stream survives exactly as it would have with the original
//! input.
//!
//! The model only covers codes that are safe to reorder and drop on any
//! terminal. A terminal may ignore a whole attribute (vt100-style emulators
//! ignore blink and strikethrough), but it must not ignore just some of the
//! codes that set it: dropping `ESC[4m` because a later `ESC[21m` "replaces"
//! it is wrong on a terminal that ignores 21. So these are left out and the
//! optimizer passes them through as barriers:
//!
//! - colon sub-parameters (`4:3`, `38:2::r:g:b`): ignored by some terminals,
//!   read as `;` by others (turning `4:3` into underline plus italic);
//! - `21` (double underline, or "bold off" on older terminals), `6` (rapid
//!   blink), `20` (fraktur), `60..=65` (ideograms);
//! - `58`/`59` (underline color): terminals without it read `58;5;0` as
//!   "blink, reset".
//!
//! Colors keep the spelling the input used (`38;5;1` stays distinct from `31`)
//! because terminals disagree on whether those are equivalent.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    /// `30..=37` / `40..=47`.
    Basic(u8),
    /// `90..=97` / `100..=107`.
    Bright(u8),
    /// `38;5;n`.
    Indexed(u8),
    /// `38;2;r;g;b`.
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Off,
    Framed,
    Encircled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Off,
    Super,
    Sub,
}

/// Bold and faint are tracked together. Terminals disagree on whether they
/// combine (xterm, VTE, kitty) or replace each other (vt100-style), so when
/// both are on we also remember which came last and emit them in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intensity {
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    /// Meaningful only when both are `Some(true)`: faint was set after bold.
    pub dim_last: bool,
}

impl Intensity {
    const UNKNOWN: Self = Intensity {
        bold: None,
        dim: None,
        dim_last: false,
    };
    const NORMAL: Self = Intensity {
        bold: Some(false),
        dim: Some(false),
        dim_last: false,
    };

    fn set_bold(&mut self) {
        self.bold = Some(true);
        self.dim_last = false;
    }

    fn set_dim(&mut self) {
        self.dim = Some(true);
        self.dim_last = self.bold == Some(true);
    }

    fn both(&self) -> bool {
        self.bold == Some(true) && self.dim == Some(true)
    }

    fn eq_effective(&self, other: &Self) -> bool {
        self.bold == other.bold
            && self.dim == other.dim
            && (!self.both() || self.dim_last == other.dim_last)
    }
}

/// Graphic rendition state. `None` fields are unknown.
#[derive(Debug, Clone, Copy)]
pub struct Pen {
    pub intensity: Intensity,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub blink: Option<bool>,
    pub inverse: Option<bool>,
    pub hidden: Option<bool>,
    pub strike: Option<bool>,
    /// Alternate font number, 0 for the primary font (`10`).
    pub font: Option<u8>,
    pub proportional: Option<bool>,
    pub frame: Option<Frame>,
    pub overline: Option<bool>,
    pub script: Option<Script>,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

impl PartialEq for Pen {
    fn eq(&self, other: &Self) -> bool {
        // Colors and intensity differ most often, so they go first.
        self.fg == other.fg
            && self.bg == other.bg
            && self.intensity.eq_effective(&other.intensity)
            && self.italic == other.italic
            && self.underline == other.underline
            && self.blink == other.blink
            && self.inverse == other.inverse
            && self.hidden == other.hidden
            && self.strike == other.strike
            && self.font == other.font
            && self.proportional == other.proportional
            && self.frame == other.frame
            && self.overline == other.overline
            && self.script == other.script
    }
}

/// Bit set naming pen fields, used to describe which attributes an operation
/// makes visible.
pub type Mask = u32;

pub const INTENSITY: Mask = 1 << 0;
pub const ITALIC: Mask = 1 << 1;
pub const UNDERLINE: Mask = 1 << 2;
pub const BLINK: Mask = 1 << 3;
pub const INVERSE: Mask = 1 << 4;
pub const HIDDEN: Mask = 1 << 5;
pub const STRIKE: Mask = 1 << 6;
pub const FONT: Mask = 1 << 7;
pub const PROPORTIONAL: Mask = 1 << 8;
pub const FRAME: Mask = 1 << 9;
pub const OVERLINE: Mask = 1 << 10;
pub const SCRIPT: Mask = 1 << 11;
pub const FG: Mask = 1 << 12;
pub const BG: Mask = 1 << 13;
pub const ALL: Mask = (1 << 14) - 1;

/// Attributes that are visible on a blank cell when reverse video is off.
pub const BLANK_VISIBLE: Mask =
    BG | UNDERLINE | STRIKE | OVERLINE | FRAME | HIDDEN | INVERSE | PROPORTIONAL;

impl Pen {
    pub const UNKNOWN: Pen = Pen {
        intensity: Intensity::UNKNOWN,
        italic: None,
        underline: None,
        blink: None,
        inverse: None,
        hidden: None,
        strike: None,
        font: None,
        proportional: None,
        frame: None,
        overline: None,
        script: None,
        fg: None,
        bg: None,
    };

    pub const DEFAULT: Pen = Pen {
        intensity: Intensity::NORMAL,
        italic: Some(false),
        underline: Some(false),
        blink: Some(false),
        inverse: Some(false),
        hidden: Some(false),
        strike: Some(false),
        font: Some(0),
        proportional: Some(false),
        frame: Some(Frame::Off),
        overline: Some(false),
        script: Some(Script::Off),
        fg: Some(Color::Default),
        bg: Some(Color::Default),
    };

    /// True when no attribute is unknown.
    pub fn is_known(&self) -> bool {
        self.intensity.bold.is_some()
            && self.intensity.dim.is_some()
            && self.italic.is_some()
            && self.underline.is_some()
            && self.blink.is_some()
            && self.inverse.is_some()
            && self.hidden.is_some()
            && self.strike.is_some()
            && self.font.is_some()
            && self.proportional.is_some()
            && self.frame.is_some()
            && self.overline.is_some()
            && self.script.is_some()
            && self.fg.is_some()
            && self.bg.is_some()
    }

    /// Copies the fields in `mask` from `src`.
    pub fn copy_from(&mut self, src: &Pen, mask: Mask) {
        if mask & INTENSITY != 0 {
            self.intensity = src.intensity;
        }
        macro_rules! copy {
            ($($bit:ident => $field:ident),*) => {$(
                if mask & $bit != 0 {
                    self.$field = src.$field;
                }
            )*};
        }
        copy!(ITALIC => italic, UNDERLINE => underline, BLINK => blink, INVERSE => inverse,
            HIDDEN => hidden, STRIKE => strike, FONT => font, PROPORTIONAL => proportional,
            FRAME => frame, OVERLINE => overline, SCRIPT => script, FG => fg, BG => bg);
    }

    /// Keeps the fields that equal `other` and marks the rest unknown. Used
    /// after operations that may or may not have replaced the pen with
    /// `other`.
    pub fn merge_uncertain(&mut self, other: &Pen) {
        if !self.intensity.eq_effective(&other.intensity) {
            self.intensity = Intensity::UNKNOWN;
        }
        macro_rules! merge {
            ($($field:ident),*) => {$(
                if self.$field != other.$field {
                    self.$field = None;
                }
            )*};
        }
        merge!(
            italic,
            underline,
            blink,
            inverse,
            hidden,
            strike,
            font,
            proportional,
            frame,
            overline,
            script,
            fg,
            bg
        );
    }

    /// Applies SGR parameters. Returns `false`, leaving the pen in an
    /// unspecified state, if the parameters contain anything this model does
    /// not cover.
    pub fn apply(&mut self, params: &[u8]) -> bool {
        // A trailing empty parameter is a reset to xterm and ignored by
        // Ghostty.
        if params.ends_with(b";") {
            return false;
        }
        // A lazy iterator over the parameters: extended colors pull their
        // arguments from it as they go.
        let mut params = Params {
            bytes: params,
            done: false,
        };
        while let Some(param) = params.next() {
            let Some(code) = param.value else {
                return false;
            };
            match code {
                0 => *self = Pen::DEFAULT,
                1 => self.intensity.set_bold(),
                2 => self.intensity.set_dim(),
                3 => self.italic = Some(true),
                4 => self.underline = Some(true),
                5 => self.blink = Some(true),
                7 => self.inverse = Some(true),
                8 => self.hidden = Some(true),
                9 => self.strike = Some(true),
                10..=19 => self.font = Some((code - 10) as u8),
                22 => self.intensity = Intensity::NORMAL,
                23 => self.italic = Some(false),
                24 => self.underline = Some(false),
                25 => self.blink = Some(false),
                26 => self.proportional = Some(true),
                27 => self.inverse = Some(false),
                28 => self.hidden = Some(false),
                29 => self.strike = Some(false),
                30..=37 => self.fg = Some(Color::Basic((code - 30) as u8)),
                39 => self.fg = Some(Color::Default),
                40..=47 => self.bg = Some(Color::Basic((code - 40) as u8)),
                49 => self.bg = Some(Color::Default),
                50 => self.proportional = Some(false),
                51 => self.frame = Some(Frame::Framed),
                52 => self.frame = Some(Frame::Encircled),
                53 => self.overline = Some(true),
                54 => self.frame = Some(Frame::Off),
                55 => self.overline = Some(false),
                73 => self.script = Some(Script::Super),
                74 => self.script = Some(Script::Sub),
                75 => self.script = Some(Script::Off),
                90..=97 => self.fg = Some(Color::Bright((code - 90) as u8)),
                100..=107 => self.bg = Some(Color::Bright((code - 100) as u8)),
                38 | 48 => {
                    let color = match params.next().and_then(|p| p.value) {
                        Some(5) => {
                            let Some(n) = params.next().and_then(Param::byte) else {
                                return false;
                            };
                            Color::Indexed(n)
                        }
                        Some(2) => {
                            // Tuple fields are evaluated left to right, so a
                            // missing component leaves the rest `None` too.
                            let (Some(r), Some(g), Some(b)) = (
                                params.next().and_then(Param::byte),
                                params.next().and_then(Param::byte),
                                params.next().and_then(Param::byte),
                            ) else {
                                return false;
                            };
                            Color::Rgb(r, g, b)
                        }
                        _ => return false,
                    };
                    if code == 38 {
                        self.fg = Some(color);
                    } else {
                        self.bg = Some(color);
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

/// One SGR parameter, parsed while splitting.
#[derive(Debug, Clone, Copy)]
struct Param {
    /// The decimal value, `None` for anything but at most nine digits
    /// (sub-parameters included). An empty parameter is 0.
    value: Option<u32>,
    empty: bool,
}

impl Param {
    /// The value of a parameter that must be present and fit in a byte.
    fn byte(self) -> Option<u8> {
        if self.empty {
            return None;
        }
        self.value.and_then(|n| u8::try_from(n).ok())
    }
}

/// Splits parameters at `;` and parses each one in the same pass.
struct Params<'a> {
    bytes: &'a [u8],
    done: bool,
}

impl Iterator for Params<'_> {
    type Item = Param;

    fn next(&mut self) -> Option<Param> {
        if self.done {
            return None;
        }
        let (mut value, mut digits, mut valid) = (0u32, 0, true);
        let mut rest = self.bytes.iter();
        loop {
            match rest.next() {
                None => {
                    self.done = true;
                    break;
                }
                Some(b';') => break,
                Some(&b) => {
                    digits += 1;
                    valid &= b.is_ascii_digit();
                    // Wrapping: the value is discarded unless every byte
                    // was a digit and there were at most nine of them.
                    value = value
                        .wrapping_mul(10)
                        .wrapping_add(u32::from(b.wrapping_sub(b'0')));
                }
            }
        }
        self.bytes = rest.as_slice();
        Some(Param {
            value: (valid && digits <= 9).then_some(value),
            empty: digits == 0,
        })
    }
}

/// One SGR parameter (possibly several `;`-separated numbers, like
/// `38;5;196`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Code {
    Num(u8),
    /// `base;5;n`.
    Indexed(u8, u8),
    /// `base;2;r;g;b`.
    Rgb(u8, u8, u8, u8),
}

impl Code {
    /// How many `;`-separated numbers it writes, for [`Style::max_params`].
    fn params(self) -> usize {
        match self {
            Code::Num(_) => 1,
            Code::Indexed(..) => 3,
            Code::Rgb(..) => 5,
        }
    }

    fn write(self, out: &mut impl Sink) {
        match self {
            Code::Num(n) => out.num(u32::from(n)),
            Code::Indexed(base, n) => {
                out.num(u32::from(base));
                out.put(b";5;");
                out.num(u32::from(n));
            }
            Code::Rgb(base, r, g, b) => {
                out.num(u32::from(base));
                out.put(b";2;");
                out.num(u32::from(r));
                out.put(b";");
                out.num(u32::from(g));
                out.put(b";");
                out.num(u32::from(b));
            }
        }
    }
}

/// The codes of one transition. [`diff_codes`] writes at most 16: three for
/// intensity, one for each of the other eleven flag-like fields, and one each
/// for the colors.
#[derive(Clone, Copy)]
struct Codes {
    items: [Code; 16],
    len: u8,
    /// Some code turns a field back to its default (`22`, `39`, ...).
    /// Without one, a reset can never be shorter; see [`shortest`].
    clears: bool,
}

impl Codes {
    const fn new() -> Codes {
        Codes {
            items: [Code::Num(0); 16],
            len: 0,
            clears: false,
        }
    }

    fn push(&mut self, code: Code) {
        self.items[usize::from(self.len)] = code;
        self.len += 1;
    }

    fn as_slice(&self) -> &[Code] {
        &self.items[..usize::from(self.len)]
    }
}

/// Where SGR text goes: a buffer, or a counter that measures a candidate
/// without writing it.
trait Sink {
    fn put(&mut self, bytes: &[u8]);
    fn num(&mut self, n: u32);
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }

    fn num(&mut self, n: u32) {
        push_num(self, n);
    }
}

/// Appends `n` in decimal without allocating a `String`.
pub(crate) fn push_num(out: &mut Vec<u8>, mut n: u32) {
    // Nearly every number written (SGR codes, color components, cursor
    // positions) is below 1000. Fixed-size appends compile to plain stores
    // instead of a `memcpy` call.
    let digit = |d: u32| b'0' + d as u8;
    if n < 10 {
        return out.push(digit(n));
    }
    if n < 100 {
        return out.extend_from_slice(&[digit(n / 10), digit(n % 10)]);
    }
    if n < 1000 {
        return out.extend_from_slice(&[digit(n / 100), digit(n / 10 % 10), digit(n % 10)]);
    }
    let mut digits = [0; 10];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

/// Counts the bytes that would be written.
struct Measure(usize);

impl Sink for Measure {
    fn put(&mut self, bytes: &[u8]) {
        self.0 += bytes.len();
    }

    fn num(&mut self, n: u32) {
        self.0 += n.checked_ilog10().map_or(1, |d| d as usize + 1);
    }
}

fn color_code(color: &Color, base: u8) -> Code {
    match color {
        Color::Default => Code::Num(base + 1),
        Color::Basic(n) => Code::Num(base - 8 + *n),
        Color::Bright(n) => Code::Num(base + 52 + *n),
        Color::Indexed(n) => Code::Indexed(base, *n),
        Color::Rgb(r, g, b) => Code::Rgb(base, *r, *g, *b),
    }
}

fn flag(on: u8, off: u8) -> impl Fn(&bool) -> Code {
    move |&v| Code::Num(if v { on } else { off })
}

/// Codes that move field-by-field from `from` to `to`, or `None` when that
/// is impossible without disturbing an unknown attribute.
fn diff_codes(from: &Pen, to: &Pen) -> Option<Codes> {
    let mut out = Codes::new();
    intensity_codes(&from.intensity, &to.intensity, &mut out)?;

    macro_rules! field {
        ($field:ident, $encode:expr) => {
            if from.$field != to.$field {
                // An unknown target must equal the (equally unknown) source.
                let value = to.$field.as_ref()?;
                out.push($encode(value));
                out.clears |= to.$field == Pen::DEFAULT.$field;
            }
        };
    }
    field!(italic, flag(3, 23));
    field!(underline, flag(4, 24));
    field!(blink, flag(5, 25));
    field!(inverse, flag(7, 27));
    field!(hidden, flag(8, 28));
    field!(strike, flag(9, 29));
    field!(font, |v: &u8| Code::Num(10 + *v));
    field!(proportional, flag(26, 50));
    field!(frame, |f: &Frame| Code::Num(match f {
        Frame::Off => 54,
        Frame::Framed => 51,
        Frame::Encircled => 52,
    }));
    field!(overline, flag(53, 55));
    field!(script, |s: &Script| Code::Num(match s {
        Script::Off => 75,
        Script::Super => 73,
        Script::Sub => 74,
    }));
    field!(fg, |c: &Color| color_code(c, 38));
    field!(bg, |c: &Color| color_code(c, 48));
    Some(out)
}

fn intensity_codes(from: &Intensity, to: &Intensity, out: &mut Codes) -> Option<()> {
    if from.eq_effective(to) {
        return Some(());
    }
    let needs_clear = (to.bold == Some(false) && from.bold != Some(false))
        || (to.dim == Some(false) && from.dim != Some(false));
    if needs_clear {
        // `22` clears both, so neither may be unknown.
        to.bold?;
        to.dim?;
        out.push(Code::Num(22));
        out.clears = true;
        push_intensity(to, out);
        return Some(());
    }
    let add_bold = to.bold == Some(true) && from.bold != Some(true);
    let add_dim = to.dim == Some(true) && from.dim != Some(true);
    // An unknown target must equal the (equally unknown) source.
    if (to.bold != from.bold && !add_bold) || (to.dim != from.dim && !add_dim) {
        return None;
    }
    let (bold, dim) = (Code::Num(1), Code::Num(2));
    if !to.both() {
        if add_bold {
            out.push(bold);
        }
        if add_dim {
            out.push(dim);
        }
        return Some(());
    }
    // Both end up on. Whichever was set last must also be sent last, so a
    // terminal where they replace each other shows the same one.
    match (add_bold, add_dim) {
        (true, true) => push_intensity(to, out),
        (true, false) => {
            out.push(bold);
            if to.dim_last {
                out.push(dim);
            }
        }
        (false, true) => {
            out.push(dim);
            if !to.dim_last {
                out.push(bold);
            }
        }
        // Only the order changed: repeating the winner is enough.
        (false, false) => out.push(if to.dim_last { dim } else { bold }),
    }
    Some(())
}

/// Pushes the codes that turn on the intensities in `to` from normal.
fn push_intensity(to: &Intensity, out: &mut Codes) {
    let (bold, dim) = (Code::Num(1), Code::Num(2));
    match (to.bold == Some(true), to.dim == Some(true)) {
        (true, true) if to.dim_last => {
            out.push(bold);
            out.push(dim);
        }
        (true, true) => {
            out.push(dim);
            out.push(bold);
        }
        (true, false) => out.push(bold),
        (false, true) => out.push(dim),
        (false, false) => {}
    }
}

/// How SGR sequences are written.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// Write the reset parameter as an empty parameter (`ESC[;1m`).
    pub empty_reset: bool,
    /// Maximum number of parameters in one sequence.
    pub max_params: usize,
}

/// Writes `codes` as SGR sequences, prefixed by a reset when `reset` is set.
fn write_codes(codes: &[Code], reset: bool, style: Style, out: &mut impl Sink) {
    if codes.is_empty() {
        if reset {
            out.put(b"\x1b[m");
        }
        return;
    }
    let max = style.max_params.max(1);
    let mut count = 0;
    out.put(b"\x1b[");
    if reset {
        out.put(if style.empty_reset { b"" } else { b"0" });
        count = 1;
    }
    for &code in codes {
        let n = code.params();
        if count > 0 && count + n > max {
            out.put(b"m\x1b[");
            count = 0;
        }
        if count > 0 {
            out.put(b";");
        }
        code.write(out);
        count += n;
    }
    out.put(b"m");
}

/// Returns the shortest SGR text that takes a terminal from `from` to a
/// state that agrees with `to` on every field in `mask`, together with the
/// resulting state.
///
/// `full_target` is the complete desired pen. A reset is only used when it
/// is fully known, because a reset destroys unknown attributes that could
/// never be restored.
pub fn transition(from: &Pen, full_target: &Pen, mask: Mask, style: Style) -> (String, Pen) {
    let mut text = Vec::new();
    let state = transition_into(from, full_target, mask, style, &mut text);
    let text = String::from_utf8(text).expect("SGR sequences are ASCII");
    (text, state)
}

/// [`transition`], appending the text to `out`.
pub(crate) fn transition_into(
    from: &Pen,
    full_target: &Pen,
    mask: Mask,
    style: Style,
    out: &mut Vec<u8>,
) -> Pen {
    let best = shortest(from, full_target, mask, style);
    write_codes(best.codes.as_slice(), best.reset, style, out);
    best.state
}

/// A candidate encoding for [`transition`]: its codes, whether they follow
/// a reset, and the pen the terminal has afterwards.
#[derive(Clone, Copy)]
struct Candidate {
    codes: Codes,
    reset: bool,
    state: Pen,
}

impl Candidate {
    fn len(&self, style: Style) -> usize {
        let mut len = Measure(0);
        write_codes(self.codes.as_slice(), self.reset, style, &mut len);
        len.0
    }
}

/// The shortest candidate offered so far. Candidates are measured rather
/// than written, and only once there is a choice to make.
struct Shortest {
    style: Style,
    best: Option<Candidate>,
    /// The length of `best`, once something has been compared with it.
    best_len: Option<usize>,
}

impl Shortest {
    /// Keeps `candidate` if it is strictly shorter, so earlier candidates
    /// win ties.
    fn offer(&mut self, candidate: Candidate) {
        let Some(best) = &self.best else {
            self.best = Some(candidate);
            return;
        };
        let style = self.style;
        let best_len = *self.best_len.get_or_insert_with(|| best.len(style));
        let len = candidate.len(style);
        if len < best_len {
            self.best = Some(candidate);
            self.best_len = Some(len);
        }
    }
}

/// Chooses the encoding [`transition`] writes.
fn shortest(from: &Pen, full_target: &Pen, mask: Mask, style: Style) -> Candidate {
    // Any diff writes at least `ESC[Nm`, so a bare `ESC[m` is the shortest
    // way to the default pen. With a partial mask a diff of the masked
    // fields alone may still be empty, so this is for full syncs only.
    if mask == ALL && *full_target == Pen::DEFAULT && *from != Pen::DEFAULT {
        return Candidate {
            codes: Codes::new(),
            reset: true,
            state: Pen::DEFAULT,
        };
    }

    let mut shortest = Shortest {
        style,
        best: None,
        best_len: None,
    };

    // The full target goes first so it wins ties: it leaves less work for
    // later.
    let diff = diff_codes(from, full_target);
    if let Some(codes) = diff {
        shortest.offer(Candidate {
            codes,
            reset: false,
            state: *full_target,
        });
    }
    let known = full_target.is_known();
    // A diff that turns nothing off is a subsequence of the codes after a
    // reset (same fields, same order), and the reset adds a parameter of
    // its own. Inserting parameters never lets `write_codes` split into
    // fewer sequences, so the reset is always longer and need not be
    // measured.
    if known && diff.is_none_or(|d| d.clears) {
        shortest.offer(Candidate {
            codes: diff_codes(&Pen::DEFAULT, full_target).expect("default is known"),
            reset: true,
            state: *full_target,
        });
    }
    // With a partial mask, also try changing only the masked fields.
    if mask != ALL {
        let mut lazy = *from;
        lazy.copy_from(full_target, mask);
        if let Some(codes) = diff_codes(from, &lazy) {
            shortest.offer(Candidate {
                codes,
                reset: false,
                state: lazy,
            });
        }
        if known {
            // After a reset, fields outside the mask may as well stay at
            // their defaults.
            let mut reset_target = Pen::DEFAULT;
            reset_target.copy_from(&lazy, if lazy == *full_target { ALL } else { mask });
            shortest.offer(Candidate {
                codes: diff_codes(&Pen::DEFAULT, &reset_target).expect("default is known"),
                reset: true,
                state: reset_target,
            });
        }
    }
    shortest
        .best
        .expect("the diff to the full target only fails when a reset is allowed")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: Style = Style {
        empty_reset: true,
        max_params: 16,
    };

    fn pen(params: &str) -> Pen {
        let mut p = Pen::DEFAULT;
        assert!(p.apply(params.as_bytes()), "{params}");
        p
    }

    fn sync(from: &Pen, to: &Pen) -> String {
        transition(from, to, ALL, STYLE).0.replace('\x1b', "\\e")
    }

    #[test]
    fn picks_shortest_encoding() {
        assert_eq!(sync(&pen("1;31"), &pen("1;32")), "\\e[32m");
        assert_eq!(sync(&pen("1;3;4;31"), &pen("34")), "\\e[;34m");
        assert_eq!(sync(&pen("1;31"), &Pen::DEFAULT), "\\e[m");
        assert_eq!(sync(&pen("1;2"), &pen("2")), "\\e[;2m");
        assert_eq!(sync(&pen("1;2;31"), &pen("2;31")), "\\e[22;2m");
        assert_eq!(sync(&pen("31"), &pen("31")), "");
    }

    #[test]
    fn unknown_attributes_block_reset() {
        let mut from = Pen::UNKNOWN;
        from.apply(b"1");
        let mut to = from;
        to.apply(b"31");
        assert_eq!(sync(&from, &to), "\\e[31m");
        // Clearing bold needs 22, which is fine: 22 sets both bits.
        to.apply(b"22");
        assert_eq!(sync(&from, &to), "\\e[22;31m");
    }

    #[test]
    fn keeps_intensity_order() {
        assert_eq!(sync(&Pen::DEFAULT, &pen("2;1")), "\\e[2;1m");
        assert_eq!(sync(&pen("1"), &pen("1;2")), "\\e[2m");
        assert_eq!(sync(&pen("2"), &pen("1;2")), "\\e[1;2m");
        assert_eq!(sync(&pen("1;2"), &pen("2;1")), "\\e[1m");
    }

    #[test]
    fn colors_keep_their_spelling() {
        assert_eq!(sync(&Pen::DEFAULT, &pen("38;5;1")), "\\e[38;5;1m");
        assert_eq!(
            sync(&Pen::DEFAULT, &pen("48;2;010;020;030")),
            "\\e[48;2;10;20;30m"
        );
        assert_eq!(sync(&pen("31"), &pen("38;5;1")), "\\e[38;5;1m");
    }

    #[test]
    fn rejects_what_it_does_not_model() {
        for params in [
            "1;", "6", "20", "21", "58;5;1", "59", "60", "38;3;1", "38;5", "38;5;256", "4:3",
            "38:5:1",
        ] {
            let mut p = Pen::DEFAULT;
            assert!(!p.apply(params.as_bytes()), "{params}");
        }
    }

    #[test]
    fn splits_long_sequences() {
        let style = Style {
            empty_reset: true,
            max_params: 4,
        };
        let to = pen("1;3;4;38;2;1;2;3");
        let (text, _) = transition(&Pen::DEFAULT, &to, ALL, style);
        assert_eq!(text.replace('\x1b', "\\e"), "\\e[1;3;4m\\e[38;2;1;2;3m");
    }

    #[test]
    fn writes_numbers() {
        for n in [0, 7, 9, 10, 42, 99, 100, 255, 999, 1000, 9999, u32::MAX] {
            let mut out = Vec::new();
            push_num(&mut out, n);
            assert_eq!(out, n.to_string().as_bytes());
        }
    }

    #[test]
    fn parses_params_in_one_pass() {
        let parse = |params: &[u8]| {
            Params {
                bytes: params,
                done: false,
            }
            .map(|p| (p.value, p.empty))
            .collect::<Vec<_>>()
        };
        assert_eq!(parse(b""), [(Some(0), true)]);
        assert_eq!(
            parse(b"38;;007"),
            [(Some(38), false), (Some(0), true), (Some(7), false)]
        );
        assert_eq!(parse(b"4:3"), [(None, false)]);
        assert_eq!(
            parse(b"123456789;1234567890"),
            [(Some(123_456_789), false), (None, false)]
        );
    }

    #[test]
    fn default_target_is_a_bare_reset() {
        assert_eq!(sync(&pen("1;3;38;2;1;2;3"), &Pen::DEFAULT), "\\e[m");
        let compat = Style {
            empty_reset: false,
            max_params: 2,
        };
        let (text, state) = transition(&pen("4"), &Pen::DEFAULT, ALL, compat);
        assert_eq!(text, "\x1b[m");
        assert_eq!(state, Pen::DEFAULT);
    }

    /// The shortcuts in [`shortest`] pick exactly what measuring the diff
    /// and the reset would, for every sequence length limit.
    #[test]
    fn shortcuts_match_measuring_every_candidate() {
        const CODES: [&str; 14] = [
            "0",
            "1",
            "2",
            "22",
            "3",
            "23",
            "4",
            "24",
            "31",
            "39",
            "44",
            "49",
            "38;5;196",
            "48;2;10;200;3",
        ];
        /// splitmix64, so the test is deterministic without a dependency.
        struct Rng(u64);
        impl Rng {
            fn below(&mut self, n: usize) -> usize {
                self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = self.0;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                ((z ^ (z >> 31)) % n as u64) as usize
            }
        }
        let random_pen = |rng: &mut Rng, start: Pen| {
            let mut p = start;
            for _ in 0..rng.below(5) {
                assert!(p.apply(CODES[rng.below(CODES.len())].as_bytes()));
            }
            p
        };
        let mut rng = Rng(0x1234_5678);
        for _ in 0..2_000 {
            let start = if rng.below(4) == 0 {
                Pen::UNKNOWN
            } else {
                Pen::DEFAULT
            };
            let from = random_pen(&mut rng, start);
            let base = if rng.below(3) == 0 {
                from
            } else {
                Pen::DEFAULT
            };
            let to = random_pen(&mut rng, base);
            if from == to {
                continue;
            }
            for max_params in 1..=6 {
                for empty_reset in [true, false] {
                    let style = Style {
                        empty_reset,
                        max_params,
                    };
                    let measure = |codes: Codes, reset| {
                        let mut len = Measure(0);
                        write_codes(codes.as_slice(), reset, style, &mut len);
                        len.0
                    };
                    let diff = diff_codes(&from, &to).map(|c| measure(c, false));
                    let reset = to
                        .is_known()
                        .then(|| measure(diff_codes(&Pen::DEFAULT, &to).unwrap(), true));
                    let expected = match (diff, reset) {
                        (Some(d), Some(r)) => d.min(r),
                        (d, r) => d.or(r).unwrap(),
                    };
                    let (text, _) = transition(&from, &to, ALL, style);
                    assert_eq!(text.len(), expected, "{from:?} -> {to:?} with {style:?}");
                }
            }
        }
    }

    #[test]
    fn partial_masks_leave_other_fields_alone() {
        let from = pen("31;41");
        let to = pen("32;41");
        let (text, state) = transition(&from, &to, BLANK_VISIBLE, STYLE);
        assert_eq!(text, "");
        assert_eq!(state.fg, Some(Color::Basic(1)));
    }
}
