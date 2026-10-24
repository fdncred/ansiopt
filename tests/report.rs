//! Properties of the reporting engine on random streams: the alignment
//! always works and covers both streams exactly.

#[allow(dead_code, unused_macros)]
mod harness;

use ansiopt::Options;
use ansiopt::align::{Piece, align};
use ansiopt::report::{Report, html, term};
use ansiopt::trace::Fate;
use harness::{Rng, escape, fixtures, gen_stream, iterations, seed};

fn option_sets() -> [(&'static str, Options); 5] {
    [
        ("default", Options::default()),
        (
            "compat",
            Options {
                compat: true,
                max_params: 3,
                ..Options::default()
            },
        ),
        (
            "rep",
            Options {
                rep: true,
                ..Options::default()
            },
        ),
        (
            "visual",
            Options {
                visual: true,
                rep: true,
                ..Options::default()
            },
        ),
        (
            "assume_reset",
            Options {
                assume_reset: true,
                ..Options::default()
            },
        ),
    ]
}

/// The pieces of an alignment, read in order, reproduce both streams.
#[test]
fn alignment_partitions_both_streams() {
    let mut rng = Rng(seed(31));
    for _ in 0..iterations() / 4 {
        let input = gen_stream(&mut rng, 40);
        for (name, options) in option_sets() {
            let output = ansiopt::minify(&input, &options);
            let alignment = align(&input, &output).unwrap_or_else(|e| {
                panic!(
                    "{name}: {e}\ninput:  {}\noutput: {}",
                    escape(&input),
                    escape(&output)
                )
            });
            let (mut a, mut b) = (Vec::new(), Vec::new());
            for piece in &alignment.pieces {
                match piece {
                    Piece::Text {
                        input: i,
                        output: o,
                        ..
                    } => {
                        a.extend_from_slice(&input[i.clone()]);
                        b.extend_from_slice(&output[o.clone()]);
                    }
                    Piece::Gap(gap) => {
                        for r in &gap.input {
                            a.extend_from_slice(&input[r.clone()]);
                        }
                        for r in &gap.output {
                            b.extend_from_slice(&output[r.clone()]);
                        }
                    }
                }
            }
            assert_eq!(escape(&a), escape(&input), "{name}: input not covered");
            assert_eq!(escape(&b), escape(&output), "{name}: output not covered");
        }
    }
}

/// Recording a trace never changes the output, every token gets exactly one
/// settled fate, and the output a fate points at exists.
#[test]
fn trace_covers_every_token() {
    let mut rng = Rng(seed(32));
    for _ in 0..iterations() / 4 {
        let input = gen_stream(&mut rng, 40);
        for (name, options) in option_sets() {
            let plain = ansiopt::minify(&input, &options);
            let traced = ansiopt::minify_detailed(&input, &options, true);
            let context = || {
                format!(
                    "{name}\ninput:  {}\noutput: {}",
                    escape(&input),
                    escape(&plain)
                )
            };
            assert_eq!(traced.output, plain, "{}", context());
            let trace = traced.trace.expect("trace requested");
            let mut pos = 0;
            for entry in &trace.entries {
                assert_eq!(entry.input.start, pos, "{}", context());
                pos = entry.input.end;
                let ranges: Vec<_> = match &entry.fate {
                    Fate::Pending => {
                        panic!("pending entry at {}: {}", entry.input.start, context())
                    }
                    Fate::Rewritten { output, .. } | Fate::Merged { output } => {
                        vec![output.clone()]
                    }
                    Fate::Folded { outputs } => outputs.clone(),
                    _ => Vec::new(),
                };
                for r in ranges {
                    assert!(r.end <= plain.len() && !r.is_empty(), "{}", context());
                }
                if entry.fate == Fate::Kept {
                    let bytes = &input[entry.input.clone()];
                    assert!(
                        plain.windows(bytes.len()).any(|w| w == bytes),
                        "kept {} missing from output: {}",
                        escape(bytes),
                        context()
                    );
                }
            }
            assert_eq!(pos, input.len(), "{}", context());
        }
    }
}

/// A color or style change is only folded into the output when something
/// written needed it; otherwise its fate says why it was left out.
#[test]
fn color_changes_say_why_they_were_left_out() {
    let outcomes = |input: &str, options: &Options| -> Vec<String> {
        let trace = ansiopt::minify_detailed(input.as_bytes(), options, true)
            .trace
            .expect("trace requested");
        trace
            .entries
            .iter()
            .filter(|e| e.fate != Fate::Text)
            .map(|e| match &e.fate {
                Fate::NoEffect { why } => format!("no effect: {why}"),
                Fate::Superseded { why } => format!("dropped: {why}"),
                fate => fate.kind().to_string(),
            })
            .collect()
    };
    let default = Options::default();
    let already = "no effect: already in effect";
    let again = "dropped: set again before it mattered";
    let undone = "no effect: changed nothing by the time it mattered";
    let cases: [(&str, &Options, &[&str]); 9] = [
        // The second reset finds everything reset already.
        (
            "\x1b[0m\x1b[0m\x1b[31mhi\x1b[0m",
            &default,
            &["folded", already, "folded", "folded"],
        ),
        (
            "\x1b[0m\x1b[0m\x1b[31mhi\x1b[0m",
            &Options {
                assume_reset: true,
                ..Options::default()
            },
            &[already, already, "folded", "folded"],
        ),
        // The reset already made the background the default, and the second
        // reset only cleared a foreground that the next sequence sets again.
        (
            "\x1b[0m\x1b[38;5;8m\x1b[49m 1 \x1b[0m\x1b[48;5;0;38;5;7m 25 ",
            &default,
            &["folded", "folded", already, again, "folded"],
        ),
        ("\x1b[31m\x1b[32mx", &default, &[again, "folded"]),
        // The repeat finds red in effect, so the first one keeps the credit.
        ("\x1b[31m\x1b[31mx", &default, &["folded", already]),
        // The reset turns off reverse and italic again, and is needed anyway
        // for everything else.
        (
            "\x1b[27m\x1b[23m\x1b[mx",
            &default,
            &[again, again, "folded"],
        ),
        // Faint adds to bold instead of replacing it.
        ("\x1b[1m\x1b[2mx", &default, &["folded", "folded"]),
        // Green is set back to the red the terminal already has.
        (
            "\x1b[31mx\x1b[32m\x1b[31m\x1b[1my",
            &default,
            &["folded", again, undone, "folded"],
        ),
        // The red background is written for the space before green replaces
        // it.
        (
            "\x1b[0mA\x1b[41m \x1b[42mx",
            &Options {
                visual: true,
                ..Options::default()
            },
            &["folded", "folded", "folded"],
        ),
    ];
    for (input, options, expected) in cases {
        assert_eq!(
            outcomes(input, options),
            expected,
            "{}",
            escape(input.as_bytes())
        );
    }
}

/// A color or style sequence that the trace calls already in effect, or set
/// again before it mattered, can be deleted from the input without
/// changing the output. The outputs compared are the optimizer's own, since
/// a shorter input may make `minify` fall back to it.
#[test]
fn colors_left_out_can_be_deleted() {
    let optimize = |input: &[u8], options: Options| {
        let mut minifier = ansiopt::Minifier::new(options);
        let mut output = minifier.feed(input);
        output.extend(minifier.finish());
        output
    };
    let is_sgr = |raw: &[u8]| {
        let params = raw
            .strip_prefix(b"\x1b[")
            .or_else(|| raw.strip_prefix("\u{9b}".as_bytes()))
            .and_then(|rest| rest.strip_suffix(b"m"));
        params.is_some_and(|p| p.iter().all(|b| b"0123456789;:".contains(b)))
    };
    let params = |raw: &[u8]| raw.iter().filter(|&&b| b == b';').count() + 1;
    let mut rng = Rng(seed(33));
    for _ in 0..iterations() / 4 {
        let input = gen_stream(&mut rng, 40);
        for (name, options) in option_sets() {
            let mut minifier = ansiopt::Minifier::with_trace(options);
            let mut output = minifier.feed(&input);
            let (rest, trace) = minifier.finish_traced();
            output.extend(rest);
            let trace = trace.expect("trace requested");
            // Written sequences have at most as many parameters as the option
            // allows or the longest SGR so far has, so deleting one longer
            // than that may change how later ones are written.
            let mut cap = options.max_params.max(1);
            for entry in &trace.entries {
                let raw = &input[entry.input.clone()];
                if !is_sgr(raw) {
                    continue;
                }
                let longer = params(raw) > cap;
                cap = cap.max(params(raw));
                let left_out = matches!(
                    entry.fate,
                    Fate::NoEffect {
                        why: "already in effect"
                    } | Fate::Superseded {
                        why: "set again before it mattered"
                    }
                );
                if !left_out || longer {
                    continue;
                }
                let mut without = input.clone();
                without.drain(entry.input.clone());
                assert_eq!(
                    escape(&optimize(&without, options)),
                    escape(&output),
                    "{name}: deleting {} at {} changed the output\ninput: {}",
                    escape(raw),
                    entry.input.start,
                    escape(&input)
                );
            }
        }
    }
}

/// Every view works on real captures, and the HTML preview confirms the
/// output draws the same screen.
#[test]
fn views_on_fixtures() {
    for (path, input) in fixtures() {
        for (name, options) in option_sets() {
            let minified = ansiopt::minify_detailed(&input, &options, true);
            let report = Report::new(&input, &minified.output, minified.fell_back, minified.trace)
                .unwrap_or_else(|e| panic!("{} with {name}: {e}", path.display()));
            let paint = term::Paint { color: true };
            for view in [
                term::diff(&report, paint, false),
                term::explain(&report, paint, true),
                term::hotspot(&report, paint),
            ] {
                assert!(!view.is_empty());
            }
            let settings = html::Settings {
                title: path.display().to_string(),
                options,
                width: None,
                height: None,
            };
            let page = html::html(&report, &settings);
            assert!(page.contains("</html>"));
            #[cfg(feature = "render")]
            assert!(
                page.contains("✓ Identical"),
                "{} with {name}: the preview differs",
                path.display()
            );
        }
    }
}
