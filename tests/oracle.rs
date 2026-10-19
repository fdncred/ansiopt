//! Differential tests against the vt100 crate. The `ghostty-oracle` crate
//! runs the same suite against libghostty-vt as well.

#[macro_use]
mod harness;

suite!(&[&harness::Vt100]);
