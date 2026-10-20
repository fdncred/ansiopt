# Changelog

## 0.3.0 (unreleased)

Minifying is about twice as fast as 0.2.0, between 1.75 and 3 times depending
on the capture and options. For the same input and options, the output is byte for
byte what 0.2.0 writes. The public API is unchanged.

- The lexer checks text 16 bytes at a time, skips UTF-8 and C1 checks for
  ASCII text, and jumps to the end of OSC, DCS and APC strings.
  `lexer::tokenize` sizes its vector from a count of ESC bytes.
- `Pen::apply` splits and parses parameters in one pass.
- The SGR encoder writes `ESC[m` for a change to the default pen without
  comparing candidates, and skips the reset candidate when it can't be
  shorter.
- `Minifier::feed` scans its input in place, and text goes straight to the
  output unless REP is on. Cursor moves, mode changes, hyperlinks and
  character sets no longer allocate per sequence.
- Adds `memchr` and `simdutf8` as dependencies.
- Adds criterion benchmarks in `benches/`.

## 0.2.0 (unreleased)

Minifying is about 1.7 times faster on captures from nu, bat, rg, vim and
nvim. For the same input and options, the output is byte for byte what 0.1.0
writes. The public API is unchanged.

- The SGR encoder keeps each code in a fixed-size array on the stack instead
  of formatting a `String` for it, and writes out only the shortest candidate
  sequence.
- `Pen::apply` parses parameters without allocating.
- The lexer finds C1 controls with a byte search instead of decoding every
  character.
- The minifier reuses its text buffer and reserves output space up front.
- Outside visual mode, `Minifier::feed` processes each token as soon as it is
  scanned instead of collecting them all first.

## 0.1.0 (2026-10-19)

First release.
