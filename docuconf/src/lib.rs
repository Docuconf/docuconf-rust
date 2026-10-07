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
//! #[derive(Deserialize, schemars::JsonSchema)]
//! struct Routes { routes: Vec<String> }
//!
//! let cue = docuconf::export::<Config>(&docuconf::Meta::new("billing-api")).unwrap();
//! assert!(cue.contains("DATABASE_URL: {"));
//! ```
//!
//! At boot, `let config: Config = docuconf::load()?;`.
//! [`Loader`] adds the app's own config files, profiles and a platform
//! config-file [`Overlay`]. [`Contract`] validates an environment against a
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
//! | `Vec<String>`, `Vec<int>` | `list`, encoding `json`, with an int item type's range as `itemMin`/`itemMax` (narrow it with `item_min`/`item_max`); string items take `item_min_length`/`item_max_length` |
//! | [`Json<T>`] | `json`, schema from `T: JsonSchema` (`max_length` on its wire string) |
//! | [`Secret<T>`] | `T`, with `secret: true` |
//! | `Option<T>` | `T`, optional |
//! | [`ConfigFile<T>`], [`TlsKeyPair`], [`CaBundle`], [`Keystore`], [`TextFile`], [`BinaryFile`] | file inputs |
//! | a nested `#[derive(Docuconf)]` struct | its fields, named `PARENT__CHILD` |
//!
//! A field without `Option` and without a `default` is required.
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
//! parses), durations are parsed with `humantime` (which reads Go syntax
//! such as `1m30s`), and an empty value is unset for every type but
//! `string`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate self as docuconf;

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
pub use load::Loader;
pub use overlay::{Overlay, OverlayFormat, Reload};
pub use types::{BinaryFile, CaBundle, ConfigFile, Json, Keystore, Secret, TextFile, TlsKeyPair};

/// Re-exported for `#[serde(with = "docuconf::humantime_serde")]` on
/// `Duration` fields.
pub use humantime_serde;
/// Re-exported so apps and docuconf agree on the version.
pub use {figment, schemars, url};

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
/// default options. See [`Loader`] for config files, profiles and tests.
pub fn load<C: Docuconf + serde::de::DeserializeOwned>() -> Result<C, Error> {
    Loader::<C>::new().load()
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
