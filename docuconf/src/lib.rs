//! # docuconf
//!
//! Typed configuration contracts for [figment]. Keep your
//! `#[derive(Deserialize)]` config struct; add `#[derive(Docuconf)]` and a
//! few attributes, and docuconf
//!
//! 1. exports the struct as a CUE contract (`contract.cue`) that the
//!    platform validates its values against before deploying, and
//! 2. at boot, loads the environment and files through figment, checks
//!    every declared input, and reports **all** violations together with
//!    the stable codes of the docuconf spec, never printing a secret.
//!
//! ```
//! use docuconf::{Docuconf, DocuconfEnum, Secret, TlsKeyPair, ConfigFile};
//! use serde::Deserialize;
//! use std::time::Duration;
//!
//! #[derive(Deserialize, Docuconf)]
//! struct Config {
//!     /// HTTP listen port.
//!     #[docuconf(default = 8080, min = 1)]
//!     port: u16,
//!
//!     /// Primary Postgres connection string.
//!     #[docuconf(schemes("postgres", "postgresql"))]
//!     database_url: Secret<String>,
//!
//!     /// Upstream request timeout.
//!     #[docuconf(default = "30s", max = "5m")]
//!     #[serde(with = "docuconf::humantime_serde")]
//!     request_timeout: Duration,
//!
//!     /// Minimum log level emitted.
//!     #[docuconf(default = "info")]
//!     log_level: LogLevel,
//!
//!     /// Certificate the service serves HTTPS with.
//!     #[docuconf(path = "/etc/app/tls", dns_names("api.example.com"), min_remaining = "720h")]
//!     serving_tls: TlsKeyPair,
//!
//!     /// Routing table.
//!     #[docuconf(path = "/etc/app/routes/routes.yaml")]
//!     routes: Option<ConfigFile<Routes>>,
//! }
//!
//! #[derive(Deserialize, DocuconfEnum)]
//! #[serde(rename_all = "lowercase")]
//! enum LogLevel { Debug, Info, Warn, Error }
//!
//! #[derive(Deserialize, docuconf::JsonSchema)]
//! #[schemars(crate = "docuconf::schemars")]
//! struct Routes { routes: Vec<String> }
//!
//! let cue = docuconf::export::<Config>(&docuconf::Meta::new("billing-api")).unwrap();
//! assert!(cue.contains("DATABASE_URL: {"));
//! ```
//!
//! At boot, `let config: Config = docuconf::load_or_exit();`.
//! [`Loader`] adds the app's own config files, profiles and a platform
//! config-file [`Overlay`]. A [`Watched<T>`] file input rereads its file
//! when it changes (`reload: watch`). [`Contract`] validates an environment against a
//! contract given as JSON, with no Rust declaration (contract-first mode).
//!
//! ## Types
//!
//! | Rust field type | Contract type |
//! |---|---|
//! | `String` | `string` (`url` with `schemes`/`url`, `enum` with `values`) |
//! | `i8`..`i64`, `u8`..`u64`, `isize`, `usize` | `int`, with the type's range as `min`/`max` |
//! | `f32`, `f64` | `float` |
//! | `bool` | `bool` |
//! | `std::time::Duration` with `#[serde(with = "docuconf::humantime_serde")]` | `duration`, encoding `go` |
//! | `url::Url` | `url` (`max_length`) |
//! | a `#[derive(DocuconfEnum)]` enum | `enum` |
//! | `Vec<String>`, `Vec<int>` | `list`, encoding `json` (or `csv` with `encoding = "csv"` and an optional `separator`), with an int item type's range as `itemMin`/`itemMax` (narrow it with `item_min`/`item_max`); string items take `item_min_length`/`item_max_length` |
//! | [`KeySet`] | `keySet`, always secret, encoding `csv` (or `json`), with `min_keys`, `max_keys`, `key_min_length` and `key_max_length` |
//! | [`Json<T>`] | `json`, schema from `T: JsonSchema` (`max_length` on its wire string) |
//! | [`Secret<T>`] | `T`, with `secret: true` |
//! | `Option<T>` | `T`, optional |
//! | [`ConfigFile<T>`], `TlsKeyPair`, `CaBundle`, `Keystore`, [`TextFile`], [`BinaryFile`] | file inputs |
//! | [`Watched<T>`] of a file input type (or of an `Option` of one) | the file input, `reload: watch` |
//! | a nested `#[derive(Docuconf)]` struct | its fields, named `PARENT__CHILD` |
//!
//! A field without `Option` and without a `default` is required.
//!
//! ## Descriptions and details
//!
//! The first paragraph of a field's `///` comment is its `description`,
//! and the rest its `details`: CommonMark for generated docs, never read at
//! runtime. Intra-doc links become code spans, and doctest attributes and
//! hidden `# ` lines are dropped from code blocks. `#[docuconf(description
//! = "...")]` and `#[docuconf(details = "...")]` override the comment.
//! Details must not be blank and have at most 4000 characters. The
//! `docuconf docs` command of the docuconf CLI renders them, with the rest
//! of the contract, as CONFIG.md and CONFIG.agents.md.
//!
//! ## Cargo features
//!
//! - `tls` (default): the `TlsKeyPair` and `CaBundle` file inputs, checked
//!   with rustls, webpki and x509-parser.
//! - `keystore` (default): the `Keystore` (PKCS#12) file input.
//! - `secrecy`: `secrecy::SecretString` and `secrecy::SecretBox<T>` fields
//!   declare secret variables.
//!
//! A service that reads only environment variables can use
//! `default-features = false` and skip the TLS stack.
//!
//! ## Names
//!
//! Variable names follow figment's `Env::prefixed(prefix).split("__")`:
//! the struct's `#[docuconf(prefix = "APP_")]` (empty by default), then the
//! serde key in upper case, with `__` between nested levels. So field
//! `cache.ttl` under prefix `APP_` reads `APP_CACHE__TTL`. Override one
//! name with `#[docuconf(env = "NAME")]`.
//!
//! ## Parsing
//!
//! docuconf reads the declared variables itself and hands figment typed
//! values, because figment's `Env` provider trims values and guesses types
//! (it reads `8080` as a number even for a `String` field), which the spec
//! forbids. Lists are JSON arrays (`["a","b"]`, which figment's `Env` also
//! parses) unless declared `encoding = "csv"` (`a,b`), durations use Go's syntax (`1m30s`, exactly what Go's
//! `time.ParseDuration` accepts), and an empty value is unset for every
//! type but `string`. Parsing is strict (SPEC §5): a `bool` is `true` or
//! `false` in any case, an `int` only decimal digits with an optional
//! sign, a `float` only a decimal number (never `inf`, `NaN`, `.5` or a
//! hex float), and nothing is trimmed, `csv` items included; anything
//! else is `invalid_type`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate self as docuconf;

#[cfg(doctest)]
mod compile_errors;
pub mod contract;
mod cue;
mod decl;
mod duration;
mod env;
mod error;
mod export;
mod files;
mod load;
mod overlay;
#[cfg(feature = "tls")]
mod pki;
mod reload;
mod schema;
mod types;
mod value;

#[doc(hidden)]
pub mod __private;

pub use contract::Contract;
pub use docuconf_derive::{Docuconf, DocuconfEnum};
pub use duration::format_go;
pub use error::{Code, DeclarationError, Error, ValidationError, Violation};
pub use export::Meta;
pub use load::{Export, Loader};
pub use overlay::{Overlay, OverlayFormat, Reload};
pub use reload::{RejectedReload, ReloadStatus, Subscription, Watched};
#[cfg(feature = "keystore")]
pub use types::Keystore;
pub use types::{BinaryFile, ConfigFile, Json, KeySet, Secret, TextFile};
#[cfg(feature = "tls")]
pub use types::{CaBundle, TlsKeyPair};

/// Re-exported for `#[serde(with = "docuconf::humantime_serde")]` on
/// `Duration` fields (and `docuconf::humantime_serde::option` on
/// `Option<Duration>`).
pub use humantime_serde;
/// Re-exported so apps and docuconf agree on the version.
pub use {figment, schemars, url};

/// The `schemars` trait and derive, re-exported so a type in a
/// [`ConfigFile<T>`] or [`Json<T>`] needs no `schemars` dependency of its
/// own. The derive generates `schemars::` paths, so point it at this
/// re-export with `#[schemars(crate = "docuconf::schemars")]`:
///
/// ```
/// #[derive(serde::Deserialize, docuconf::JsonSchema)]
/// #[schemars(crate = "docuconf::schemars")]
/// struct Fees {
///     basis_points: std::collections::BTreeMap<String, u32>,
/// }
/// ```
pub use schemars::JsonSchema;

/// A configuration struct with a docuconf declaration. Derive it with
/// `#[derive(Docuconf)]`.
pub trait Docuconf {
    /// Prefix of every variable name, from `#[docuconf(prefix = "...")]`.
    #[doc(hidden)]
    const PREFIX: &'static str;

    /// Declares the struct's fields.
    #[doc(hidden)]
    fn declare_fields(cx: &mut __private::DeclCx);
}

/// Loads `C` from the process environment and its declared files, with
/// default options. See [`Loader`] for config files, profiles and tests,
/// and [`load_or_exit`] for `main`.
pub fn load<C: Docuconf + serde::de::DeserializeOwned>() -> Result<C, Error> {
    Loader::<C>::new().load()
}

/// Loads `C` like [`load`]; on failure prints every problem to stderr and
/// exits with status 1 (no panic, no backtrace):
///
/// ```text
/// docuconf: 2 configuration problems:
///   DATABASE_URL: is required but not set (missing_required)
///   PORT: 0 is below min 1 (out_of_range)
/// ```
///
/// The problems are also written to the termination log.
pub fn load_or_exit<C: Docuconf + serde::de::DeserializeOwned>() -> C {
    Loader::<C>::new().load_or_exit()
}

/// When the program was started as `<program> export [--check] [PATH]`,
/// writes (or, with `--check`, verifies) `C`'s contract and exits;
/// otherwise returns. See [`Loader::export_command`].
pub fn export_command<C: Docuconf + serde::de::DeserializeOwned>(meta: &Meta) {
    Loader::<C>::new().export_command(meta)
}

/// Panics when the contract file at `path` is stale; rewrites it when
/// `UPDATE_CONTRACT=1`. For a unit test. See [`Loader::assert_contract`].
#[track_caller]
pub fn assert_contract<C: Docuconf + serde::de::DeserializeOwned>(
    meta: &Meta,
    path: impl AsRef<std::path::Path>,
) {
    Loader::<C>::new().assert_contract(meta, path)
}

/// Exports `C`'s contract as CUE. See [`Loader::export`] to include the
/// values in the app's config files.
pub fn export<C: Docuconf + serde::de::DeserializeOwned>(
    meta: &Meta,
) -> Result<String, DeclarationError> {
    Loader::<C>::new().export(meta)
}

/// Checks `C`'s declaration without reading anything: names, descriptions,
/// defaults against constraints, RE2 patterns, file paths. Handy in a unit
/// test.
pub fn check_declaration<C: Docuconf>() -> Result<(), DeclarationError> {
    decl::declaration::<C>().map(|_| ())
}
