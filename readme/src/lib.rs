//! Not a library: `cargo test -p docuconf-readme --doc` compiles every Rust
//! block in the repository's README.md, and runs the test blocks, against
//! a crate that depends on nothing but `docuconf` and `serde`.
// README links are relative to the repository, not rustdoc items.
#![allow(rustdoc::broken_intra_doc_links)]
#![doc = include_str!("../../README.md")]
