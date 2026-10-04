//! Writes the contract of a small service to `contract.cue` (or the path
//! given as the first argument), and shows how the same struct is loaded at
//! boot.
//!
//! ```sh
//! cargo run --example export -- contract.cue
//! ```

use std::time::Duration;

use docuconf::{ConfigFile, Docuconf, DocuconfEnum, Meta, Secret, TlsKeyPair};
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
    #[docuconf(default = "30s", min = "1s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    pub request_timeout: Duration,

    /// CORS origins allowed to call the API.
    #[docuconf(min_items = 1)]
    pub allowed_origins: Vec<String>,

    /// Certificate the service serves HTTPS with.
    #[docuconf(
        path = "/etc/billing/tls",
        dns_names("billing.internal"),
        key_algorithms("ECDSA", "RSA"),
        min_remaining = "720h"
    )]
    pub serving_tls: TlsKeyPair,

    /// Fee schedule, one entry per currency.
    #[docuconf(path = "/etc/billing/fees/fees.yaml", path_env = "FEES_FILE")]
    pub fees: ConfigFile<Fees>,
}

#[derive(Debug, Deserialize, DocuconfEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Fees charged per currency.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fees {
    /// Fee in basis points, keyed by ISO currency code.
    pub basis_points: std::collections::BTreeMap<String, u32>,
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "contract.cue".into());
    let meta = Meta::new("billing-api").app_version(env!("CARGO_PKG_VERSION"));
    match docuconf::export::<Config>(&meta) {
        Ok(cue) => {
            std::fs::write(&out, cue).expect("write contract");
            println!("wrote {out}");
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    // At boot the service would call:
    //     let config: Config = docuconf::load()?;
    // which reads the environment and files and fails with every violation.
}
