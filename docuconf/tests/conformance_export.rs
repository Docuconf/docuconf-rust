//! The shared export fixture (SPEC §11.2 item 3, §12): docuconf-go's
//! `conformance/export/fixture.yaml`, declared with this SDK's own API,
//! exported, and compared with `conformance/export/golden.cue` by
//! `docuconf conformance export`.
//!
//! The golden contract comes from docuconf-go: `$DOCUCONF_GO_DIR`, or
//! `../docuconf-go` next to this repository. The CLI is `$DOCUCONF_CLI`, or
//! `docuconf` on `PATH`. The comparison is skipped when either is missing,
//! unless `DOCUCONF_REQUIRE_EXPORT=1`.
//!
//! One difference is expected, and only that one: the fixture declares
//! `reload: watch` on `settings` and `serving-tls`, and this SDK does not
//! reload files in place yet, so it rejects `watch` at declaration time
//! (SPEC §11.2 item 8) and declares them `restart`.
#![cfg(all(feature = "tls", feature = "keystore"))]

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use docuconf::{
    BinaryFile, CaBundle, ConfigFile, Docuconf, DocuconfEnum, Json, KeySet, Keystore, Meta, Secret,
    TextFile, TlsKeyPair,
};
use serde::Deserialize;

/// The differences the comparison may report: `reload: watch`, which this
/// SDK cannot declare yet.
const EXPECTED_DIFFS: &[&str] = &["files.serving-tls.reload", "files.settings.reload"];

/// Removes the keywords schemars adds that the fixture's schemas do not
/// have: `format` (`int64`, ...), and `default: null` on an optional
/// property.
fn no_format(schema: &mut docuconf::schemars::Schema) {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                m.remove("format");
                if m.get("default") == Some(&serde_json::Value::Null) {
                    m.remove("default");
                }
                m.values_mut().for_each(walk);
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut v = serde_json::Value::from(schema.clone());
    walk(&mut v);
    *schema = docuconf::schemars::Schema::try_from(v).expect("still a schema");
}

// The settings type of the fixture's config files.
#[derive(Deserialize, docuconf::JsonSchema)]
#[schemars(crate = "docuconf::schemars", transform = no_format)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Settings {
    #[schemars(length(min = 1))]
    name: String,
    #[schemars(range(min = 1))]
    replicas: i64,
    #[schemars(with = "Vec<String>")]
    #[serde(default)]
    tags: Option<Vec<String>>,
}

// The rate-limit object of `RATE_LIMITS`.
#[derive(Deserialize, docuconf::JsonSchema)]
#[schemars(crate = "docuconf::schemars", transform = no_format)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[allow(dead_code)]
struct RateLimits {
    #[schemars(range(min = 1))]
    per_minute: i64,
    #[schemars(range(min = 0), with = "i64")]
    #[serde(default)]
    burst: Option<i64>,
}

#[derive(Deserialize, DocuconfEnum)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Deserialize, Docuconf)]
#[allow(dead_code)]
struct Fixture {
    /// Service name, used in logs and metrics
    ///
    /// Lower case, as a DNS label allows.
    #[docuconf(
        default = "orders",
        min_length = 2,
        max_length = 40,
        pattern = "^[a-z][a-z0-9-]*$",
        group = "general",
        examples("orders", "billing"),
        config_key = "App:Name"
    )]
    app_name: String,

    /// Primary Postgres connection string
    #[docuconf(
        schemes("postgres", "postgresql"),
        max_length = 2048,
        group = "database"
    )]
    database_url: Secret<String>,

    /// HTTP listen port
    #[docuconf(default = 8080, min = 1, max = 65535)]
    port: i64,

    /// Fraction of requests traced
    #[docuconf(default = 0.25, min = 0, max = 1)]
    trace_ratio: f64,

    /// Serve the debug endpoints
    #[docuconf(default = false)]
    debug: bool,

    /// Upstream request timeout
    #[docuconf(default = "1m30s", min = "1s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    request_timeout: Duration,

    /// Minimum log level
    #[docuconf(default = "info")]
    log_level: LogLevel,

    /// CORS origins allowed to call the API
    #[docuconf(
        encoding = "csv",
        separator = ";",
        min_items = 1,
        max_items = 5,
        item_min_length = 1,
        item_max_length = 255
    )]
    allowed_origins: Option<Vec<String>>,

    /// Shards this instance owns
    #[docuconf(encoding = "csv", item_min = 0, item_max = 1023)]
    shards: Option<Vec<i64>>,

    /// Keys that verify webhook signatures
    #[docuconf(key_min_length = 32, key_max_length = 256)]
    webhook_keys: Option<KeySet>,

    /// Per-client rate limits
    #[docuconf(default = r#"{"perMinute":60}"#, max_length = 1024)]
    rate_limits: Json<RateLimits>,

    /// Port the service used to listen on
    #[docuconf(deprecated = "Use PORT instead", replaced_by = "PORT")]
    old_port: Option<i64>,

    /// Password of the partner keystore
    partner_password: Option<Secret<String>>,

    /// Application settings
    #[docuconf(
        path = "/etc/app/settings/settings.json",
        path_env = "SETTINGS_FILE",
        max_size = 65536,
        group = "general"
    )]
    settings: ConfigFile<Settings>,

    /// Routing rules
    #[docuconf(path = "/etc/app/rules/rules.yaml")]
    rules: Option<ConfigFile<Settings>>,

    /// Feature defaults
    #[docuconf(path = "/etc/app/flags/flags.toml")]
    flags: Option<ConfigFile<Settings>>,

    /// Certificate the service serves HTTPS with
    #[docuconf(
        path = "/etc/app/tls",
        dns_names("app.example.test", "api.example.test"),
        key_algorithms("ECDSA", "Ed25519"),
        min_remaining = "720h",
        require_ca
    )]
    serving_tls: Option<TlsKeyPair>,

    /// CAs the service trusts
    #[docuconf(path = "/etc/app/trust/bundle.pem", min_certificates = 2)]
    trust: Option<CaBundle>,

    /// Client certificate for the partner API
    #[docuconf(
        path = "/etc/app/partner/keystore.p12",
        password_var = "PARTNER_PASSWORD"
    )]
    partner: Option<Keystore>,

    /// Licence key
    #[docuconf(
        path = "/etc/app/licence/licence.key",
        min_length = 8,
        max_length = 64,
        pattern = "^[A-Z0-9-]+\\n?$"
    )]
    licence: Option<TextFile>,

    /// GeoIP database
    #[docuconf(
        path = "/data/geoip/geoip.mmdb",
        max_size = 134217728,
        deprecated = "Use geo-db instead",
        replaced_by = "geo-db"
    )]
    geoip: Option<BinaryFile>,

    /// City-level location database
    #[docuconf(name = "geo-db", path = "/data/geo-db/geo.mmdb")]
    geo_db: Option<BinaryFile>,
}

fn go_dir() -> PathBuf {
    match std::env::var_os("DOCUCONF_GO_DIR") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docuconf-go"),
    }
}

fn cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DOCUCONF_CLI").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("docuconf"))
        .find(|p| p.is_file())
}

#[test]
fn fixture_declares_and_exports() {
    let cue = docuconf::export::<Fixture>(
        &Meta::new("docuconf-fixture")
            .app_version("1.0.0")
            .package("docuconf_fixture"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(cue.contains("\"keySet\""), "{cue}");
}

#[test]
fn fixture_matches_the_golden_contract() {
    let required = std::env::var("DOCUCONF_REQUIRE_EXPORT").as_deref() == Ok("1");
    let golden = go_dir().join("conformance/export/golden.cue");
    let (Some(cli), true) = (cli(), golden.is_file()) else {
        assert!(
            !required,
            "the export check needs the docuconf CLI ($DOCUCONF_CLI or PATH) and {}",
            golden.display()
        );
        eprintln!("skipping the export check: no docuconf CLI or no golden contract");
        return;
    };
    let cue = docuconf::export::<Fixture>(
        &Meta::new("docuconf-fixture")
            .app_version("1.0.0")
            .package("docuconf_fixture"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let dir = tempfile::tempdir().unwrap();
    let exported = dir.path().join("exported.cue");
    std::fs::write(&exported, &cue).unwrap();
    let out = Command::new(&cli)
        .args(["conformance", "export", "--golden"])
        .arg(&golden)
        .arg(&exported)
        .output()
        .unwrap_or_else(|e| panic!("running {}: {e}", cli.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        return;
    }
    // Every reported difference must be one of the expected ones, and each
    // expected one must still be reported (so this list cannot go stale).
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let unexpected: Vec<&&str> = lines
        .iter()
        .filter(|l| !EXPECTED_DIFFS.iter().any(|d| l.contains(d)))
        .collect();
    assert!(
        unexpected.is_empty() && !lines.is_empty(),
        "the export of the shared fixture does not match golden.cue:\n{stdout}{stderr}\n{cue}"
    );
    for d in EXPECTED_DIFFS {
        assert!(
            lines.iter().any(|l| l.contains(d)),
            "{d} now matches: remove it from EXPECTED_DIFFS\n{stdout}"
        );
    }
}
