# Changelog

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
