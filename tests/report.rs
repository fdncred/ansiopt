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
