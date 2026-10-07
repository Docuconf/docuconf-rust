# docuconf for Rust

Typed configuration contracts for [figment](https://docs.rs/figment) and serde. Keep your
`#[derive(Deserialize)]` config struct, add `#[derive(Docuconf)]`, and the struct becomes a contract that your
Kubernetes platform checks **before deploy** and your service checks again **at boot**. It covers environment
variables, figment's config files and profiles, and file inputs: TLS key pairs, CA bundles, PKCS#12 keystores,
JSON/YAML/TOML config files, text and binary files.

Part of [docuconf](https://github.com/docuconf). See the
[specification](https://github.com/docuconf/docuconf-go/blob/main/spec/SPEC.md).

**Example:** [`examples/orders/`](https://github.com/docuconf/docuconf-rust/tree/main/examples/orders), a small HTTP service with its declaration, exported
contract and boot-time errors.

> **Status:** `0.1.0`. The contract format is a draft (`v1alpha1`) and the API may change.

## Why figment

figment is how Rust services already layer configuration (Rocket uses it): serde structs, file providers with
named profiles (`[default]`, `[production]`), and the environment on top. That maps directly onto the spec's
always-loaded base file, profile files and platform-supplied variables, so docuconf builds on it rather than on
`envy` (environment only) or `config` (which layers sources but has no named profiles).

## Declare

```rust
use std::time::Duration;
use docuconf::{ConfigFile, Docuconf, DocuconfEnum, Secret, TlsKeyPair};
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

    /// Certificate the service serves HTTPS with.
    #[docuconf(path = "/etc/billing/tls", dns_names("billing.internal"), min_remaining = "720h")]
    pub serving_tls: TlsKeyPair,

    /// Fee schedule, one entry per currency.
    #[docuconf(path = "/etc/billing/fees/fees.yaml", path_env = "FEES_FILE")]
    pub fees: ConfigFile<Fees>,
}

#[derive(Debug, Deserialize, DocuconfEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel { Debug, Info, Warn, Error }

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Fees {
    pub basis_points: std::collections::BTreeMap<String, u32>,
}
```

The `///` doc comment is the description (required, at least 5 characters; a trailing period is dropped). A field
that is not an `Option` and has no `default` is required. The Rust type picks the contract type:

| Field type | Contract |
|---|---|
| `String` | `string`; `url` with `schemes(...)` or `url`; `enum` with `values(...)` |
| `i8`..`i64`, `u8`..`u64`, `isize`, `usize` | `int`, with the type's range as `min`/`max` |
| `f32`, `f64` | `float` (NaN and infinity rejected) |
| `bool` | `bool` (`true`/`false`, any case) |
| `std::time::Duration` + `#[serde(with = "docuconf::humantime_serde")]` | `duration`, encoding `go` |
| `url::Url` | `url` |
| `#[derive(DocuconfEnum)]` enum | `enum`, values after serde renames |
| `Vec<String>`, `Vec<u16>`... | `list`, encoding `json`; an int item type narrower than 64 bits exports its range as `itemMin`/`itemMax` |
| `docuconf::Json<T>` (`T: JsonSchema`) | `json`, with the schema from `T` |
| `docuconf::Secret<T>` | `T` with `secret: true`; `Debug` prints `Secret(***)` |
| `Option<T>` | optional |
| nested `#[derive(Docuconf)]` struct | its variables, as `PARENT__CHILD` |
| `ConfigFile<T>` (`T: Deserialize + JsonSchema`) | file `config` (`format` from the extension or `format = "..."`) |
| `TlsKeyPair` | file `tls`: `dns_names`, `key_algorithms`, `min_remaining`, `require_ca` |
| `CaBundle` | file `caBundle`: `min_certificates` |
| `Keystore` | file `keystore` (PKCS#12): `password_var` names a secret variable |
| `TextFile` | file `text`: `pattern`, `min_length`, `max_length` |
| `BinaryFile` | file `binary` |

Variable attributes: `default`, `required`, `secret`, `min`, `max`, `min_length`, `max_length`, `pattern` (RE2,
matches anywhere: anchor with `^`/`$`), `values`, `schemes`, `min_items`, `max_items`, `item_min`, `item_max`, `group`, `examples`,
`deprecated`, `replaced_by`, `config_key`, `env`, `description`, `skip`. File attributes: `path` (required),
`name` (input name; default is the field name with `-`), `path_env`, `reload` (only `"restart"`; `"watch"` is not
implemented yet and is rejected), `max_size` (`65536` or `"64Ki"`), `required`, `secret`, `group`, `deprecated`,
plus the type-specific ones above.

`item_min` and `item_max` bound each item of an int list, and an item outside them is `out_of_range` at boot. They
are narrowed to the item type, as `min`/`max` are for an int variable, so `Vec<u16>` always exports
`itemMin: 0, itemMax: 65535` or tighter:

```rust
/// Shard ids this instance owns.
#[docuconf(item_min = 0, item_max = 1023)]
pub shards: Vec<u16>,
```

Mistakes (a bad name, a default outside its own range, a pattern with
lookaround, a file mounted over `/etc`) are reported by `docuconf::check_declaration::<Config>()`, by export and
by load.

### Names

Variable names follow figment's `Env::prefixed(prefix).split("__")`: the struct's
`#[docuconf(prefix = "APP_")]` (empty by default), then the serde key in upper case, with `__` between nesting
levels. `cache.ttl` under prefix `APP_` is `APP_CACHE__TTL`. `#[docuconf(env = "NAME")]` overrides one name.

### Wire formats

docuconf reads the declared variables itself and hands figment typed values. figment's own `Env` provider trims
values and guesses types (`8080` is a number even for a `String` field), which the spec forbids, so do not add
it alongside docuconf. Values are never trimmed; an empty value is unset for every type except `string`; lists
are JSON arrays (`["a","b"]`, which figment's `Env` also reads), so the contract says `encoding: "json"`;
durations are parsed with `humantime`, which reads Go syntax such as `1m30s`, so the contract says
`encoding: "go"`.

## Validate at boot

```rust
let config: Config = docuconf::load()?;
```

`load` reads the process environment and every file input, and fails with **all** violations, each with the
spec's stable code. Secret values never appear:

```text
docuconf: 3 configuration problems:
  DATABASE_URL: value has scheme mysql, not one of postgres, postgresql (invalid_scheme)
  PORT: "80x" is not a 64-bit integer (invalid_type)
  serving-tls: certificate expires at 2026-12-20T00:00:00Z, in 288h, less than minRemaining 720h (certificate_expiring)
```

The violations are also written to `/dev/termination-log` when it exists (or to `DOCUCONF_TERMINATION_LOG`), so
`kubectl describe pod` shows them. `DOCUCONF_FILE_ROOT` is prepended to every absolute file path, including paths
read from a `path_env` variable, for local development and tests.

File checks: the file exists, is readable and within `max_size`; config files parse (figment's JSON/YAML/TOML
parsers; a UTF-8 BOM is accepted), match the `schemars` schema (checked with `jsonschema`) and bind to `T`;
`tls` key pairs parse, the key matches the certificate (rustls), the certificate is valid with at least
`min_remaining` left, covers every `dns_names` entry (webpki, one-label wildcards), uses an allowed key algorithm
and, with `require_ca`, chains to `ca.crt` (webpki); CA bundles have `min_certificates` certificates; PKCS#12
keystores open with their password (`p12-keystore`); text files are UTF-8 and match their constraints.

### Injected secrets

Platforms often inject values when the container starts: Bank-Vaults' `vault-env` resolves `vault:` references,
`op run` resolves `op://` ones, operators add variables. docuconf reads the environment as the process sees it,
after injection, so injected values are validated like any other and docuconf never resolves a reference itself.
When the injector did not run, a secret variable still holds the raw reference; a value starting with `vault:`,
`op://` or `ref+` fails with `invalid_type`, naming the variable and the scheme but never the value:

```text
DATABASE_URL: holds an unresolved vault: reference; the injector that should resolve it did not run (invalid_type)
```

### Config files and profiles

Give the loader your figment file layers and the variable that selects the profile:

```rust
use docuconf::figment::{Figment, providers::{Format, Toml}};

let loader = docuconf::Loader::<Config>::new()
    .figment(Figment::from(Toml::file("App.toml").nested()))
    .profiles("APP_PROFILE", "production"); // APP_PROFILE must be a declared variable
let config = loader.load()?;
```

Layers, lowest first: the declaration's defaults, your figment (its `[default]` and `[global]` tables are
always loaded, a `[production]` table only when that profile is selected), then the environment, so the
platform's variables override file values. At export, values in the always-loaded tables become the variables'
defaults and other tables become the contract's `profiles.defaults`. A secret with a value in a config file is
an error. Keys that are not declared variables are file-only, and export warns (through `log`) that the platform
cannot set them. Fields you load from elsewhere (a vault, say) can be left out with `#[docuconf(skip)]`.

Other options: `.dotenv(".env")` reads a `.env` file for development (real variables win), `.env(map)` replaces
the process environment in tests, `.now(time)` fixes the clock for certificate checks.

### Config-file overlays

The platform can supply values in one more config file, mounted from a ConfigMap, instead of the environment
(spec §4.7). Declare it on the loader:

```rust
use docuconf::{Meta, Overlay};

fn loader() -> docuconf::Loader<Config> {
    docuconf::Loader::new()
        .figment(Figment::from(Toml::file("App.toml").nested()))
        .profiles("APP_PROFILE", "production")
        .overlay(Overlay::new("platform", "/etc/app/platform/app.toml")) // format from the extension
}

let config = loader().load()?;                          // at boot
let cue = loader().export(&Meta::new("billing-api"))?;  // the same loader for export
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

## Export

```rust
let cue = docuconf::export::<Config>(&docuconf::Meta::new("billing-api").app_version("1.4.0"))?;
std::fs::write("contract.cue", cue)?;
// or, with config files and profiles: loader.export(&meta)?
```

`cargo run --example export -- contract.cue` runs a complete example. The output is plain CUE data that unifies
with the meta-schema's `#Contract` (`generator.language: "rust"`), variables and files sorted by name, and is
deterministic. `tests/golden/gateway.cue` is a golden export that uses every variable type and every file type.

Feature-flag-like names (`FF_`, `FEATURE_`, `ENABLE_`) produce a warning: flags that change without a rollout
belong in a flag service (spec §10).

## Contract-first mode

`docuconf::Contract` validates an environment against a contract given as JSON (`cue export contract.cue`), with
no Rust declaration, and returns typed values. Use it for a contract written by hand in CUE, or to check an
environment in a tool. It parses every wire encoding of spec §5 (lists `csv` with any `separator`, `json` and
`indexed` as `NAME__0`, `NAME__1`..., numbered from 0 with no gap; durations `go`, `iso8601`, `seconds` and `timespan`) and runs the same checks
as a `#[derive(Docuconf)]` struct, so both accept exactly the same values:

```rust
let contract = docuconf::Contract::from_json(&std::fs::read_to_string("contract.json")?)?;
let values = contract.load()?; // the process environment; or load_env([("PORT", "9090")]) in tests
let port = values.get("PORT").and_then(|v| v.as_int());
```

`load()` reports every violation together and writes them to the termination log, as `docuconf::load` does;
`load_env(...)` takes the whole environment as a map and writes nothing. `json` variables are checked against
their `schema`. Profiles in the contract apply; file inputs and overlays are not loaded in this mode.

## Conformance

`tests/conformance.rs` runs docuconf-go's shared conformance suite (spec §12, `conformance/cases.json`) through
contract-first mode. It reads `$DOCUCONF_CONFORMANCE`, or `../docuconf-go/conformance/cases.json` next to this
repository, and is skipped when neither exists unless `DOCUCONF_REQUIRE_CONFORMANCE=1`:

```sh
DOCUCONF_CONFORMANCE=../docuconf-go/conformance/cases.json DOCUCONF_REQUIRE_CONFORMANCE=1 \
  cargo test --test conformance -- --nocapture
```

Failures are reported by case id. The SDK supports both capability tags, `int64` (Rust holds every 64-bit
integer) and `json-schema` (`json` values are checked with the `jsonschema` crate), so no case is skipped. CI runs
the suite against docuconf-go `main`.

## Not yet supported

- `reload: "watch"`, for file inputs and overlays (rejected at declaration time; files are read once at boot).
- JKS keystores (PKCS#12 only).
- Falling back from a variable to its `replaced_by` successor; deprecated variables only warn when set.
- Markdown docs generation (a SHOULD in the spec).

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
UPDATE_GOLDEN=1 cargo test --test export   # accept a changed golden export
```

See [Conformance](#conformance) for the shared suite.

The export tests run `cue vet -c` against the meta-schema when `cue` (v0.17.1) is installed and the spec is at
`../docuconf-go/spec/cue` or `$DOCUCONF_SPEC_CUE`; they skip otherwise (`DOCUCONF_REQUIRE_VET=1` makes that a
failure). Minimum supported Rust version: **1.89** (set by the `aes` crate under `p12-keystore`).

## Licence

MIT. See [LICENSE](LICENSE).
