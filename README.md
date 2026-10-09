# docuconf for Rust

Documentation: [docuconf.dev](https://docuconf.dev) · [Rust guide](https://docuconf.dev/languages/rust/)

Typed configuration contracts for [figment](https://docs.rs/figment) and serde. Keep your
`#[derive(Deserialize)]` config struct, add `#[derive(Docuconf)]`, and the struct becomes a contract that your
Kubernetes platform checks **before deploy** and your service checks again **at boot**. It covers environment
variables, figment's config files and profiles, and file inputs: TLS key pairs, CA bundles, PKCS#12 keystores,
JSON/YAML/TOML config files, text and binary files.

Part of [docuconf](https://github.com/docuconf). See the
[specification](https://github.com/docuconf/docuconf-go/blob/main/spec/SPEC.md).
**Example:** [`examples/orders/`](examples/orders), a small HTTP service with its declaration, exported contract
and boot-time errors.

> **Status:** `0.1.0`, not yet on crates.io. The contract format is a draft (`v1alpha1`) and the API may change.

## Install

Until the first release is published, depend on this repository with git:

```sh
cargo add docuconf --git https://github.com/docuconf/docuconf-rust
cargo add serde --features derive
```

That is all a config struct needs: `docuconf` re-exports figment, schemars and url at the versions it uses.
`cargo add docuconf serde --features serde/derive` will work once `0.1.0` is on crates.io.

A service that reads only environment variables can drop the TLS stack (rustls, ring, webpki, x509-parser,
PKCS#12), which only the `TlsKeyPair`, `CaBundle` and `Keystore` file inputs need:

```sh
cargo add docuconf --git https://github.com/docuconf/docuconf-rust --no-default-features
```

## Declare and load

`src/main.rs`:

```rust,no_run
use std::time::Duration;

use docuconf::{ConfigFile, Docuconf, DocuconfEnum, Secret};
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
pub struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    pub port: u16,

    /// Primary Postgres connection string.
    #[docuconf(schemes("postgres", "postgresql"))]
    pub database_url: Secret<String>,

    /// Minimum log level emitted.
    #[docuconf(default = "info")]
    pub log_level: LogLevel,

    /// Upstream request timeout.
    #[docuconf(default = "30s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    pub request_timeout: Duration,

    /// Fee schedule, one entry per currency.
    #[docuconf(path = "/etc/billing/fees/fees.yaml", path_env = "FEES_FILE")]
    pub fees: Option<ConfigFile<Fees>>,
}

#[derive(Debug, Deserialize, DocuconfEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel { Debug, Info, Warn, Error }

#[derive(Debug, Deserialize, docuconf::JsonSchema)]
#[schemars(crate = "docuconf::schemars")]
pub struct Fees {
    pub basis_points: std::collections::BTreeMap<String, u32>,
}

fn main() {
    // `<program> export [--check] contract.cue` writes or checks the contract, then exits.
    docuconf::export_command::<Config>(&docuconf::Meta::new("billing-api"));

    // Every check, in one pass. On failure: the report on stderr, exit status 1.
    let config: Config = docuconf::load_or_exit();
    println!("listening on :{} (log level {:?})", config.port, config.log_level);
}
```

The `///` doc comment documents the input. Its first paragraph is the `description` (required, at least 5
characters, joined onto one line; a trailing period is dropped), and the rest of the comment is the `details`:
CommonMark used only in generated docs, never at runtime, at most 4000 characters:

```rust
use std::time::Duration;

#[derive(serde::Deserialize, docuconf::Docuconf)]
pub struct Config {
    /// Upstream request timeout.
    ///
    /// The gateway gives up after this long and answers 504. Keep it below the
    /// load balancer's idle timeout; see [`Duration`].
    ///
    /// ```
    /// # use std::time::Duration;
    /// let t = Duration::from_secs(30);
    /// ```
    #[docuconf(default = "30s", min = "1s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    pub request_timeout: Duration,
}
```

exports

```cue
REQUEST_TIMEOUT: {
	type:        "duration"
	description: "Upstream request timeout"
	details:     "The gateway gives up after this long and answers 504. Keep it below the\nload balancer's idle timeout; see `Duration`.\n\n```rust\nlet t = Duration::from_secs(30);\n```"
	...
```

Rustdoc syntax becomes CommonMark: intra-doc links (`` [`Duration`] ``, `[crate::Loader]`, `[text](crate::Meta)`)
become code spans or their text, and code blocks lose doctest attributes (`ignore`, `no_run`, ...) and hidden `# `
lines. A comment that starts with a list, a heading or a code block is all description. `#[docuconf(description =
"...")]` and `#[docuconf(details = "...")]` override the comment. Declaration and export fail when an input has no
description, or details that are blank or over 4000 characters (Unicode code points). A contract-first
[`Contract`](https://docs.rs/docuconf/latest/docuconf/contract/) accepts and ignores `details`.

`docuconf docs` (in the [docuconf CLI](https://github.com/docuconf/docuconf-go)) generates CONFIG.md and
CONFIG.agents.md from the exported contract; the SDK only exports the text.

A field that is not an `Option` and has no `default` is required. The Rust type picks the contract type, and the
variable name is the field name in upper case: `PORT`, `DATABASE_URL`. `Secret<T>` marks a variable secret: its
`Debug` and `Serialize` output is `***`, and docuconf never prints its value.

The derive catches declaration mistakes **at compile time**: a missing description, a default of the wrong type
or outside its bounds, a secret or `required` variable with a default, a `Duration` without
`humantime_serde`, a misspelt attribute, a field type docuconf cannot declare:

```text
error[E0080]: evaluation panicked: docuconf: Config.port: default "abc" is not an integer; u16 needs a default such as 8080
error: docuconf: Config.workers: default 100 is above max 64; lower the default or raise max
error: docuconf: unknown attribute `defualt`
```

## See an error

Run it without `DATABASE_URL` and with a bad `PORT`:

```console
$ PORT=80x cargo run
docuconf: 2 configuration problems:
  DATABASE_URL: is required but not set (missing_required)
  PORT: "80x" is not an integer (invalid_type)
$ echo $?
1
```

Every problem is reported at once, sorted by name, each with the spec's stable code. Secret values never
appear. The report is also written to `/dev/termination-log` when it exists (or to `DOCUCONF_TERMINATION_LOG`),
so `kubectl describe pod` shows it.

A variable that is set but not declared, and is one or two edits away from a declared name, gets a warning on
stderr (the value is never shown):

```text
docuconf: DATABSE_URL is set but not declared; did you mean DATABASE_URL?
```

`Loader::on_warning` routes warnings elsewhere, for example `.on_warning(|w| tracing::warn!("{w}"))`.

If you prefer `?`, `docuconf::load::<Config>()` returns a `docuconf::Error` whose `Debug` is the same report and
which has no `source()`, so `fn main() -> Result<(), Box<dyn Error>>` and `anyhow::Result` both print it once.

## Test your config

`Loader::env` takes the whole environment as a map. It neither reads nor changes the process environment, starts
no threads, and writes no termination log, so tests can run in parallel. Put `DOCUCONF_FILE_ROOT` in the map to
read file inputs from a test directory.

`tests/config.rs`:

```rust,test_harness
use docuconf::{Code, Docuconf, Loader, Secret};
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
pub struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    pub port: u16,

    /// Primary Postgres connection string.
    #[docuconf(schemes("postgres"))]
    pub database_url: Secret<String>,
}

#[test]
fn loads_a_valid_environment() {
    let config = Loader::<Config>::new()
        .env([("DATABASE_URL", "postgres://app@db/app"), ("PORT", "9090")])
        .load()
        .unwrap();
    assert_eq!(config.port, 9090);
}

#[test]
fn reports_every_problem() {
    let err = Loader::<Config>::new().env([("PORT", "0")]).load().unwrap_err();
    assert_eq!(err.violations().len(), 2);
    assert!(err.has(Code::MissingRequired));
    assert!(err.has(Code::OutOfRange));
}
```

## Export the contract

The platform validates values against `contract.cue`, which the app exports from its own struct. With
`export_command` at the top of `main` (see [Declare and load](#declare-and-load)):

```sh
cargo run -- export contract.cue          # write it; commit it next to the code
cargo run -- export --check contract.cue  # in CI: exit 1, showing the first difference, when it is stale
```

The same check as a unit test, if you would rather not have an export path in the production binary
(`UPDATE_CONTRACT=1 cargo test` rewrites the file):

```rust,no_run,test_harness
use docuconf::Docuconf;
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
pub struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    pub port: u16,
}

#[test]
fn contract_is_current() {
    docuconf::assert_contract::<Config>(&docuconf::Meta::new("billing-api"), "contract.cue");
}
```

The output is plain CUE data that unifies with the meta-schema's `#Contract` (`generator.language: "rust"`),
variables and files sorted by name, and is deterministic. Warnings (feature-flag-like names such as `ENABLE_*`,
config-file keys that are not declared) are printed by the command; `Loader::export_with_warnings` returns them.
`Meta::new(name).app_version("1.4.0").package("billing")` sets the metadata.

## Deploy

Ship `contract.cue` with the app. The platform checks its inputs against it before anything reaches the
cluster: `docuconf vet` reports every bad or missing value, secret given as a literal or policy violation, and
`docuconf render` turns valid inputs into the pod's env. A Helm-based platform can use the
[docuconf Helm chart](https://github.com/docuconf/docuconf-go/tree/main/helm), which generates a
`values.schema.json` from the contract. At boot, `load_or_exit` checks the same rules again.

## With axum

There is nothing to plug in: load the config in plain `main` before building the tokio runtime, so a bad
config exits before anything starts and the config can size the runtime, then share it as axum state.

```rust,no_run
use std::sync::Arc;

use axum::{extract::State, routing::get, Router};
use docuconf::Docuconf;
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    port: u16,

    /// Threads serving requests.
    #[docuconf(default = 4, min = 1, max = 64)]
    worker_count: u8,
}

fn main() {
    docuconf::export_command::<Config>(&docuconf::Meta::new("orders-api"));
    let config = Arc::new(docuconf::load_or_exit::<Config>());
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.worker_count.into())
        .enable_all()
        .build()
        .expect("start the tokio runtime")
        .block_on(serve(config));
}

async fn serve(config: Arc<Config>) {
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/port", get(|State(c): State<Arc<Config>>| async move { c.port.to_string() }))
        .with_state(Arc::clone(&config));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
        .await
        .expect("bind the port");
    axum::serve(listener, app).await.expect("serve");
}
```

With `#[tokio::main]` instead, call `load_or_exit` as the first line of `async fn main`; it does no I/O beyond
reading the environment and the declared files.

---

# Reference

## Types

| Field type | Contract |
|---|---|
| `String` | `string` (`min_length`, `max_length`, `pattern`); `url` with `schemes(...)` or `url` (`max_length`); `enum` with `values(...)` |
| `i8`..`i64`, `u8`..`u64`, `isize`, `usize` | `int`, with the type's range as `min`/`max` |
| `f32`, `f64` | `float` (decimal only; NaN and infinity rejected) |
| `bool` | `bool` (`true`/`false`, any case, nothing else) |
| `std::time::Duration` + `#[serde(with = "docuconf::humantime_serde")]` | `duration`, encoding `go` |
| `Option<Duration>` + `#[serde(default, with = "docuconf::humantime_serde::option")]` | optional `duration` |
| `url::Url` (`docuconf::url::Url`) | `url` (`schemes`, `max_length`) |
| `#[derive(DocuconfEnum)]` enum | `enum`, values after serde renames |
| `Vec<String>`, `Vec<u16>`... | `list`, encoding `json`, or `csv` with `encoding = "csv"` (`separator`, `,` by default) (`min_items`, `max_items`); string items take `item_min_length`/`item_max_length`; an int item type narrower than 64 bits exports its range as `itemMin`/`itemMax` |
| `docuconf::KeySet` | `keySet`, always secret: encoding `csv` (or `json` with `encoding = "json"`, `separator`), `min_keys` (default 1), `max_keys` (default 2), `key_min_length`, `key_max_length` |
| `docuconf::Json<T>` (`T: JsonSchema`) | `json`, with the schema from `T` (`max_length`) |
| `docuconf::Secret<T>` | `T` with `secret: true`; `Debug` prints `Secret(***)`, `Serialize` writes `"***"` |
| `secrecy::SecretString`, `secrecy::SecretBox<T>` (feature `secrecy`) | `string` / `T` with `secret: true`, zeroized on drop |
| `Option<T>` | optional |
| nested `#[derive(Docuconf)]` struct | its variables, as `PARENT__CHILD` |
| `ConfigFile<T>` (`T: Deserialize + JsonSchema`) | file `config` (`format` from the extension or `format = "..."`) |
| `TlsKeyPair` (feature `tls`) | file `tls`: `dns_names`, `key_algorithms`, `min_remaining`, `require_ca` |
| `CaBundle` (feature `tls`) | file `caBundle`: `min_certificates` |
| `Keystore` (feature `keystore`) | file `keystore` (PKCS#12): `password_var` names a secret variable |
| `TextFile` | file `text`: `pattern`, `min_length`, `max_length` |
| `BinaryFile` | file `binary` |
| `Watched<T>`, `T` any file type above or an `Option` of one | the same file input with `reload: "watch"` |

For `ConfigFile<T>` and `Json<T>`, derive `docuconf::JsonSchema` with `#[schemars(crate = "docuconf::schemars")]`
as in the first example; then the app needs no `schemars` dependency. (If the app depends on `schemars` 1.x
itself, a plain `#[derive(schemars::JsonSchema)]` works too.)

A field type docuconf cannot declare is a compile error: ``HashMap<String, String>` is not a docuconf input
type``. Use `Json<HashMap<..>>` for a map, or `#[docuconf(skip)]` to load the field some other way.

## Attributes

Variable attributes: `default`, `required`, `secret`, `min`, `max`, `min_length`, `max_length`, `pattern` (RE2,
matches anywhere: anchor with `^`/`$`), `values`, `schemes`, `min_items`, `max_items`, `item_min`, `item_max`,
`item_min_length`, `item_max_length`, `encoding` (`"json"` or `"csv"`, for a list), `separator`, `group`, `examples`, `deprecated`, `replaced_by`, `config_key`, `env`,
`description`, `details`, `skip`. File attributes: `path` (required), `name` (input name; default is the field name with
`-`), `path_env`, `reload` (`"restart"`, or `"watch"` on a `Watched<T>` field, which declares it anyway), `max_size`
(`65536` or `"64Ki"`), `required`, `secret`, `description`, `details`, `group`, `deprecated`, plus the type-specific ones above. The struct
takes `prefix`.

`item_min` and `item_max` bound each item of an int list, and an item outside them is `out_of_range` at boot.
They are narrowed to the item type, as `min`/`max` are for an int variable, so `Vec<u16>` always exports
`itemMin: 0, itemMax: 65535` or tighter:

```rust
#[derive(serde::Deserialize, docuconf::Docuconf)]
pub struct Sharding {
    /// Shard ids this instance owns.
    #[docuconf(item_min = 0, item_max = 1023)]
    pub shards: Vec<u16>,
}
```

Lengths count characters (Unicode code points, as `chars().count()` does), never bytes: `"日本"` is 2 and
`"ZÜ01"` fits `item_max_length = 4`. `min_length`/`max_length` bound a `String`; `max_length` also bounds a `url`
as it is given and a `json` value's wire string, measured as the app receives it (whitespace included) and, for a
default or a config-file value, as compact JSON. `item_min_length` and `item_max_length` bound each item of a
string list after it is split, so separators never count. Every one of them is `out_of_range` at boot, and a
secret reports its length, never its value:

```rust
#[derive(serde::Deserialize, docuconf::Docuconf)]
pub struct Reporting {
    /// Where to report each run.
    #[docuconf(schemes = "https", max_length = 40)]
    pub callback: docuconf::url::Url,
    /// Branch codes, two to four characters each.
    #[docuconf(item_min_length = 2, item_max_length = 4)]
    pub branches: Vec<String>,
}
```

A list is a JSON array (`["a","b"]`) unless it says `encoding = "csv"`; then it is the items joined by `separator`
(`a,b`), and a default item may not contain the separator. A secret list is a `Secret<Vec<String>>`: the contract
marks it `secret: true`, and `Debug` and `Serialize` never show a key. Accepting either of two keys is how a key is
rotated without downtime ([spec section 6.1](https://github.com/docuconf/docuconf-go/blob/main/spec/SPEC.md#61-rotation));
the item lengths stop an empty or truncated key at boot:

```rust
#[derive(serde::Deserialize, docuconf::Docuconf)]
pub struct Webhooks {
    /// Keys that verify the signature on incoming payment webhooks.
    #[docuconf(encoding = "csv", min_items = 1, max_items = 2, item_min_length = 32, item_max_length = 256)]
    pub webhook_keys: Option<docuconf::Secret<Vec<String>>>,
}
```

Mistakes that need the whole declaration (a bad or duplicate name, a pattern with lookaround, a duration default
above its max, a file mounted over `/etc`) are reported by `docuconf::check_declaration::<Config>()`, by export
and by load.

## Names

Variable names follow figment's `Env::prefixed(prefix).split("__")`: the struct's
`#[docuconf(prefix = "APP_")]` (empty by default), then the serde key in upper case, with `__` between nesting
levels. `cache.ttl` under prefix `APP_` is `APP_CACHE__TTL`. `#[docuconf(env = "NAME")]` overrides one name.

The serde key is upper-cased as it is, with no `_` inserted: under `#[serde(rename_all = "camelCase")]` the field
`request_timeout` is the key `requestTimeout` and the variable `REQUESTTIMEOUT`. Give such fields
`#[docuconf(env = "REQUEST_TIMEOUT")]`; at boot, a set `REQUEST_TIMEOUT` gets a "did you mean" warning.

## Key sets

A `KeySet` holds the keys that are all valid at once on the side that verifies (webhook signatures, inbound
API keys), so a key can be rotated without an outage (spec §4.3, §6.1). The platform supplies it as one Secret
key holding `old,new` during a rotation; keys are never trimmed, and an empty key (a stray comma) is always
`out_of_range`. Like `Secret`, it prints and serializes as `***`, and errors never show a key.

```rust
use docuconf::{Docuconf, KeySet};
use serde::Deserialize;

#[derive(Deserialize, Docuconf)]
struct Config {
    /// Keys that verify the signature on incoming webhooks.
    #[docuconf(key_min_length = 32, key_max_length = 256)]
    webhook_keys: KeySet,
}

fn accept(config: &Config, presented_key: &str, body: &[u8], signature: &[u8]) -> bool {
    // An API key a caller presents: compared with every key in constant time.
    let _ = config.webhook_keys.contains(presented_key);
    // An HMAC: tries every key, even after a match. Compare in constant time
    // inside the closure (hmac's `verify_slice` does).
    config.webhook_keys.verify(|key| [key, body].concat() == signature)
}
```

The generated docs print the three rotation steps for every key set, so a field's doc comment need not repeat
them.

## Wire formats

docuconf reads the declared variables itself and hands figment typed values. figment's own `Env` provider trims
values and guesses types (`8080` is a number even for a `String` field), which the spec forbids, so do not add
it alongside docuconf. Values are never trimmed; an empty value is unset for every type except `string`; lists
are JSON arrays (`["a","b"]`, which figment's `Env` also reads), so the contract says `encoding: "json"`;
durations use Go's syntax, exactly what Go's `time.ParseDuration` accepts (`1m30s`, `1.5h`, `250ms`; not `2d`
or `1 hour`, which the platform's `docuconf vet` would reject), so the contract says `encoding: "go"`.

Parsing is strict, with one exact rule per type (spec §5), whatever Rust's own parsers would take: a `bool` is
`true` or `false` in any case, never `1`, `t` or `yes`; an `int` is `^[+-]?[0-9]+$` in base 10 (`007` is 7; never
`0x10`, `1_000` or `1e3`), and a value outside the field's type is `out_of_range`; a `float` is
`^[+-]?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?$` (never `inf`, `NaN`, `.5`, `5.` or a hex float); a `go` duration takes
a sign, but a negative one is `out_of_range` for a `std::time::Duration` field. Nothing is trimmed, including
`csv` items: `a, b` is `a` and ` b`. Anything else is `invalid_type`.

## File inputs

File checks: the file exists, is readable and within `max_size`; config files parse (figment's JSON/YAML/TOML
parsers; a UTF-8 BOM is accepted), match the `schemars` schema (checked with `jsonschema`) and bind to `T`;
`tls` key pairs parse, the key matches the certificate (rustls), the certificate is valid with at least
`min_remaining` left, covers every `dns_names` entry (webpki, one-label wildcards), uses an allowed key algorithm
and, with `require_ca`, chains to `ca.crt` (webpki); CA bundles have `min_certificates` certificates; PKCS#12
keystores open with their password (`p12-keystore`); text files are UTF-8 and match their constraints.

`DOCUCONF_FILE_ROOT` is prepended to every absolute file path, including paths read from a `path_env` variable,
for local development and tests.

## Reloading files

A file input whose field is `Watched<T>` is declared `reload: "watch"`: the app rereads it when it changes, so
a renewed certificate or an updated ConfigMap reaches it without a rollout, and the platform does not list its
source as a restart trigger.

```rust
use docuconf::{ConfigFile, TextFile, Watched};

#[derive(serde::Deserialize, docuconf::Docuconf)]
pub struct Config {
    /// Feature flags, changed without a rollout.
    #[docuconf(path = "/etc/app/flags/flags.json")]
    pub flags: Watched<ConfigFile<Flags>>,

    /// Banner shown on the home page, when there is one.
    #[docuconf(path = "/etc/app/banner/banner.txt", max_length = 200)]
    pub banner: Watched<Option<TextFile>>,
}

#[derive(serde::Deserialize, docuconf::JsonSchema)]
#[schemars(crate = "docuconf::schemars")]
pub struct Flags {
    pub new_checkout: bool,
}

pub fn new_checkout(config: &Config) -> bool {
    config.flags.current().new_checkout
}
```

`current()` returns an `Arc<T>` of the current content. At most once a second (`Loader::watch_interval`
changes that) it first looks at the metadata of the files the input reads, following symlinks, so the
`..data` symlink swap Kubernetes makes when it updates a projected volume shows up as a different file; there is
no background thread and no extra dependency. `refresh()` looks right away. A change is read again and passes
the same checks as at boot. A changed file that fails them is not used: the previous content stays current and
each problem goes to `Loader::on_warning` (stderr by default) once per change, with its code and never the
file's content, for example `settings: changed file rejected, keeping the previous content: is not a valid JSON
document: ... (file_malformed)`. An optional input is `Watched<Option<T>>`, so a file that appears after boot
is picked up (and one that is removed becomes `None`); `Option<Watched<T>>` is a declaration error, and
`reload = "watch"` on a field that is not `Watched<T>` is a compile error. Clones of a `Watched<T>` share the
content, so hand one to each handler. Kubernetes never updates a file mounted with `subPath`: mount the
directory.

## Cargo features

| Feature | Default | Adds |
|---|---|---|
| `tls` | yes | `TlsKeyPair` and `CaBundle` (rustls, ring, webpki, x509-parser) |
| `keystore` | yes | `Keystore`, PKCS#12 (implies `tls`) |
| `secrecy` | no | `secrecy::SecretString` and `secrecy::SecretBox<T>` as secret fields |

## Injected secrets

Platforms often inject values when the container starts: Bank-Vaults' `vault-env` resolves `vault:` references,
`op run` resolves `op://` ones, operators add variables. docuconf reads the environment as the process sees it,
after injection, so injected values are validated like any other and docuconf never resolves a reference itself.
When the injector did not run, a secret variable still holds the raw reference; a value starting with `vault:`,
`op://` or `ref+` fails with `invalid_type`, naming the variable and the scheme but never the value:

```text
DATABASE_URL: holds an unresolved vault: reference; the injector that should resolve it did not run (invalid_type)
```

## Config files and profiles

Give the loader your figment file layers and the variable that selects the profile:

```rust,no_run
use docuconf::figment::providers::{Format, Toml};
use docuconf::figment::Figment;
use docuconf::{Docuconf, Loader};
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080)]
    port: u16,

    /// Selects the profile in App.toml.
    app_profile: Option<String>,
}

fn main() {
    let config: Config = Loader::new()
        .figment(Figment::from(Toml::file("App.toml").nested()))
        .profiles("APP_PROFILE", "production") // APP_PROFILE must be a declared variable
        .load_or_exit();
    println!("port {}", config.port);
}
```

Layers, lowest first: the declaration's defaults, your figment (its `[default]` and `[global]` tables are
always loaded, a `[production]` table only when that profile is selected), then the environment, so the
platform's variables override file values. A selector value that names a profile no config file defines (and
that is not the default) loads only base values and the environment, as the spec allows (the platform may
supply that profile's values), with a warning naming the profiles the files do define. A selector that is not a
declared variable is a declaration error at boot as well as at export.

At export, values in the always-loaded tables become the variables' defaults and other tables become the
contract's `profiles.defaults`. A secret with a value in a config file is an error. Keys that are not declared
variables are file-only, and export warns that the platform cannot set them. Fields you load from elsewhere (a
vault, say) can be left out with `#[docuconf(skip)]`.

Other options: `.dotenv(".env")` reads a `.env` file for development (real variables win), `.env(map)` replaces
the process environment in tests, `.now(time)` fixes the clock for certificate checks, `.termination_log(bool)`
and `.on_warning(f)`.

## Config-file overlays

The platform can supply values in one more config file, mounted from a ConfigMap, instead of the environment
(spec §4.7). Declare it on the loader, and use the same loader for export:

```rust,no_run
use docuconf::figment::providers::{Format, Toml};
use docuconf::figment::Figment;
use docuconf::{Docuconf, Loader, Meta, Overlay};
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080)]
    port: u16,
}

fn loader() -> Loader<Config> {
    Loader::new()
        .figment(Figment::from(Toml::file("App.toml").nested()))
        .overlay(Overlay::new("platform", "/etc/app/platform/app.toml")) // format from the extension
}

fn main() {
    loader().export_command(&Meta::new("billing-api"));
    let config = loader().load_or_exit();
    println!("port {}", config.port);
}
```

Layers, lowest first: declaration defaults, your figment (base and selected profile), the overlay, the
environment. The overlay goes in figment's global profile, so it also beats a `[global]` table. A missing overlay
is fine; one that cannot be read or parsed is a `file_unreadable` or `file_malformed` violation named after the
overlay, and its values are checked like any other. The format is TOML, JSON or YAML (from the extension, or
`.format(OverlayFormat::Json)`).

The export adds `overlays.platform` (`keySeparator: "."`, `reload: "restart"`) and a `configKey` on every
variable the overlay may carry: figment's dotted key path, such as `cache.ttl` for `APP_CACHE__TTL`. Secrets and
the profile selector get none. With overlays declared, a `#[docuconf(config_key = ...)]` must equal that path.

docuconf refuses, at export and at boot, an overlay whose directory is reserved, shared with a file input, or
holds files the app ships with (the directory of a config file in your figment, or of the executable), because
the mount would hide them. `Reload::Watch` is rejected: the overlay is read once at boot, and a change rolls the
pods.

## Contract-first mode

`docuconf::Contract` validates an environment against a contract given as JSON (`cue export contract.cue`), with
no Rust declaration, and returns typed values. Use it for a contract written by hand in CUE, or to check an
environment in a tool. It parses every wire encoding of spec §5 (lists `csv` with any `separator`, `json` and
`indexed` as `NAME__0`, `NAME__1`..., numbered from 0 with no gap; durations `go`, `iso8601`, `seconds` and
`timespan`) and runs the same checks as a `#[derive(Docuconf)]` struct, so both accept exactly the same values:

```rust,no_run
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let contract = docuconf::Contract::from_json(&std::fs::read_to_string("contract.json")?)?;
    let values = contract.load()?; // the process environment; or load_env([("PORT", "9090")]) in tests
    let port = values.get("PORT").and_then(|v| v.as_int());
    println!("port {port:?}");
    Ok(())
}
```

`load()` reports every violation together and writes them to the termination log, as `Loader::load` does;
`load_env(...)` takes the whole environment as a map and writes nothing. `json` variables are checked against
their `schema`. The whole contract is loaded: variables are layered from their default, then the selected
profile's default, then a config-file overlay (read under `DOCUCONF_FILE_ROOT`, its native values converted to
their wire form), then the environment; every file input (`config` in JSON, YAML or TOML, `tls`, `caBundle`,
`keystore`, `text`, `binary`) is read and checked as at boot, and returned by `Values::file`. A `go` duration may
be negative here (`Value::NegativeDuration`), and a key set is a `Value::KeySet`.

## Conformance

`tests/conformance.rs` runs docuconf-go's shared conformance suite (spec §12, `conformance/cases.json`) through
contract-first mode. It reads `$DOCUCONF_CONFORMANCE`, or `../docuconf-go/conformance/cases.json` next to this
repository, and is skipped when neither exists unless `DOCUCONF_REQUIRE_CONFORMANCE=1`:

```sh
DOCUCONF_CONFORMANCE=../docuconf-go/conformance/cases.json DOCUCONF_REQUIRE_CONFORMANCE=1 \
  cargo test --test conformance -- --nocapture
```

Failures are reported by case id. The runner keeps an allow-list of the capability tags the SDK supports, and
skips (never runs) a case with a tag it does not know. The SDK supports every tag: `int64` (Rust holds every
64-bit integer), `json-schema` (`json` values are checked with the `jsonschema` crate), and the transitional
`key-set`, `deprecated`, `strict-parsing`, `files`, `profiles` and `overlays`. **No case is skipped**, and the
test fails if one is. (A build without the `tls` or `keystore` cargo feature cannot load TLS key pairs, CA
bundles or keystores, and skips just the cases that declare one.) CI runs the suite against the pinned
docuconf-go commit, and nightly against `main`.

`tests/conformance_export.rs` declares the shared export fixture (`conformance/export/fixture.yaml`) with this
SDK's attributes, exports it, and compares it with `conformance/export/golden.cue` using
`docuconf conformance export` (the CLI on `PATH` or `$DOCUCONF_CLI`; `DOCUCONF_REQUIRE_EXPORT=1` makes a missing
CLI a failure). The export must match with no difference; `tests/golden/gateway.cue` stays as well.

## Not yet supported

- `reload: "watch"` for overlays (`Reload::Watch` is rejected at declaration time; the overlay is read once at
  boot). File inputs support it through `Watched<T>`.
- JKS keystores (PKCS#12 only).
- Falling back from a variable to its `replaced_by` successor; deprecated variables only warn when set.
- Markdown docs generation (a SHOULD in the spec).

## Development

```sh
cargo test --workspace --all-features
cargo test -p docuconf --no-default-features --lib --tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
UPDATE_GOLDEN=1 cargo test --test export   # accept a changed golden export
```

Every Rust block in this README is compiled (and the test blocks run) by the `readme` crate's doctests, which
depend on nothing but `docuconf` and `serde` (plus axum and tokio for the axum section).

The export tests run `cue vet -c` against the meta-schema when `cue` (v0.17.1) is installed and the spec is at
`../docuconf-go/spec/cue` or `$DOCUCONF_SPEC_CUE`; they skip otherwise (`DOCUCONF_REQUIRE_VET=1` makes that a
failure). Minimum supported Rust version: **1.89** (set by the `aes` crate under `p12-keystore`).

## Licence

MIT. See [LICENSE](LICENSE).
