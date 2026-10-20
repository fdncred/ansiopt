//! CPU benchmarks for the minifier: each captured fixture on its own, a
//! large corpus under every option set, the lexer alone, and the streaming
//! path the CLI's `--stream` mode takes.

use std::hint::black_box;
use std::time::Duration;

use ansiopt::{Minifier, Options, lexer};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const FIXTURES: [(&str, &[u8]); 5] = [
    ("bat", include_bytes!("../tests/fixtures/bat.ansi")),
    (
        "nu_table",
        include_bytes!("../tests/fixtures/nu_table.ansi"),
    ),
    ("nvim", include_bytes!("../tests/fixtures/nvim.ansi")),
    ("rg", include_bytes!("../tests/fixtures/rg.ansi")),
    ("vim", include_bytes!("../tests/fixtures/vim.ansi")),
];

/// How many times the concatenated fixtures repeat in the corpus (~1 MB).
const CORPUS_REPEATS: usize = 50;

/// Chunk size `ansiopt --stream` reads with.
const STREAM_CHUNK: usize = 64 * 1024;

fn corpus() -> Vec<u8> {
    let one: Vec<u8> = FIXTURES
        .iter()
        .flat_map(|(_, f)| f.iter().copied())
        .collect();
    one.repeat(CORPUS_REPEATS)
}

/// The option sets the report tests exercise.
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
            "visual_rep",
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

fn bench_fixtures(c: &mut Criterion) {
    let mut group = c.benchmark_group("fixture");
    let options = Options::default();
    for (name, input) in FIXTURES {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), input, |b, input| {
            b.iter(|| ansiopt::minify(black_box(input), &options));
        });
    }
    group.finish();
}

fn bench_corpus(c: &mut Criterion) {
    let input = corpus();
    let mut group = c.benchmark_group("corpus");
    group.throughput(Throughput::Bytes(input.len() as u64));
    for (name, options) in option_sets() {
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |b, input| {
            b.iter(|| ansiopt::minify(black_box(input), &options));
        });
    }
    group.finish();
}

fn bench_lexer(c: &mut Criterion) {
    let input = corpus();
    let mut group = c.benchmark_group("lexer");
    group.throughput(Throughput::Bytes(input.len() as u64));
    group.bench_function("tokenize", |b| {
        b.iter(|| lexer::tokenize(black_box(&input)).len());
    });
    group.finish();
}

fn bench_stream(c: &mut Criterion) {
    let input = corpus();
    let mut group = c.benchmark_group("stream");
    group.throughput(Throughput::Bytes(input.len() as u64));
    group.bench_function("feed_flush_64k", |b| {
        b.iter(|| {
            let mut minifier = Minifier::new(Options::default());
            let mut written = 0;
            for chunk in black_box(&input).chunks(STREAM_CHUNK) {
                written += minifier.feed(chunk).len();
                written += minifier.flush().len();
            }
            written + minifier.finish().len()
        });
    });
    group.finish();
}

fn config() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .sample_size(50)
}

criterion_group! {
    name = benches;
    config = config();
    targets = bench_fixtures, bench_corpus, bench_lexer, bench_stream
}
criterion_main!(benches);
