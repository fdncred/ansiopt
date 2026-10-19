//! Differential test harness shared by `tests/oracle.rs` (vt100) and the
//! `ghostty-oracle` crate (vt100 plus libghostty-vt).
//!
//! Each case renders `prefix + input + suffix` and `prefix + output +
//! suffix` in every oracle and compares what they observe. Random prefixes
//! leave the terminal in an arbitrary state, which checks that the optimizer
//! never assumes anything about the starting state; random suffixes check
//! that the stream ends in the same state. Failures shrink to a minimal case.

use std::num::NonZeroU16;

use ansiopt::{Minifier, Options, lexer};

/// A terminal emulator used as ground truth.
pub trait Oracle {
    fn name(&self) -> &'static str;

    /// Everything observable after rendering `bytes`. With `visual`, blank
    /// cells are described only by what can be seen on them.
    fn snapshot(&self, bytes: &[u8], size: Size, visual: bool) -> Vec<String>;
}

/// Generates the test functions for a set of oracles.
macro_rules! suite {
    ($oracles:expr) => {
        #[test]
        fn fuzz_default() {
            harness::fuzz("default", ansiopt::Options::default(), 1, $oracles);
        }

        #[test]
        fn fuzz_compat() {
            let options = ansiopt::Options {
                compat: true,
                max_params: 3,
                ..Default::default()
            };
            harness::fuzz("compat", options, 2, $oracles);
        }

        #[test]
        fn fuzz_rep() {
            let options = ansiopt::Options {
                rep: true,
                ..Default::default()
            };
            harness::fuzz("rep", options, 3, $oracles);
        }

        #[test]
        fn fuzz_visual() {
            let options = ansiopt::Options {
                visual: true,
                rep: true,
                ..Default::default()
            };
            harness::fuzz("visual", options, 4, $oracles);
        }

        #[test]
        fn fuzz_assume_reset() {
            let options = ansiopt::Options {
                assume_reset: true,
                ..Default::default()
            };
            harness::fuzz("assume_reset", options, 5, $oracles);
        }

        #[test]
        fn fuzz_streaming() {
            harness::fuzz_streaming(ansiopt::Options::default(), 6, $oracles);
        }

        #[test]
        fn fuzz_streaming_visual() {
            let options = ansiopt::Options {
                visual: true,
                rep: true,
                ..Default::default()
            };
            harness::fuzz_streaming(options, 8, $oracles);
        }

        #[test]
        fn second_pass_never_grows() {
            harness::second_pass_never_grows();
        }

        /// Checks real captured output: `tests/fixtures`, or the directory
        /// in `ANSIOPT_CORPUS`.
        #[test]
        fn corpus() {
            harness::corpus($oracles);
        }
    };
}

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const CHARS: &[&str] = &["a", "b", "Z", " ", " ", "─", "é", "日", "q", "x"];

fn gen_number(rng: &mut Rng, out: &mut String) {
    out.push_str(rng.pick(&["", "0", "1", "1", "2", "3", "5", "03", "12", "40"]));
}

fn gen_sgr_code(rng: &mut Rng) -> String {
    match rng.below(12) {
        0 => rng.pick(&["", "0", "00"]).to_string(),
        1..=3 => rng
            .pick(&[
                "1", "2", "3", "4", "5", "7", "8", "9", "21", "22", "23", "24", "25", "27", "28",
                "29", "53", "55",
            ])
            .to_string(),
        4 | 5 => format!("{}", rng.pick(&[30, 40, 90, 100]) + rng.below(8)),
        6 => rng.pick(&["39", "49", "59"]).to_string(),
        7 => format!("{};5;{}", rng.pick(&[38, 48, 58]), rng.below(20)),
        8 => format!(
            "{};2;{};{};0{}",
            rng.pick(&[38, 48]),
            rng.below(3),
            rng.below(256),
            rng.below(3)
        ),
        9 => rng
            .pick(&["4:3", "4:0", "4:1", "38:2::1:2:3", "48:5:3", "58:2::9:9:9"])
            .to_string(),
        10 => rng
            .pick(&["01", "031", "10", "11", "73", "51", "54"])
            .to_string(),
        _ => rng.pick(&["20", "60", "38;7", "38;5"]).to_string(),
    }
}

fn gen_token(rng: &mut Rng, out: &mut String) {
    match rng.below(100) {
        0..=24 => {
            // A combining mark at the start attaches to whatever was printed
            // before the preceding escape sequences.
            if rng.chance(3) {
                out.push('\u{301}');
            }
            if rng.chance(25) {
                let c = rng.pick(CHARS);
                for _ in 0..2 + rng.below(14) {
                    out.push_str(c);
                }
            } else {
                for _ in 0..1 + rng.below(5) {
                    out.push_str(rng.pick(CHARS));
                }
            }
            if rng.chance(3) {
                out.push('\u{301}');
            }
        }
        25..=49 => {
            out.push_str("\x1b[");
            let n = 1 + rng.below(4);
            for i in 0..n {
                if i > 0 {
                    out.push(';');
                }
                out.push_str(&gen_sgr_code(rng));
            }
            out.push('m');
        }
        50..=64 => match rng.below(10) {
            0 => out.push('\r'),
            1 => out.push('\x08'),
            2 => out.push('\t'),
            3 => out.push('\n'),
            4 => {
                out.push_str("\x1b[");
                gen_number(rng, out);
                if rng.chance(70) {
                    out.push(';');
                    gen_number(rng, out);
                }
                out.push(*rng.pick(&['H', 'f']));
            }
            _ => {
                out.push_str("\x1b[");
                gen_number(rng, out);
                out.push(*rng.pick(&[
                    'A', 'B', 'C', 'D', 'G', 'd', '`', 'E', 'F', 'a', 'e', 'I', 'Z',
                ]));
            }
        },
        65..=72 => {
            out.push_str("\x1b[");
            if rng.chance(10) {
                out.push('?');
            }
            gen_number(rng, out);
            out.push(*rng.pick(&['J', 'K', 'X', '@', 'P', 'L', 'M', 'S', 'T', 'b']));
        }
        73..=76 => {
            out.push_str(rng.pick(&["\x1b7", "\x1b8", "\x1b[s", "\x1b[u", "\x1b7", "\x1b8"]))
        }
        77..=82 => {
            let modes = [
                "25", "7", "1", "2004", "1049", "47", "1000", "1006", "6", "12", "25;2004", "1;7",
            ];
            out.push_str(&format!(
                "\x1b[?{}{}",
                rng.pick(&modes),
                rng.pick(&["h", "l"])
            ));
        }
        83..=87 => {
            let st = *rng.pick(&["\x07", "\x1b\\"]);
            match rng.below(5) {
                0 | 1 => out.push_str(&format!(
                    "\x1b]{};{}{st}",
                    rng.pick(&[0, 1, 2]),
                    rng.pick(&["a", "b", ""])
                )),
                2 | 3 => out.push_str(&format!(
                    "\x1b]8;{};{}{st}",
                    rng.pick(&["", "id=1"]),
                    rng.pick(&["", "", "http://a", "http://b"])
                )),
                _ => out.push_str(&format!(
                    "\x1b]{}{st}",
                    rng.pick(&["7;file:///", "4;1;?", "4;1;rgb:0/0/0", "133;A"])
                )),
            }
        }
        88..=93 => out.push_str(rng.pick(&[
            "\x1bM",
            "\x1bD",
            "\x1bE",
            "\x1b(0",
            "\x1b(B",
            "\x1b)0",
            "\x0e",
            "\x0f",
            "\x1b=",
            "\x1b>",
            "\x1b[2;4r",
            "\x1b[r",
            "\x07",
            "\x1b[!p",
        ])),
        94 | 95 => out.push_str(rng.pick(&[
            "\x1bc",
            "\x1b[5x",
            "\x1bP$qm\x1b\\",
            "\x1b_Ga=q\x1b\\",
            "\x1b[3\x18",
            "\x1b]0;z\x1b[m",
        ])),
        _ => out.push_str(rng.pick(&[
            "\x1b[?25l",
            "\x1b[?25h",
            "\x1b[?2004h",
            "\x1b[?7l",
            "\x1b[?7h",
        ])),
    }
}

pub fn gen_stream(rng: &mut Rng, max_tokens: usize) -> Vec<u8> {
    let mut s = String::new();
    for _ in 0..rng.below(max_tokens + 1) {
        gen_token(rng, &mut s);
    }
    s.into_bytes()
}

#[derive(Clone, Copy)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

/// The vt100 crate. It lets bold and faint replace each other, ignores VPA's
/// origin mode, copies the whole pen into erased cells and doesn't model
/// blink, strikethrough, hyperlinks or character sets.
pub struct Vt100;

#[derive(Default)]
struct Titles {
    title: Vec<u8>,
    icon: Vec<u8>,
}

impl vt100::Callbacks for Titles {
    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        self.icon = name.to_vec();
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = title.to_vec();
    }
}

impl Oracle for Vt100 {
    fn name(&self) -> &'static str {
        "vt100"
    }

    fn snapshot(&self, bytes: &[u8], size: Size, visual: bool) -> Vec<String> {
        let mut parser = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(size.rows).unwrap(),
            NonZeroU16::new(size.cols).unwrap(),
            0,
            Titles::default(),
        );
        parser.process(bytes);
        let screen = parser.screen();
        let mut lines = vec![
            format!("cursor {:?}", screen.cursor_position()),
            format!("pen {:?}", screen.attributes_formatted()),
            format!(
                "modes alt={} hide={} appcur={} appkp={} paste={} mouse={:?}/{:?}",
                screen.alternate_screen(),
                screen.hide_cursor(),
                screen.application_cursor(),
                screen.application_keypad(),
                screen.bracketed_paste(),
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding()
            ),
            format!(
                "title {:?} icon {:?}",
                String::from_utf8_lossy(&parser.callbacks().title),
                String::from_utf8_lossy(&parser.callbacks().icon)
            ),
        ];
        for row in 0..size.rows {
            lines.push(format!("wrapped {row} {}", screen.row_wrapped(row)));
            for col in 0..size.cols {
                let cell = screen.cell(row, col).unwrap();
                let text = cell.contents();
                let blank = text.is_empty() || text == " ";
                let desc = if visual && blank {
                    let bg = if cell.inverse() {
                        cell.fgcolor()
                    } else {
                        cell.bgcolor()
                    };
                    format!("{text:?} bg={bg:?} ul={}", cell.underline())
                } else {
                    format!(
                        "{text:?} fg={:?} bg={:?} b={} d={} i={} u={} r={} w={}",
                        cell.fgcolor(),
                        cell.bgcolor(),
                        cell.bold(),
                        cell.dim(),
                        cell.italic(),
                        cell.underline(),
                        cell.inverse(),
                        cell.is_wide()
                    )
                };
                lines.push(format!("{row},{col} {desc}"));
            }
        }
        lines
    }
}

pub fn escape(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).escape_debug().to_string()
}

pub struct Case {
    pub prefix: Vec<u8>,
    pub input: Vec<u8>,
    pub suffix: Vec<u8>,
}

/// Returns the first difference between rendering the input and its
/// minified form, if any.
pub fn check(
    case: &Case,
    options: &Options,
    size: Size,
    oracles: &[&dyn Oracle],
) -> Option<String> {
    let output = ansiopt::minify(&case.input, options);
    let original = [&case.prefix[..], &case.input, &case.suffix].concat();
    let minified = [&case.prefix[..], &output, &case.suffix].concat();
    for oracle in oracles {
        let expected = oracle.snapshot(&original, size, options.visual);
        let actual = oracle.snapshot(&minified, size, options.visual);
        if let Some((e, a)) = expected.iter().zip(&actual).find(|(e, a)| e != a) {
            return Some(format!(
                "output:   {}\noracle:   {}\nexpected: {e}\nactual:   {a}",
                escape(&output),
                oracle.name()
            ));
        }
    }
    None
}

/// Removes tokens from a failing case while it keeps failing.
fn shrink(mut case: Case, options: &Options, size: Size, oracles: &[&dyn Oracle]) -> Case {
    loop {
        let mut progress = false;
        for part in 0..3 {
            let mut i = 0;
            loop {
                let bytes = match part {
                    0 => &case.prefix,
                    1 => &case.input,
                    _ => &case.suffix,
                };
                let mut pieces = lexer::spans(bytes);
                if i >= pieces.len() {
                    break;
                }
                pieces.remove(i);
                let candidate = pieces.concat();
                let mut trial = Case {
                    prefix: case.prefix.clone(),
                    input: case.input.clone(),
                    suffix: case.suffix.clone(),
                };
                *match part {
                    0 => &mut trial.prefix,
                    1 => &mut trial.input,
                    _ => &mut trial.suffix,
                } = candidate;
                if check(&trial, options, size, oracles).is_some() {
                    case = trial;
                    progress = true;
                } else {
                    i += 1;
                }
            }
        }
        if !progress {
            return case;
        }
    }
}

/// Cases per test; set `ANSIOPT_FUZZ_ITERATIONS` for a longer soak.
pub fn iterations() -> usize {
    std::env::var("ANSIOPT_FUZZ_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

/// Mixes `ANSIOPT_FUZZ_SEED` into a test's base seed to explore other
/// streams.
pub fn seed(base: u64) -> u64 {
    let extra: u64 = std::env::var("ANSIOPT_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    base.wrapping_add(extra.wrapping_mul(1_000_003))
}

pub fn fuzz(name: &str, options: Options, base_seed: u64, oracles: &[&dyn Oracle]) {
    let mut rng = Rng(seed(base_seed));
    for iteration in 0..iterations() {
        let size = *rng.pick(&[
            Size { rows: 4, cols: 8 },
            Size { rows: 6, cols: 13 },
            Size { rows: 24, cols: 80 },
        ]);
        let prefix = if options.assume_reset {
            Vec::new()
        } else {
            gen_stream(&mut rng, 10)
        };
        let input = gen_stream(&mut rng, 30);
        let mut suffix = gen_stream(&mut rng, 6);
        if options.visual {
            // Visual mode lets hidden attributes of blank cells differ; a
            // combining mark printed onto one later would show them. That is
            // the mode's documented trade-off, not something to test for.
            suffix = String::from_utf8(suffix)
                .unwrap()
                .replace('\u{301}', "")
                .into_bytes();
        }
        suffix.extend_from_slice(b"Q \x1b[K");
        let case = Case {
            prefix,
            input,
            suffix,
        };
        if check(&case, &options, size, oracles).is_some() {
            let case = shrink(case, &options, size, oracles);
            let diff = check(&case, &options, size, oracles).unwrap();
            panic!(
                "{name}: mismatch at iteration {iteration} ({}x{})\nprefix:   {}\ninput:    {}\nsuffix:   {}\n{diff}",
                size.rows,
                size.cols,
                escape(&case.prefix),
                escape(&case.input),
                escape(&case.suffix)
            );
        }
    }
}

/// Feeding the stream in arbitrary chunks must give equivalent output,
/// whether or not the caller flushes between chunks.
pub fn fuzz_streaming(options: Options, base_seed: u64, oracles: &[&dyn Oracle]) {
    let mut rng = Rng(seed(base_seed));
    let size = Size { rows: 6, cols: 13 };
    for iteration in 0..iterations() / 2 {
        let prefix = gen_stream(&mut rng, 10);
        let input = gen_stream(&mut rng, 30);
        // Visual mode only notices a combining mark from the chunk it
        // arrives in on (a documented limitation), so leave those out here.
        if options.visual && input.windows(2).any(|w| w == "\u{301}".as_bytes()) {
            continue;
        }
        let flush = rng.chance(50);
        let mut minifier = Minifier::new(options);
        let mut output = Vec::new();
        let mut rest = &input[..];
        while !rest.is_empty() {
            let (chunk, tail) = rest.split_at(1 + rng.below(rest.len()));
            output.extend(minifier.feed(chunk));
            if flush {
                output.extend(minifier.flush());
            }
            rest = tail;
        }
        output.extend(minifier.finish());
        for oracle in oracles {
            let expected =
                oracle.snapshot(&[&prefix[..], &input, b"Q"].concat(), size, options.visual);
            let actual =
                oracle.snapshot(&[&prefix[..], &output, b"Q"].concat(), size, options.visual);
            assert_eq!(
                expected,
                actual,
                "{} iteration {iteration}\nprefix: {}\ninput:  {}\noutput: {}",
                oracle.name(),
                escape(&prefix),
                escape(&input),
                escape(&output)
            );
        }
    }
}

/// A second pass may find a little more to remove (for example writes made
/// dead by a reset it can now see), but must never make things worse.
pub fn second_pass_never_grows() {
    let mut rng = Rng(seed(7));
    for _ in 0..iterations() {
        let input = gen_stream(&mut rng, 30);
        let once = ansiopt::minify(&input, &Options::default());
        let twice = ansiopt::minify(&once, &Options::default());
        assert!(
            twice.len() <= once.len(),
            "input: {}\nonce:  {}\ntwice: {}",
            escape(&input),
            escape(&once),
            escape(&twice)
        );
    }
}

/// The captures in `tests/fixtures`, found from either crate of the
/// workspace.
pub fn fixtures() -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let dir = match std::env::var("ANSIOPT_CORPUS") {
        Ok(dir) => std::path::PathBuf::from(dir),
        Err(_) => {
            let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            [
                manifest.join("tests/fixtures"),
                manifest.join("../tests/fixtures"),
            ]
            .into_iter()
            .find(|d| d.is_dir())
            .expect("tests/fixtures exists")
        }
    };
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

/// Checks every capture in [`fixtures`] under several option sets.
pub fn corpus(oracles: &[&dyn Oracle]) {
    let configs = [
        ("default", Options::default()),
        (
            "assume_reset",
            Options {
                assume_reset: true,
                ..Options::default()
            },
        ),
        (
            "aggressive",
            Options {
                visual: true,
                rep: true,
                ..Options::default()
            },
        ),
        (
            "compat",
            Options {
                compat: true,
                ..Options::default()
            },
        ),
    ];
    for (path, input) in fixtures() {
        for (name, options) in &configs {
            for size in [
                Size { rows: 24, cols: 80 },
                Size {
                    rows: 60,
                    cols: 200,
                },
            ] {
                let case = Case {
                    prefix: Vec::new(),
                    input: input.clone(),
                    suffix: b"Q \x1b[K".to_vec(),
                };
                if let Some(diff) = check(&case, options, size, oracles) {
                    let diff: Vec<_> = diff.lines().filter(|l| !l.starts_with("output:")).collect();
                    panic!(
                        "{} with {name} at {}x{}:\n{}",
                        path.display(),
                        size.rows,
                        size.cols,
                        diff.join("\n")
                    );
                }
            }
        }
        eprintln!("ok {}", path.display());
    }
}
