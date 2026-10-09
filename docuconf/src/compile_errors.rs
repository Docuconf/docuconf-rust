//! Declaration mistakes that `#[derive(Docuconf)]` rejects at compile time.
//! Each block below must fail to compile; `cargo test --doc` checks it.
//! Mistakes visible in the attributes alone are `compile_error!`s spanned
//! on the attribute; the ones that also need the field's type are `const`
//! evaluation failures (`E0080`) spanned on the field. The messages are
//! checked in `docuconf-derive`'s unit tests and by hand.
//!
//! The baseline compiles:
//!
//! ```
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// HTTP listen port.
//!     #[docuconf(default = 8080, min = 1)]
//!     port: u16,
//!     /// Upstream request timeout.
//!     #[docuconf(default = "30s")]
//!     #[serde(with = "docuconf::humantime_serde")]
//!     timeout: std::time::Duration,
//!     /// Optional upstream connect timeout.
//!     #[serde(default, with = "docuconf::humantime_serde::option")]
//!     connect_timeout: Option<std::time::Duration>,
//! }
//! ```
//!
//! A field without a description (`docuconf: Config.port needs a
//! description: add a /// doc comment above the field`):
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     #[docuconf(default = 8080)]
//!     port: u16,
//! }
//! ```
//!
//! Blank details (`details must not be blank`); details over 4000
//! characters fail the same way:
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// HTTP listen port.
//!     #[docuconf(default = 8080, details = " ")]
//!     port: u16,
//! }
//! ```
//!
//! A default of the wrong kind (`default "abc" is not an integer`):
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// HTTP listen port.
//!     #[docuconf(default = "abc")]
//!     port: u16,
//! }
//! ```
//!
//! A default outside the field type's range (`default 300 is outside the
//! range of u8`):
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Threads serving requests.
//!     #[docuconf(default = 300)]
//!     workers: u8,
//! }
//! ```
//!
//! A default that breaks its own bounds (`default 100 is above max 64`):
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Threads serving requests.
//!     #[docuconf(default = 100, max = 64)]
//!     workers: u8,
//! }
//! ```
//!
//! A secret with a default, through `Secret<T>` (E0080) or `secret`:
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Database password.
//!     #[docuconf(default = "changeme")]
//!     db_password: docuconf::Secret<String>,
//! }
//! ```
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Database password.
//!     #[docuconf(secret, default = "changeme")]
//!     db_password: String,
//! }
//! ```
//!
//! `required` with a default:
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// HTTP listen port.
//!     #[docuconf(required, default = 8080)]
//!     port: u16,
//! }
//! ```
//!
//! A `Duration` without `humantime_serde`, and an `Option<Duration>` without
//! `humantime_serde::option` and `default`:
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Upstream request timeout.
//!     #[docuconf(default = "30s")]
//!     timeout: std::time::Duration,
//! }
//! ```
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Upstream request timeout.
//!     #[serde(with = "docuconf::humantime_serde")]
//!     timeout: Option<std::time::Duration>,
//! }
//! ```
//!
//! An enum default that is not one of its values:
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::DocuconfEnum)]
//! #[serde(rename_all = "lowercase")]
//! enum Level { Debug, Info }
//!
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Minimum log level emitted.
//!     #[docuconf(default = "verbose")]
//!     log_level: Level,
//! }
//! ```
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Deployment environment.
//!     #[docuconf(values("dev", "prod"), default = "staging")]
//!     environment: String,
//! }
//! ```
//!
//! A field type docuconf cannot declare (``HashMap<String, String>` is not a
//! docuconf input type``):
//!
//! ```compile_fail,E0277
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Extra labels.
//!     labels: std::collections::HashMap<String, String>,
//! }
//! ```
//!
//! A misspelt attribute:
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// HTTP listen port.
//!     #[docuconf(defualt = 8080)]
//!     port: u16,
//! }
//! ```
//!
//! A blank deprecation message (`deprecated must say what to use instead,
//! or why the input is going away`); one over 500 characters fails the
//! same way:
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Port the service used to listen on.
//!     #[docuconf(deprecated = " ")]
//!     old_port: Option<u16>,
//! }
//! ```
//!
//! A deprecated input marked `required` (`a required input cannot be
//! deprecated`); a field that is required because it is neither an
//! `Option` nor defaulted is a declaration error at boot instead:
//!
//! ```compile_fail
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Port the service used to listen on.
//!     #[docuconf(required, deprecated = "Use PORT instead")]
//!     old_port: Option<u16>,
//! }
//! ```
//!
//! A default on a key set, which is always secret:
//!
//! ```compile_fail,E0080
//! #[derive(serde::Deserialize, docuconf::Docuconf)]
//! struct Config {
//!     /// Keys that verify webhook signatures.
//!     #[docuconf(default = "a,b")]
//!     keys: docuconf::KeySet,
//! }
//! ```
