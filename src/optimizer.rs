//! The streaming optimizer.
//!
//! The optimizer keeps two copies of the state that escape sequences set up
//! for later text: `desired`, what the input has asked for so far, and
//! `emitted`, what the output has actually told the terminal. Sequences that
//! only change that state are absorbed into `desired`. When something
//! visible happens (text is printed, the screen is erased, ...) the
//! attributes it depends on are brought up to date with the shortest
//! encoding of the difference, then the operation is written.
//!
//! Operations whose effect we cannot model act as barriers: everything is
//! synchronized first, the bytes are copied verbatim, and afterwards the
//! state they might have touched is marked unknown.

use std::collections::HashMap;
use std::ops::Range;

use crate::Options;
use crate::lexer::{self, Csi, Esc, Osc, Scan, StrKind, Terminator, Token};
use crate::sgr::{self, Pen};
use crate::trace::{Entry, Fate, Trace};

/// Mask bits beyond the pen fields defined in [`sgr`].
const LINK: u32 = 1 << 20;
const CHARSET: u32 = 1 << 21;
const PEN: u32 = sgr::ALL;
const EVERYTHING: u32 = PEN | LINK | CHARSET;

/// Pen fields every terminal restores on DECRC. Other fields are only
/// trusted after a restore when the saved and current values agree.
const CORE: u32 = sgr::INTENSITY
    | sgr::ITALIC
    | sgr::UNDERLINE
    | sgr::BLINK
    | sgr::INVERSE
    | sgr::HIDDEN
    | sgr::STRIKE
    | sgr::FG
    | sgr::BG;

/// DEC private modes that are independent on/off flags with no effect on
/// the screen contents, so adjacent changes can be merged, reordered and
/// deduplicated.
const FLAG_MODES: &[u32] = &[1, 7, 8, 25, 66, 1004, 1007, 1034, 1036, 1039, 1042, 2004];

/// DEC private modes that do not touch the screen or the pen but must keep
/// their order (mouse modes share one setting in xterm).
/// Also cursor blink (12), which DECSCUSR changes too, and synchronized
/// output (2026), which some parsers only recognize as the exact bytes
/// `ESC[?2026h`.
const QUIET_MODES: &[u32] = &[
    4, 5, 6, 9, 12, 45, 1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016, 2026, 2031, 9001,
];

/// Largest cursor distance we combine; terminals clamp parameters at
/// different limits.
const MAX_MOVE: u32 = 9999;

/// The OSC 8 hyperlink attribute: `None` while unknown, `Some(None)` for no
/// link.
type Link = Option<Option<LinkSpec>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkSpec {
    /// The `params;uri` text.
    spec: Vec<u8>,
    /// Links without an `id` parameter get a new identity from every OSC 8
    /// that opens them (terminals group hover highlighting by identity), so
    /// each such opening is distinct. Zero for links with an `id`.
    serial: u64,
}

/// G0-G3 designations (kept as the raw designating sequence) and the sets
/// invoked into GL and GR. `None` is unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Charsets {
    g: [Option<Vec<u8>>; 4],
    gl: Option<u8>,
    gr: Option<u8>,
}

impl Charsets {
    fn reset() -> Self {
        Charsets {
            gl: Some(0),
            ..Default::default()
        }
    }

    fn assume_reset() -> Self {
        let mut c = Self::reset();
        c.g[0] = Some(b"\x1b(B".to_vec());
        c
    }

    fn merge_uncertain(&mut self, other: &Charsets) {
        for (mine, theirs) in self.g.iter_mut().zip(&other.g) {
            if mine != theirs {
                *mine = None;
            }
        }
        if self.gl != other.gl {
            self.gl = None;
        }
        if self.gr != other.gr {
            self.gr = None;
        }
    }
}

/// State that DECSC saves and later text depends on.
#[derive(Debug, Clone)]
struct State {
    pen: Pen,
    link: Link,
    charsets: Charsets,
    /// Origin mode (DECOM). Not deferred like the rest, so it is always the
    /// same in `desired` and `emitted`; it lives here because DECSC saves it.
    origin: Option<bool>,
}

impl State {
    fn unknown() -> Self {
        State {
            pen: Pen::UNKNOWN,
            link: None,
            charsets: Charsets::default(),
            origin: None,
        }
    }

    fn reset() -> Self {
        State {
            pen: Pen::DEFAULT,
            link: Some(None),
            charsets: Charsets::assume_reset(),
            origin: Some(false),
        }
    }
}

/// Which coordinates a verbatim movement can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// Only the column.
    Column,
    /// Only the row.
    Row,
    Both,
}

/// A pure cursor movement: it changes nothing but the cursor position, so a
/// later absolute movement makes it dead.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Move {
    /// CUU, CUD, CUF, CUB by their final byte.
    Rel {
        dir: u8,
        n: u32,
    },
    /// CHA (`G`) or HPA (`` ` ``).
    Col {
        n: u32,
        fin: u8,
    },
    /// VPA.
    Row {
        n: u32,
    },
    /// CUP (`H`) or HVP (`f`).
    Pos {
        row: u32,
        col: u32,
        fin: u8,
    },
    Cr,
    Bs,
    /// Any other movement, kept verbatim.
    Other {
        raw: Vec<u8>,
        axis: Axis,
    },
}

impl Move {
    /// True for movements that never change the row, so a later absolute
    /// column makes them dead. CUB and BS are excluded: with reverse
    /// wraparound they can move to the previous row. CHA and HPR only
    /// qualify when origin mode is known to be off, because alacritty (CHA)
    /// and Ghostty (HPR) apply the top margin to the row again in origin
    /// mode.
    fn keeps_row(&self, origin_off: bool) -> bool {
        match self {
            Move::Rel { dir, .. } => *dir == b'C',
            Move::Cr => true,
            Move::Col { .. } => origin_off,
            // Ghostty's HPR re-applies the top margin like alacritty's CHA.
            Move::Other { axis, .. } => *axis == Axis::Column && origin_off,
            _ => false,
        }
    }

    /// True for movements that never change the column.
    fn keeps_column(&self) -> bool {
        match self {
            Move::Rel { dir, .. } => matches!(dir, b'A' | b'B'),
            Move::Row { .. } => true,
            Move::Other { axis, .. } => *axis == Axis::Row,
            _ => false,
        }
    }
}

/// Deferred changes of one kind of state, waiting to be written.
#[derive(Debug, Default)]
struct Deferred {
    /// Trace entries whose change hasn't been resolved yet.
    entries: Vec<usize>,
    /// Output written for this kind of state since the oldest of them.
    outputs: Vec<Range<usize>>,
}

/// Records what happens to each token, for `--explain`.
#[derive(Debug, Default)]
struct Tracer {
    entries: Vec<Entry>,
    /// Input offset of the next token.
    pos: usize,
    /// The current token.
    cur: usize,
    /// The input so far, to compare tokens with what was written for them.
    input: Vec<u8>,
    /// Output bytes already handed back to the caller.
    out_base: usize,
    pen: Deferred,
    link: Deferred,
    charset: Deferred,
}

impl Tracer {
    fn set(&mut self, entry: usize, fate: Fate) {
        self.entries[entry].fate = fate;
    }

    fn raw(&self, entry: usize) -> &[u8] {
        &self.input[self.entries[entry].input.clone()]
    }

    /// Settles deferred changes: folded into what was written for them, or
    /// without effect when nothing was.
    fn resolve(deferred: &mut Deferred, entries: &mut [Entry]) {
        for &e in &deferred.entries {
            entries[e].fate = if deferred.outputs.is_empty() {
                Fate::NoEffect {
                    why: "changed nothing by the time it mattered",
                }
            } else {
                Fate::Folded {
                    outputs: deferred.outputs.clone(),
                }
            };
        }
        deferred.entries.clear();
        deferred.outputs.clear();
    }

    fn written(deferred: &mut Deferred, range: Range<usize>) {
        if !deferred.entries.is_empty() {
            deferred.outputs.push(range);
        }
    }

    fn wipe(deferred: &mut Deferred, entries: &mut [Entry], why: &'static str) {
        for &e in &deferred.entries {
            entries[e].fate = Fate::Superseded { why };
        }
        deferred.entries.clear();
        deferred.outputs.clear();
    }
}

/// Streaming minifier. Feed it chunks of a stream and concatenate what it
/// returns.
pub struct Minifier {
    opts: Options,
    /// Unprocessed input: a token cut off at the end of the last chunk.
    pending: Vec<u8>,
    out: Vec<u8>,
    /// Text not yet written to `out`, held so runs can use REP.
    text: String,
    desired: State,
    emitted: State,
    /// What DECSC saved, when we saw it happen.
    saved: Option<State>,
    moves: Vec<Move>,
    /// The trace entries each pending move came from.
    move_sources: Vec<Vec<usize>>,
    /// Pending changes to [`FLAG_MODES`], in order, with the trace entry
    /// each came from.
    modes: Vec<(u32, bool, usize)>,
    /// Known values of [`FLAG_MODES`] in the terminal.
    mode_values: HashMap<u32, bool>,
    title: Option<Vec<u8>>,
    icon: Option<Vec<u8>>,
    /// Left/right margin mode (DECLRMM). Unless it is known to be off, CR
    /// is not simply "column 1" and moving right may stop at a margin.
    lr_margins: Option<bool>,
    /// The scrolling region (DECSTBM) covers the whole screen. Terminals
    /// clamp vertical movement at its margins differently, so movements are
    /// only combined when there are none.
    full_region: Option<bool>,
    /// Reverse wraparound (xterm mode 45), which lets CUB move to the
    /// previous line.
    reverse_wrap: Option<bool>,
    /// Counter for [`LinkSpec::serial`].
    links_opened: u64,
    /// The next character printed after the current token is known not to
    /// be a combining mark, so blank cells left behind stay blank.
    base_follows: bool,
    /// The previous token opened a control string that the next token's
    /// `ESC` closes. The next token is copied verbatim so it still does.
    raw_next: bool,
    max_params: usize,
    trace: Option<Box<Tracer>>,
}

impl Minifier {
    pub fn new(opts: Options) -> Self {
        let start = if opts.assume_reset {
            State::reset()
        } else {
            State::unknown()
        };
        Minifier {
            opts,
            pending: Vec::new(),
            out: Vec::new(),
            text: String::new(),
            desired: start.clone(),
            emitted: start,
            saved: None,
            moves: Vec::new(),
            move_sources: Vec::new(),
            modes: Vec::new(),
            mode_values: HashMap::new(),
            title: None,
            icon: None,
            lr_margins: opts.assume_reset.then_some(false),
            full_region: opts.assume_reset.then_some(true),
            reverse_wrap: opts.assume_reset.then_some(false),
            links_opened: 0,
            base_follows: false,
            raw_next: false,
            max_params: opts.max_params.max(1),
            trace: None,
        }
    }

    /// A minifier that also records what it does with every token; see
    /// [`Minifier::finish_traced`].
    pub fn with_trace(opts: Options) -> Self {
        let mut m = Self::new(opts);
        m.trace = Some(Box::default());
        m
    }

    /// Processes a chunk and returns the output that is ready. State changes
    /// that nothing has used yet are held back; [`Minifier::flush`] writes
    /// them out.
    pub fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(input);
        let data = std::mem::take(&mut self.pending);
        // The output is rarely longer than the input.
        self.out.reserve(data.len());
        let mut tokens = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            match lexer::scan(&data[pos..], false) {
                Scan::Token(token, len) => {
                    // Only visual mode looks ahead at later tokens (see
                    // `process_all`); otherwise each token is processed as
                    // soon as it is scanned.
                    if self.opts.visual {
                        tokens.push(token);
                    } else {
                        self.process_token(token, false);
                    }
                    pos += len;
                }
                Scan::NeedMore => break,
            }
        }
        self.process_all(&tokens);
        self.pending = data[pos..].to_vec();
        self.flush_text();
        self.take_out()
    }

    fn take_out(&mut self) -> Vec<u8> {
        let out = std::mem::take(&mut self.out);
        if let Some(t) = &mut self.trace {
            t.out_base += out.len();
        }
        out
    }

    /// Writes all held-back state so the terminal matches the input so far,
    /// e.g. before the producer pauses. Optimization continues afterwards.
    pub fn flush(&mut self) -> Vec<u8> {
        self.barrier();
        self.flush_text();
        self.take_out()
    }

    /// Ends the stream, returning the remaining output.
    pub fn finish(self) -> Vec<u8> {
        self.finish_traced().0
    }

    /// Ends the stream, returning the remaining output and, for a minifier
    /// made with [`Minifier::with_trace`], what happened to every token.
    pub fn finish_traced(mut self) -> (Vec<u8>, Option<Trace>) {
        let data = std::mem::take(&mut self.pending);
        self.process_all(&lexer::tokenize(&data));
        let out = self.flush();
        let trace = self.trace.map(|t| Trace { entries: t.entries });
        (out, trace)
    }

    fn process_all(&mut self, tokens: &[Token]) {
        // A combining mark lands on a cell written earlier, possibly a blank
        // whose hidden attributes visual mode let drift. After cursor
        // movement, or at the right margin with autowrap off, that isn't
        // even the previous character's cell, so stop relaxing for the rest
        // of the stream (all of it, for a single chunk) once any character
        // that might combine shows up.
        let may_combine = |t: &Token| matches!(t, Token::Text(s) if !s.chars().all(cannot_combine));
        if self.opts.visual && tokens.iter().any(may_combine) {
            self.opts.visual = false;
        }
        // For each token, whether the next character printed after it is
        // known not to be a combining mark, found by walking backwards.
        let mut base_follows = vec![false; tokens.len()];
        let mut next = false;
        for (i, token) in tokens.iter().enumerate().rev() {
            base_follows[i] = next;
            next = match token {
                Token::Text(s) => s.chars().next().is_some_and(cannot_combine),
                t if only_sets_attributes(t) => next,
                // Anything else might move the cursor or print, and a stream
                // ending here might be continued by anything.
                _ => false,
            };
        }
        for (&token, follows) in tokens.iter().zip(base_follows) {
            self.process_token(token, follows);
        }
    }

    /// Processes one token, recording it when tracing. `base_follows` says
    /// whether the next character printed is known not to combine.
    fn process_token(&mut self, token: Token, base_follows: bool) {
        self.base_follows = base_follows;
        if let Some(t) = &mut self.trace {
            let raw = token.raw();
            t.cur = t.entries.len();
            t.entries.push(Entry {
                input: t.pos..t.pos + raw.len(),
                fate: Fate::Pending,
            });
            t.pos += raw.len();
            t.input.extend_from_slice(raw);
        }
        self.process(token);
    }

    fn process(&mut self, token: Token) {
        if std::mem::take(&mut self.raw_next) {
            match token {
                Token::Opaque { raw, open_string } => self.opaque(raw, open_string),
                token => self.opaque(token.raw(), false),
            }
            self.fate(Fate::Barrier {
                why: "ends a string the previous sequence left open",
            });
            return;
        }
        match token {
            Token::Text(s) => self.print(s),
            Token::Control(b) => self.control(b),
            Token::Csi(c) => self.csi(c),
            Token::Esc(e) => self.esc(e),
            Token::Osc(o) => self.osc(o),
            Token::Str { kind, raw } => self.string(kind, raw),
            Token::Opaque { raw, open_string } => self.opaque(raw, open_string),
        }
    }

    // Output plumbing.

    /// Writes bytes, returning where they landed in the whole output.
    fn write(&mut self, bytes: &[u8]) -> Range<usize> {
        self.write_with(|out| out.extend_from_slice(bytes)).0
    }

    /// Lets `f` append to the output, returning where its bytes landed in
    /// the whole output along with what `f` returned.
    fn write_with<T>(&mut self, f: impl FnOnce(&mut Vec<u8>) -> T) -> (Range<usize>, T) {
        self.flush_text();
        let base = self.trace.as_ref().map_or(0, |t| t.out_base);
        let start = self.out.len();
        let value = f(&mut self.out);
        (base + start..base + self.out.len(), value)
    }

    /// Records the fate of the current token.
    fn fate(&mut self, fate: Fate) {
        if let Some(t) = &mut self.trace {
            let cur = t.cur;
            t.set(cur, fate);
        }
    }

    /// Records that the current token was written at `range` as `bytes`.
    fn wrote_token(&mut self, range: Range<usize>, bytes: &[u8]) {
        if let Some(t) = &mut self.trace {
            let raw = t.raw(t.cur);
            let fate = if bytes == raw {
                Fate::Kept
            } else {
                let why = if raw.starts_with(b"\x1b]") {
                    "BEL instead of ESC \\ as terminator"
                } else {
                    "default or zero-padded parameters dropped"
                };
                Fate::Rewritten { output: range, why }
            };
            let cur = t.cur;
            t.set(cur, fate);
        }
    }

    /// Defers the current token's change to the pen, hyperlink or
    /// character sets.
    fn defer(&mut self, which: u32) {
        if let Some(t) = &mut self.trace {
            let cur = t.cur;
            match which {
                LINK => t.link.entries.push(cur),
                CHARSET => t.charset.entries.push(cur),
                _ => t.pen.entries.push(cur),
            }
        }
    }

    fn flush_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        if self.opts.rep {
            write_with_rep(&self.text, &mut self.out);
        } else {
            self.out.extend_from_slice(self.text.as_bytes());
        }
        // Clearing keeps the allocation for the next run of text.
        self.text.clear();
    }

    /// Writes pending cursor movements and mode changes.
    fn flush_pending(&mut self) {
        self.flush_modes();
        if self.moves.is_empty() {
            return;
        }
        let moves = std::mem::take(&mut self.moves);
        let sources = std::mem::take(&mut self.move_sources);
        let mut buf = Vec::new();
        let mut spans = Vec::new();
        for m in &moves {
            let start = buf.len();
            self.encode_move(m, &mut buf);
            spans.push(start..buf.len());
        }
        let at = self.write(&buf);
        if let Some(t) = &mut self.trace {
            for (span, srcs) in spans.into_iter().zip(sources) {
                let range = at.start + span.start..at.start + span.end;
                let bytes = &buf[span];
                for &src in &srcs {
                    let fate = if srcs.len() == 1 && t.raw(src) == bytes {
                        Fate::Kept
                    } else if srcs.len() == 1 {
                        Fate::Rewritten {
                            output: range.clone(),
                            why: "shorter form of the same movement",
                        }
                    } else {
                        Fate::Merged {
                            output: range.clone(),
                        }
                    };
                    t.set(src, fate);
                }
            }
        }
    }

    /// Brings the attributes in `mask` up to date.
    fn sync(&mut self, mask: u32) {
        let pen_mask = mask & PEN;
        if pen_mask != 0 && !agrees(&self.emitted.pen, &self.desired.pen, pen_mask) {
            let style = sgr::Style {
                empty_reset: !self.opts.compat,
                max_params: self.max_params,
            };
            let (from, to) = (self.emitted.pen, self.desired.pen);
            let (range, state) =
                self.write_with(|out| sgr::transition_into(&from, &to, pen_mask, style, out));
            if let Some(t) = &mut self.trace {
                Tracer::written(&mut t.pen, range);
            }
            self.emitted.pen = state;
        }
        if pen_mask == PEN
            && let Some(t) = &mut self.trace
        {
            Tracer::resolve(&mut t.pen, &mut t.entries);
        }
        if mask & LINK != 0 {
            if self.emitted.link != self.desired.link
                && let Some(link) = self.desired.link.clone()
            {
                let mut seq = b"\x1b]8;".to_vec();
                seq.extend_from_slice(link.as_ref().map_or(b";", |l| &l.spec));
                seq.extend_from_slice(self.string_terminator());
                let range = self.write(&seq);
                if let Some(t) = &mut self.trace {
                    Tracer::written(&mut t.link, range);
                }
                self.emitted.link = Some(link);
            }
            if let Some(t) = &mut self.trace {
                Tracer::resolve(&mut t.link, &mut t.entries);
            }
        }
        if mask & CHARSET != 0 {
            if self.emitted.charsets != self.desired.charsets {
                self.sync_charsets();
            }
            if let Some(t) = &mut self.trace {
                Tracer::resolve(&mut t.charset, &mut t.entries);
            }
        }
    }

    fn sync_charsets(&mut self) {
        let desired = self.desired.charsets.clone();
        let mut seq = Vec::new();
        for (slot, want) in desired.g.iter().enumerate() {
            if let Some(raw) = want
                && self.emitted.charsets.g[slot].as_ref() != Some(raw)
            {
                seq.extend_from_slice(raw);
            }
        }
        if let Some(gl) = desired.gl
            && self.emitted.charsets.gl != Some(gl)
        {
            seq.extend_from_slice(match gl {
                0 => b"\x0f",
                1 => b"\x0e",
                2 => b"\x1bn",
                _ => b"\x1bo",
            });
        }
        if let Some(gr) = desired.gr
            && self.emitted.charsets.gr != Some(gr)
        {
            seq.extend_from_slice(match gr {
                1 => b"\x1b~",
                2 => b"\x1b}",
                _ => b"\x1b|",
            });
        }
        let range = self.write(&seq);
        if let Some(t) = &mut self.trace {
            Tracer::written(&mut t.charset, range);
        }
        self.emitted.charsets = desired;
    }

    fn string_terminator(&self) -> &'static [u8] {
        if self.opts.compat { b"\x1b\\" } else { b"\x07" }
    }

    /// Synchronizes everything ahead of an operation we cannot see into.
    fn barrier(&mut self) {
        self.flush_pending();
        self.sync(EVERYTHING);
    }

    /// Forgets everything an unmodeled operation might have changed. Only
    /// valid right after [`Minifier::barrier`], when `desired` and `emitted`
    /// agree.
    fn forget_all(&mut self) {
        self.desired = State::unknown();
        self.emitted = State::unknown();
        self.saved = None;
        self.mode_values.clear();
        self.title = None;
        self.icon = None;
        self.lr_margins = None;
        self.full_region = None;
        self.reverse_wrap = None;
    }

    fn opaque(&mut self, raw: &[u8], open_string: bool) {
        self.barrier();
        self.write(raw);
        self.forget_all();
        self.raw_next = open_string;
        self.fate(Fate::Barrier {
            why: "not something the optimizer models",
        });
    }

    /// Copies the current token through as a barrier, giving the reason.
    fn barrier_token(&mut self, raw: &[u8], why: &'static str) {
        self.barrier();
        self.write(raw);
        self.fate(Fate::Barrier { why });
    }

    /// Writes an operation that does not depend on the pen.
    fn neutral(&mut self, raw: &[u8]) {
        self.flush_pending();
        let range = self.write(raw);
        self.wrote_token(range, raw);
    }

    /// Writes an operation that makes the attributes in `mask` visible.
    fn consume(&mut self, mask: u32, raw: &[u8]) {
        self.flush_pending();
        self.sync(mask);
        let range = self.write(raw);
        self.wrote_token(range, raw);
    }

    fn inverse_involved(&self) -> bool {
        self.desired.pen.inverse != Some(false) || self.emitted.pen.inverse != Some(false)
    }

    /// Attributes that erased or scrolled-in cells take on. Some terminals
    /// copy the whole pen into them; in visual mode only what shows on a
    /// blank cell matters, unless a combining mark may be printed onto one
    /// next.
    fn erase_mask(&self) -> u32 {
        if !self.opts.visual || !self.base_follows || self.inverse_involved() {
            PEN | LINK
        } else {
            sgr::BLANK_VISIBLE | LINK
        }
    }

    // Tokens.

    fn print(&mut self, s: &str) {
        self.fate(Fate::Text);
        self.flush_pending();
        if !self.opts.visual {
            self.sync(EVERYTHING);
            self.text.push_str(s);
            return;
        }
        // A space is only blank if what follows cannot be a combining mark
        // drawn on top of it.
        let base_follows = self.base_follows;
        let blank_at = |i: usize| {
            s.as_bytes()[i] == b' '
                && s[i + 1..]
                    .chars()
                    .next()
                    .map_or(base_follows, cannot_combine)
        };
        let mut start = 0;
        while start < s.len() {
            let blank = blank_at(start);
            let end = s[start..]
                .char_indices()
                .map(|(i, _)| start + i)
                .find(|&i| blank_at(i) != blank)
                .unwrap_or(s.len());
            let mask = if blank && !self.inverse_involved() {
                sgr::BLANK_VISIBLE | LINK | CHARSET
            } else {
                EVERYTHING
            };
            self.sync(mask);
            self.text.push_str(&s[start..end]);
            start = end;
        }
    }

    fn control(&mut self, b: u8) {
        match b {
            b'\r' => self.push_move(Move::Cr),
            0x08 => self.push_move(Move::Bs),
            // Tabs are not plain movements: some terminals record them in
            // the cells they skip (to copy them back as tabs), and a tab at
            // the right edge may wrap and scroll.
            b'\t' => self.consume(self.erase_mask(), &[b]),
            b'\n' | 0x0b | 0x0c => self.consume(self.erase_mask(), &[b]),
            0x0e => {
                self.desired.charsets.gl = Some(1);
                self.defer(CHARSET);
            }
            0x0f => {
                self.desired.charsets.gl = Some(0);
                self.defer(CHARSET);
            }
            // CAN and SUB may print a substitute character.
            0x18 | 0x1a => self.consume(EVERYTHING, &[b]),
            _ => self.neutral(&[b]),
        }
    }

    fn csi(&mut self, c: Csi) {
        let p = c.params;
        match (c.private, c.intermediates, c.final_byte) {
            (None, b"", b'm') => self.sgr(c),
            (None, b"", b'A'..=b'D') => match numbers(p, 1) {
                Some(n) => self.push_move(Move::Rel {
                    dir: c.final_byte,
                    n: at_least_one(&n, 0),
                }),
                None => self.push_move(Move::Other {
                    raw: c.raw.to_vec(),
                    axis: Axis::Both,
                }),
            },
            (None, b"", fin @ (b'G' | b'`')) => match numbers(p, 1) {
                Some(n) => self.push_move(Move::Col {
                    n: at_least_one(&n, 0),
                    fin,
                }),
                None => self.push_move(Move::Other {
                    raw: c.raw.to_vec(),
                    axis: Axis::Both,
                }),
            },
            (None, b"", b'd') => match numbers(p, 1) {
                Some(n) => self.push_move(Move::Row {
                    n: at_least_one(&n, 0),
                }),
                None => self.push_move(Move::Other {
                    raw: c.raw.to_vec(),
                    axis: Axis::Column,
                }),
            },
            (None, b"", fin @ (b'H' | b'f')) => match numbers(p, 2) {
                Some(n) => {
                    let (row, col) = (at_least_one(&n, 0), at_least_one(&n, 1));
                    self.push_move(Move::Pos { row, col, fin });
                }
                None => self.push_move(Move::Other {
                    raw: c.raw.to_vec(),
                    axis: Axis::Both,
                }),
            },
            (None, b"", fin @ (b'a' | b'Z' | b'E' | b'F' | b'e')) => {
                let axis = match fin {
                    b'a' | b'Z' => Axis::Column,
                    b'e' => Axis::Row,
                    _ => Axis::Both,
                };
                self.push_move(Move::Other {
                    raw: c.raw.to_vec(),
                    axis,
                });
            }
            (None | Some(b'?'), b"", b'J' | b'K') => {
                let seq = normalize_single(&c, |n| n == 0);
                self.consume(self.erase_mask(), &seq);
            }
            (None, b"", b'@' | b'P' | b'L' | b'M' | b'X' | b'S' | b'T')
                if numbers(p, 1).is_some() =>
            {
                // Only 1 is the default everywhere: Ghostty treats IL 0
                // and DL 0 as no-ops where xterm treats them as 1.
                let seq = normalize_single(&c, |n| n == 1);
                self.consume(self.erase_mask(), &seq);
            }
            (None, b"", b'b') => {
                let seq = normalize_single(&c, |n| n == 1);
                self.consume(EVERYTHING, &seq);
            }
            (Some(b'?'), b"", b'h' | b'l') => self.dec_mode(c),
            (None, b"", b's') if p.is_empty() => {
                // SCOSC: some terminals share DECSC's slot, some save only
                // the position.
                self.consume(EVERYTHING, c.raw);
                self.saved = None;
            }
            (None, b"", b'u') if p.is_empty() => {
                // SCORC: may or may not restore attributes.
                self.barrier_token(c.raw, "may or may not restore saved attributes");
                self.desired = State::unknown();
                self.emitted = State::unknown();
            }
            (None, b"", b't') => {
                if numbers(p, 3).is_some_and(|n| n.first() == Some(&23)) {
                    self.title = None;
                    self.icon = None;
                }
                self.neutral(c.raw);
            }
            (None, b"!", b'p') => self.soft_reset(c.raw),
            // Mode set/reset, margins, tab clear, reports, cursor style,
            // keyboard protocols and requests: no effect on the pen.
            (None, b"", b'I') => self.consume(self.erase_mask(), c.raw),
            (None, b"", b'r') => {
                // DECSTBM. Only "top 1, bottom default" is known to mean the
                // whole screen; the screen height is unknown.
                let full = numbers(p, 2).is_some_and(|n| {
                    n.first().is_none_or(|&t| t <= 1) && n.get(1).is_none_or(|&b| b == 0)
                });
                self.full_region = full.then_some(true);
                self.neutral(c.raw);
            }
            (None, b"", b'h' | b'l' | b's' | b'g' | b'n' | b'c')
            | (Some(b'?'), b"", b'n' | b's' | b'u')
            | (Some(b'>'), b"", b'c' | b'm' | b'n' | b'q' | b'u')
            | (Some(b'=' | b'<'), b"", b'c' | b'u')
            | (None, b" ", b'q')
            | (None, b"\"", b'q')
            | (None | Some(b'?'), b"$", b'p') => self.neutral(c.raw),
            _ => self.opaque(c.raw, false),
        }
    }

    fn sgr(&mut self, c: Csi) {
        let count = c.params.split(|&b| b == b';').count();
        self.max_params = self.max_params.max(count);
        let mut pen = self.desired.pen;
        if pen.apply(c.params) {
            self.desired.pen = pen;
            self.defer(PEN);
        } else {
            self.barrier_token(
                c.raw,
                "uses SGR forms terminals disagree on (colons, 21, 58, a trailing ;, ...)",
            );
            self.desired.pen = Pen::UNKNOWN;
            self.emitted.pen = Pen::UNKNOWN;
        }
    }

    fn soft_reset(&mut self, raw: &[u8]) {
        // DECSTR resets the pen and character sets where implemented; it may
        // also be ignored, so only values equal to the defaults stay known.
        self.barrier_token(raw, "soft reset: only certain defaults stay known");
        let mut pen = self.desired.pen;
        pen.merge_uncertain(&Pen::DEFAULT);
        let mut charsets = self.desired.charsets.clone();
        charsets.merge_uncertain(&Charsets::reset());
        // DECSTR also turns origin mode off.
        let origin = self.desired.origin.filter(|&o| !o);
        for state in [&mut self.desired, &mut self.emitted] {
            state.pen = pen;
            state.charsets = charsets.clone();
            state.origin = origin;
        }
        self.lr_margins = self.lr_margins.filter(|&m| !m);
        self.full_region = self.full_region.filter(|&f| f);
        self.reverse_wrap = self.reverse_wrap.filter(|&r| !r);
        self.saved = None;
        self.mode_values.clear();
    }

    fn dec_mode(&mut self, c: Csi) {
        let set = c.final_byte == b'h';
        let Some(modes) = numbers(c.params, usize::MAX) else {
            return self.opaque(c.raw, false);
        };
        if !modes.is_empty() && modes.iter().all(|m| FLAG_MODES.contains(m)) {
            self.flush_moves_only();
            let cur = self.trace.as_ref().map_or(0, |t| t.cur);
            let mut queued = false;
            for m in modes {
                if let Some(t) = &mut self.trace {
                    for &(n, _, src) in &self.modes {
                        if n == m {
                            t.set(
                                src,
                                Fate::Superseded {
                                    why: "changed again before it mattered",
                                },
                            );
                        }
                    }
                }
                self.modes.retain(|&(n, _, _)| n != m);
                if self.mode_values.get(&m) != Some(&set) {
                    self.modes.push((m, set, cur));
                    queued = true;
                }
            }
            if !queued {
                self.fate(Fate::NoEffect {
                    why: "the terminal already has these modes",
                });
            }
            return;
        }

        self.flush_pending();
        let screen = |m: &u32| matches!(m, 3 | 47 | 1047 | 1048 | 1049);
        let quiet = |m: &u32| FLAG_MODES.contains(m) || QUIET_MODES.contains(m) || *m == 69;
        if !modes.iter().all(quiet) {
            self.sync(EVERYTHING);
        }
        let range = self.write(c.raw);
        if modes.iter().any(|&m| matches!(m, 47 | 1047 | 1048 | 1049)) {
            self.fate(Fate::Barrier {
                why: "switches screens or saves/restores the cursor",
            });
        } else {
            self.wrote_token(range, c.raw);
        }
        for &m in &modes {
            if FLAG_MODES.contains(&m) {
                self.mode_values.insert(m, set);
            }
        }
        if modes.contains(&6) {
            self.desired.origin = Some(set);
            self.emitted.origin = Some(set);
        }
        if modes.contains(&69) {
            self.lr_margins = Some(set);
        }
        if modes.contains(&45) {
            self.reverse_wrap = Some(set);
        }
        if modes.iter().any(screen) {
            self.saved = None;
        }
        if modes.iter().any(|&m| matches!(m, 47 | 1047 | 1048 | 1049)) {
            // These save or restore the cursor, attributes included, or
            // switch screens, which in Ghostty each keep their own cursor
            // state.
            self.desired = State::unknown();
            self.emitted = State::unknown();
        }
    }

    fn flush_modes(&mut self) {
        if self.modes.is_empty() {
            return;
        }
        let modes = std::mem::take(&mut self.modes);
        let mut seq = Vec::new();
        for value in [true, false] {
            let list: Vec<u32> = modes.iter().filter(|m| m.1 == value).map(|m| m.0).collect();
            for chunk in list.chunks(self.max_params) {
                seq.extend_from_slice(b"\x1b[?");
                let joined: Vec<String> = chunk.iter().map(u32::to_string).collect();
                seq.extend_from_slice(joined.join(";").as_bytes());
                seq.push(if value { b'h' } else { b'l' });
            }
        }
        for &(m, v, _) in &modes {
            self.mode_values.insert(m, v);
        }
        let range = self.write(&seq);
        if let Some(t) = &mut self.trace {
            let first = modes[0].2;
            let single = modes.iter().all(|m| m.2 == first);
            for &(_, _, src) in &modes {
                let fate = if single && t.raw(src) == seq.as_slice() {
                    Fate::Kept
                } else if single {
                    Fate::Rewritten {
                        output: range.clone(),
                        why: "modes the terminal already had were dropped",
                    }
                } else {
                    Fate::Merged {
                        output: range.clone(),
                    }
                };
                t.set(src, fate);
            }
        }
    }

    fn flush_moves_only(&mut self) {
        let modes = std::mem::take(&mut self.modes);
        self.flush_pending();
        self.modes = modes;
    }

    fn esc(&mut self, e: Esc) {
        match (e.intermediates, e.final_byte) {
            (b"", b'7') => {
                self.consume(EVERYTHING, e.raw);
                self.saved = Some(self.desired.clone());
            }
            (b"", b'8') => {
                self.barrier_token(e.raw, "restores the saved cursor and attributes");
                self.restore_cursor();
            }
            (b"", b'c') => self.full_reset(e.raw),
            (b"", b'D' | b'E' | b'M') => self.consume(self.erase_mask(), e.raw),
            (b"", b'N' | b'O') => self.consume(EVERYTHING, e.raw),
            (b"", f @ (b'n' | b'o')) => {
                self.desired.charsets.gl = Some(if f == b'n' { 2 } else { 3 });
                self.defer(CHARSET);
            }
            (b"", f @ (b'~' | b'}' | b'|')) => {
                self.desired.charsets.gr = Some(match f {
                    b'~' => 1,
                    b'}' => 2,
                    _ => 3,
                });
                self.defer(CHARSET);
            }
            ([first, rest @ ..], _) if rest.len() <= 1 && b"()*+-./".contains(first) => {
                let slot = match first {
                    b'(' => 0,
                    b')' | b'-' => 1,
                    b'*' | b'.' => 2,
                    _ => 3,
                };
                self.desired.charsets.g[slot] = Some(e.raw.to_vec());
                self.defer(CHARSET);
            }
            (b"#", b'8') => self.consume(EVERYTHING, e.raw),
            (b"", b'=' | b'>' | b'H' | b'\\' | b'Z')
            | (b"#", b'3'..=b'6')
            | (b" ", b'F' | b'G') => self.neutral(e.raw),
            _ => self.opaque(e.raw, false),
        }
    }

    fn restore_cursor(&mut self) {
        let mut state = self.desired.clone();
        match &self.saved {
            Some(saved) => {
                let mut uncertain = state.pen;
                uncertain.merge_uncertain(&saved.pen);
                state.pen = saved.pen;
                state.pen.copy_from(&uncertain, PEN & !CORE);
                state.charsets.merge_uncertain(&saved.charsets);
                if state.origin != saved.origin {
                    state.origin = None;
                }
            }
            None => {
                state.pen = Pen::UNKNOWN;
                state.charsets = Charsets::default();
                state.origin = None;
            }
        }
        // Whether the hyperlink is saved varies.
        state.link = None;
        self.desired = state.clone();
        self.emitted = state;
    }

    fn full_reset(&mut self, raw: &[u8]) {
        // Pending pen changes and movements are wiped by RIS, so drop them.
        if let Some(t) = &mut self.trace {
            let why = "wiped by the full reset (RIS) that followed";
            for &src in self.move_sources.iter().flatten() {
                t.set(src, Fate::Superseded { why });
            }
            for &(_, _, src) in &self.modes {
                t.set(src, Fate::Superseded { why });
            }
            Tracer::wipe(&mut t.pen, &mut t.entries, why);
            Tracer::wipe(&mut t.charset, &mut t.entries, why);
        }
        self.moves.clear();
        self.move_sources.clear();
        self.modes.clear();
        self.sync(LINK);
        let range = self.write(raw);
        self.wrote_token(range, raw);
        let mut state = State::reset();
        state.link = None;
        state.charsets.g[0] = None;
        self.desired = state.clone();
        self.emitted = state;
        self.saved = None;
        self.mode_values.clear();
        self.title = None;
        self.icon = None;
        self.lr_margins = Some(false);
        self.full_region = Some(true);
        self.reverse_wrap = Some(false);
    }

    fn osc(&mut self, o: Osc) {
        let (command, rest) = match o.payload.iter().position(|&b| b == b';') {
            Some(i) => (&o.payload[..i], Some(&o.payload[i + 1..])),
            None => (o.payload, None),
        };
        // Terminals parse the command as a number, so `08` means 8. Only the
        // canonical spelling is modeled.
        if command.len() > 1 && command[0] == b'0' && command.iter().all(u8::is_ascii_digit) {
            return self.opaque(o.raw, false);
        }
        match (command, rest) {
            // An empty URI with parameters closes the link in some
            // terminals and is ignored by Ghostty, so only `8;;` is a close.
            (b"8", Some(rest))
                if rest.contains(&b';') && (rest == b";" || !rest.ends_with(b";")) =>
            {
                let split = rest.iter().position(|&b| b == b';').unwrap();
                let (params, uri) = (&rest[..split], &rest[split + 1..]);
                self.desired.link = Some(if uri.is_empty() {
                    None
                } else {
                    let has_id = params
                        .split(|&b| b == b':')
                        .any(|p| p.starts_with(b"id=") && p.len() > 3);
                    let serial = if has_id {
                        0
                    } else {
                        self.links_opened += 1;
                        self.links_opened
                    };
                    Some(LinkSpec {
                        spec: rest.to_vec(),
                        serial,
                    })
                });
                self.defer(LINK);
            }
            (b"8", _) => self.opaque(o.raw, false),
            (b"0" | b"1" | b"2", Some(value)) => {
                let icon = command != b"2";
                let title = command != b"1";
                let redundant = (!icon || self.icon.as_deref() == Some(value))
                    && (!title || self.title.as_deref() == Some(value));
                if redundant {
                    self.fate(Fate::NoEffect {
                        why: "the title is already set to this",
                    });
                } else {
                    let seq = self.normalized_osc(o);
                    self.neutral(&seq);
                    if icon {
                        self.icon = Some(value.to_vec());
                    }
                    if title {
                        self.title = Some(value.to_vec());
                    }
                }
            }
            _ => {
                let seq = self.normalized_osc(o);
                if osc_is_quiet(command) {
                    self.neutral(&seq);
                } else {
                    // Others can draw or move the cursor: OSC 133;A starts a
                    // fresh line in Ghostty, OSC 1337 places inline images.
                    self.consume(EVERYTHING, &seq);
                }
            }
        }
    }

    /// The OSC with BEL as terminator, unless it may be a query: terminals
    /// answer with the terminator the query used.
    fn normalized_osc(&self, o: Osc) -> Vec<u8> {
        if self.opts.compat || o.terminator == Terminator::Bel || o.payload.contains(&b'?') {
            return o.raw.to_vec();
        }
        let mut seq = b"\x1b]".to_vec();
        seq.extend_from_slice(o.payload);
        seq.push(0x07);
        seq
    }

    fn string(&mut self, kind: StrKind, raw: &[u8]) {
        match kind {
            StrKind::Dcs if dcs_is_harmless(raw) => self.consume(EVERYTHING, raw),
            StrKind::Dcs => self.opaque(raw, false),
            // Kitty graphics and other APC/PM/SOS payloads leave the pen alone.
            _ => self.neutral(raw),
        }
    }

    // Cursor movement.

    fn push_move(&mut self, m: Move) {
        self.flush_modes();
        let cur = self.trace.as_ref().map_or(0, |t| t.cur);
        let origin_off = self.desired.origin == Some(false);
        let lr_off = self.lr_margins == Some(false);
        match m {
            Move::Pos { .. } => {
                self.drop_trailing(|_| true);
                self.push_tracked(m, cur);
            }
            Move::Cr if origin_off && lr_off => self.push_move(Move::Col { n: 1, fin: b'G' }),
            Move::Cr => {
                // With left/right margins, where CR lands depends on the
                // column, so only an earlier CR is certainly dead.
                if lr_off {
                    self.drop_trailing(|m| m.keeps_row(origin_off));
                } else {
                    self.drop_trailing(|m| *m == Move::Cr);
                }
                self.push_tracked(m, cur);
            }
            Move::Col { n, .. } => {
                self.drop_trailing(|m| m.keeps_row(origin_off));
                match self.moves.last_mut() {
                    Some(Move::Pos { col, .. }) if origin_off => {
                        *col = n;
                        self.merge_source(cur);
                    }
                    Some(last @ &mut Move::Row { n: row }) if origin_off => {
                        *last = Move::Pos {
                            row,
                            col: n,
                            fin: b'H',
                        };
                        self.merge_source(cur);
                    }
                    _ => self.push_tracked(m, cur),
                }
            }
            Move::Row { n } => {
                self.drop_trailing(Move::keeps_column);
                match self.moves.last_mut() {
                    // vt100-style terminals ignore origin mode for VPA.
                    Some(Move::Pos { row, .. }) if origin_off => {
                        *row = n;
                        self.merge_source(cur);
                    }
                    Some(last @ &mut Move::Col { n: col, .. }) if origin_off => {
                        *last = Move::Pos {
                            row: n,
                            col,
                            fin: b'H',
                        };
                        self.merge_source(cur);
                    }
                    _ => self.push_tracked(m, cur),
                }
            }
            Move::Rel { dir, n } => {
                // Where relative movement stops at a margin depends, on some
                // terminals, on whether it started inside the margins, so
                // movements only add up when there are no margins (and CUB
                // cannot wrap backwards).
                let additive = match dir {
                    b'A' | b'B' => self.full_region == Some(true),
                    b'C' => lr_off,
                    _ => lr_off && self.reverse_wrap == Some(false),
                };
                let merged = match self.moves.last_mut() {
                    Some(Move::Rel { dir: d, n: total })
                        if *d == dir && additive && *total + n <= MAX_MOVE =>
                    {
                        *total += n;
                        true
                    }
                    // Moving right from an absolute column is an absolute
                    // column: both stop at the right edge.
                    Some(Move::Pos { col, .. } | Move::Col { n: col, .. })
                        if dir == b'C' && lr_off && *col + n <= MAX_MOVE =>
                    {
                        *col += n;
                        true
                    }
                    _ => false,
                };
                if merged {
                    self.merge_source(cur);
                } else {
                    self.push_tracked(m, cur);
                }
            }
            _ => self.push_tracked(m, cur),
        }
    }

    fn push_tracked(&mut self, m: Move, source: usize) {
        self.moves.push(m);
        self.move_sources.push(vec![source]);
    }

    /// Records that the current token was folded into the last move.
    fn merge_source(&mut self, source: usize) {
        if let Some(srcs) = self.move_sources.last_mut() {
            srcs.push(source);
        }
    }

    /// Drops trailing moves a new one makes pointless.
    fn drop_trailing(&mut self, pred: impl Fn(&Move) -> bool) {
        while self.moves.last().is_some_and(&pred) {
            self.moves.pop();
            let srcs = self.move_sources.pop().unwrap_or_default();
            if let Some(t) = &mut self.trace {
                for src in srcs {
                    t.set(
                        src,
                        Fate::Superseded {
                            why: "a later movement makes it pointless",
                        },
                    );
                }
            }
        }
    }

    fn encode_move(&self, m: &Move, out: &mut Vec<u8>) {
        // CR equals "column 1" only without margins or origin mode in play.
        let cr_is_col1 = self.desired.origin == Some(false) && self.lr_margins == Some(false);
        match m {
            Move::Rel { dir, n } => csi_num(out, *n, 1, *dir),
            Move::Col { n, fin } => {
                if cr_is_col1 && *n == 1 {
                    out.push(b'\r');
                    return;
                }
                let mut direct = Vec::new();
                csi_num(&mut direct, *n, 1, *fin);
                let mut via_cr = vec![b'\r'];
                csi_num(&mut via_cr, n - 1, 1, b'C');
                if cr_is_col1 && via_cr.len() < direct.len() {
                    out.extend(via_cr);
                } else {
                    out.extend(direct);
                }
            }
            Move::Row { n } => csi_num(out, *n, 1, b'd'),
            Move::Pos { row, col, fin } => {
                out.extend_from_slice(b"\x1b[");
                if *row != 1 || (*col != 1 && self.opts.compat) {
                    out.extend_from_slice(row.to_string().as_bytes());
                }
                if *col != 1 {
                    out.push(b';');
                    out.extend_from_slice(col.to_string().as_bytes());
                }
                out.push(*fin);
            }
            Move::Cr => out.push(b'\r'),
            Move::Bs => out.push(0x08),
            Move::Other { raw, .. } => out.extend_from_slice(raw),
        }
    }
}

/// True when `a` and `b` agree on every field in `mask`.
fn agrees(a: &Pen, b: &Pen, mask: u32) -> bool {
    if mask == PEN {
        return a == b;
    }
    let mut merged = *a;
    merged.copy_from(b, mask);
    merged == *a
}

/// Parses up to `max` numeric parameters (empty counts as 0). Returns `None`
/// for subparameters, too many parameters or values above [`MAX_MOVE`].
fn numbers(params: &[u8], max: usize) -> Option<Vec<u32>> {
    if params.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for p in params.split(|&b| b == b';') {
        if out.len() == max || p.len() > 5 || !p.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let n = p.iter().fold(0u32, |n, &d| n * 10 + u32::from(d - b'0'));
        if n > MAX_MOVE {
            return None;
        }
        out.push(n);
    }
    Some(out)
}

/// Parameter `i`, with 0 and missing both meaning 1.
fn at_least_one(n: &[u32], i: usize) -> u32 {
    n.get(i).copied().unwrap_or(0).max(1)
}

/// Writes `CSI n fin`, leaving out `n` when it equals `default`.
fn csi_num(out: &mut Vec<u8>, n: u32, default: u32, fin: u8) {
    out.extend_from_slice(b"\x1b[");
    if n != default {
        out.extend_from_slice(n.to_string().as_bytes());
    }
    out.push(fin);
}

/// Rewrites a single-parameter CSI without leading zeros, dropping the
/// parameter when `is_default` says the terminal would assume it anyway.
fn normalize_single(c: &Csi, is_default: impl Fn(u32) -> bool) -> Vec<u8> {
    let Some(n) = numbers(c.params, 1) else {
        return c.raw.to_vec();
    };
    if n.is_empty() {
        return c.raw.to_vec();
    }
    let mut seq = b"\x1b[".to_vec();
    if let Some(p) = c.private {
        seq.push(p);
    }
    let value = n.first().copied().unwrap_or(0);
    if !is_default(value) {
        seq.extend_from_slice(value.to_string().as_bytes());
    }
    seq.push(c.final_byte);
    seq
}

/// OSC commands that neither draw nor move the cursor: palette and dynamic
/// colors, working directory, pointer shape, clipboard, notifications and
/// progress reports.
fn osc_is_quiet(command: &[u8]) -> bool {
    let Ok(n) = std::str::from_utf8(command).unwrap_or("").parse::<u32>() else {
        return false;
    };
    matches!(n, 4..=7 | 9..=19 | 22 | 52 | 104..=119 | 777)
}

/// DCS strings known not to change the pen: sixel images, DECRQSS and
/// XTGETTCAP queries, XTSETTCAP, user-defined keys, soft fonts and
/// synchronized-update markers. tmux passthrough is not among them: a
/// terminal that isn't tmux runs the escapes inside it.
fn dcs_is_harmless(raw: &[u8]) -> bool {
    let body = &raw[2..];
    let params = body
        .iter()
        .take_while(|b| (0x30..=0x3f).contains(*b))
        .count();
    let inter = body[params..]
        .iter()
        .take_while(|b| (0x20..=0x2f).contains(*b))
        .count();
    let (intermediates, fin) = (&body[params..params + inter], body.get(params + inter));
    matches!(
        (intermediates, fin),
        (b"" | b"$" | b"+", Some(b'q')) | (b"+", Some(b'p')) | (b"", Some(b'|' | b'{' | b's'))
    )
}

/// Writes `text`, replacing runs of a repeated character with REP when that
/// is shorter.
fn write_with_rep(text: &str, out: &mut Vec<u8>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let end = chars[i..]
            .iter()
            .position(|&d| d != c)
            .map_or(chars.len(), |p| i + p);
        let mut run = end - i;
        // A combining character after the run must follow a literal copy.
        if end < chars.len() && !cannot_combine(chars[end]) {
            run -= 1;
        }
        let mut buf = [0; 4];
        let encoded = c.encode_utf8(&mut buf).as_bytes();
        if rep_safe(c) && run > 1 {
            let mut repeats = run - 1;
            out.extend_from_slice(encoded);
            while repeats > 0 {
                let n = repeats.min(MAX_MOVE as usize);
                let mut rep = Vec::new();
                csi_num(&mut rep, n as u32, 1, b'b');
                if rep.len() < n * encoded.len() {
                    out.extend(rep);
                } else {
                    for _ in 0..n {
                        out.extend_from_slice(encoded);
                    }
                }
                repeats -= n;
            }
            for _ in run..end - i {
                out.extend_from_slice(encoded);
            }
        } else {
            for _ in i..end {
                out.extend_from_slice(encoded);
            }
        }
        i = end;
    }
}

/// Characters REP can repeat safely: single-width and never combining.
fn rep_safe(c: char) -> bool {
    matches!(c, ' '..='~' | '\u{2500}'..='\u{259f}' | '\u{2800}'..='\u{28ff}')
}

/// A conservative test for characters that are certainly not combining
/// marks, zero-width joiners, variation selectors or emoji modifiers:
/// characters from blocks that contain none of those.
fn cannot_combine(c: char) -> bool {
    matches!(c,
        ' '..='~'
        | '\u{a0}'..='\u{2ff}'       // Latin-1, Latin extended, IPA, spacing modifiers
        | '\u{370}'..='\u{482}'      // Greek, Cyrillic letters
        | '\u{1e00}'..='\u{1fff}'    // Latin and Greek precomposed letters
        | '\u{2010}'..='\u{2027}'    // dashes, quotes, bullets
        | '\u{2030}'..='\u{205e}'    // more punctuation
        | '\u{20a0}'..='\u{20cf}'    // currency
        | '\u{2100}'..='\u{2bff}'    // symbols, arrows, math, box drawing, blocks, shapes
        | '\u{3000}'..='\u{3029}'    // CJK punctuation
        | '\u{3041}'..='\u{3096}'    // hiragana
        | '\u{30a1}'..='\u{30fa}'    // katakana
        | '\u{4e00}'..='\u{9fff}'    // CJK ideographs
        | '\u{ac00}'..='\u{d7a3}'    // Hangul syllables
        | '\u{e000}'..='\u{f8ff}'    // private use (Nerd Font and Powerline icons)
        | '\u{ff01}'..='\u{ff9d}'    // fullwidth and halfwidth forms
        | '\u{1f300}'..='\u{1f3fa}'  // emoji (stopping before skin tones)
        | '\u{1f400}'..='\u{1faff}'
        | '\u{f0000}'..='\u{10fffd}') // supplementary private use (Nerd Font icons)
}

/// True for tokens that change attributes for later text and nothing else:
/// SGR, hyperlinks and character set selection.
fn only_sets_attributes(token: &Token) -> bool {
    match token {
        Token::Csi(c) => c.private.is_none() && c.intermediates.is_empty() && c.final_byte == b'm',
        Token::Osc(o) => o.payload.starts_with(b"8;"),
        Token::Control(b) => matches!(b, 0x0e | 0x0f),
        Token::Esc(e) => e
            .intermediates
            .first()
            .is_some_and(|i| b"()*+-./".contains(i)),
        _ => false,
    }
}
