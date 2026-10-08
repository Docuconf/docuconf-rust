//! What a first-time user meets: error reports, warnings, profiles, the
//! export helpers and secret redaction. Uses only environment variables,
//! so it also runs with `--no-default-features`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use docuconf::figment::providers::{Format, Toml};
use docuconf::figment::Figment;
use docuconf::{Code, Docuconf, DocuconfEnum, Error, Loader, Meta, Secret};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Docuconf)]
#[allow(dead_code)]
struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    port: u16,

    /// Minimum log level emitted.
    #[docuconf(default = "info")]
    log_level: Level,

    /// Primary Postgres connection string.
    #[docuconf(schemes("postgres"))]
    database_url: Secret<String>,

    /// Upstream request timeout.
    #[docuconf(default = "30s")]
    #[serde(with = "docuconf::humantime_serde")]
    request_timeout: Duration,

    /// Optional connect timeout.
    #[serde(default, with = "docuconf::humantime_serde::option")]
    connect_timeout: Option<Duration>,

    /// Browser origins allowed to call the API.
    #[docuconf(default = ["http://localhost:3000"])]
    allowed_origins: Vec<String>,

    /// Old name of the listen port.
    #[docuconf(deprecated = "use PORT")]
    legacy_port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, DocuconfEnum, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Level {
    Debug,
    Info,
}

const DB: (&str, &str) = ("DATABASE_URL", "postgres://app:hunter2@db/app");

/// A loader on `vars` that collects its warnings.
fn loader(vars: &[(&str, &str)]) -> (Loader<Config>, Arc<Mutex<Vec<String>>>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&warnings);
    let l = Loader::<Config>::new()
        .env(vars.iter().copied())
        .on_warning(move |w| sink.lock().unwrap().push(w.to_string()));
    (l, warnings)
}

fn violations(e: Error) -> Vec<(String, Code)> {
    e.violations()
        .iter()
        .map(|v| (v.input.clone(), v.code))
        .collect()
}

#[test]
fn debug_prints_the_report_and_there_is_no_source() {
    let err = Loader::<Config>::new()
        .env([("PORT", "80x")])
        .load()
        .unwrap_err();
    let report = err.to_string();
    assert_eq!(format!("{err:?}"), report, "Debug must be the report");
    assert!(
        report.starts_with("docuconf: 2 configuration problems:"),
        "{report}"
    );
    assert!(std::error::Error::source(&err).is_none());
    // `?` into Box<dyn Error> prints the same, once.
    let boxed: Box<dyn std::error::Error> = Box::new(err);
    assert_eq!(format!("{boxed:?}"), report);
}

#[test]
fn integers_are_not_called_64_bit() {
    let err = Loader::<Config>::new()
        .env([DB, ("PORT", "80x")])
        .load()
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("PORT: \"80x\" is not an integer (invalid_type)"),
        "{err}"
    );
}

#[test]
fn violations_are_sorted_by_name_whatever_found_them() {
    // ALLOWED_ORIGINS and REQUEST_TIMEOUT fail to parse, DATABASE_URL is
    // missing, PORT is out of range: one alphabetical report.
    let err = Loader::<Config>::new()
        .env([
            ("PORT", "0"),
            ("REQUEST_TIMEOUT", "soon"),
            ("ALLOWED_ORIGINS", "x"),
        ])
        .load()
        .unwrap_err();
    let names: Vec<String> = violations(err).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        ["ALLOWED_ORIGINS", "DATABASE_URL", "PORT", "REQUEST_TIMEOUT"]
    );
}

#[test]
fn durations_follow_go_not_humantime() {
    for (value, ok) in [
        ("1m30s", true),
        ("1.5h", true),
        ("2d", false),
        ("1 hour", false),
    ] {
        let r = Loader::<Config>::new()
            .env([DB, ("REQUEST_TIMEOUT", value)])
            .load();
        assert_eq!(r.is_ok(), ok, "{value}: {r:?}");
        if let Err(e) = r {
            assert!(e.to_string().contains("ns, us, ms, s, m or h"), "{e}");
        }
    }
    let c = Loader::<Config>::new()
        .env([DB, ("CONNECT_TIMEOUT", "250ms")])
        .load()
        .unwrap();
    assert_eq!(c.connect_timeout, Some(Duration::from_millis(250)));
    let c = Loader::<Config>::new().env([DB]).load().unwrap();
    assert_eq!(c.connect_timeout, None);
}

#[test]
fn typo_hints_name_both_and_never_the_value() {
    let (l, warnings) = loader(&[
        ("DATABSE_URL", "postgres://app:hunter2@db/app"),
        ("PORTS", "9090"),
        // Too far from anything declared, or a system variable.
        ("HOST", "x"),
        ("HOME", "/root"),
        ("UNRELATED_THING", "x"),
    ]);
    let err = l.load().unwrap_err();
    assert_eq!(
        violations(err),
        [("DATABASE_URL".to_string(), Code::MissingRequired)],
        "a typo is a warning, not a violation"
    );
    let w = warnings.lock().unwrap().clone();
    assert_eq!(
        w,
        [
            "DATABSE_URL is set but not declared; did you mean DATABASE_URL?",
            "PORTS is set but not declared; did you mean PORT?",
        ]
    );
    assert!(!w.concat().contains("hunter2"));
}

#[test]
fn deprecated_variables_warn_through_the_sink() {
    let (l, warnings) = loader(&[DB, ("LEGACY_PORT", "81")]);
    l.load().unwrap();
    assert_eq!(
        warnings.lock().unwrap().clone(),
        ["LEGACY_PORT is deprecated: use PORT"]
    );
}

#[test]
fn secrets_never_print() {
    let c = Loader::<Config>::new().env([DB]).load().unwrap();
    assert!(!format!("{c:?}").contains("hunter2"));
    assert_eq!(serde_json::to_string(&c.database_url).unwrap(), "\"***\"");
    let err = Loader::<Config>::new()
        .env([("DATABASE_URL", "mysql://app:hunter2@db/app")])
        .load()
        .unwrap_err();
    for shown in [err.to_string(), format!("{err:?}")] {
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("DATABASE_URL"), "{shown}");
    }
}

// ---------------------------------------------------------------------------
// Profiles

#[derive(Debug, Deserialize, Docuconf)]
#[allow(dead_code)]
struct Profiled {
    /// HTTP listen port.
    #[docuconf(default = 8080)]
    port: u16,
    /// Selects the config-file profile.
    app_profile: Option<String>,
}

#[derive(Debug, Deserialize, Docuconf)]
#[allow(dead_code)]
struct NoSelector {
    /// HTTP listen port.
    #[docuconf(default = 8080)]
    port: u16,
}

const APP_TOML: &str = "[default]\nport = 9000\n[production]\nport = 9443\n[canary]\nport = 9444\n";

#[test]
fn an_unknown_profile_warns() {
    let l = |profile: &str| {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&warnings);
        let port = Loader::<Profiled>::new()
            .env([("APP_PROFILE", profile)])
            .figment(Figment::from(Toml::string(APP_TOML).nested()))
            .profiles("APP_PROFILE", "production")
            .on_warning(move |w| sink.lock().unwrap().push(w.to_string()))
            .load()
            .unwrap()
            .port;
        let w = warnings.lock().unwrap().clone();
        (port, w)
    };
    assert_eq!(l("canary"), (9444, vec![]));
    assert_eq!(l("production"), (9443, vec![]));
    // Allowed by the spec (the platform may supply the values), but not
    // silent: usually a typo.
    let (port, w) = l("staging");
    assert_eq!(port, 9000);
    assert_eq!(
        w,
        ["APP_PROFILE selects profile \"staging\", which no config file defines, so only base values and the environment apply; the config files define canary, production"]
    );
}

#[test]
fn an_undeclared_selector_fails_at_boot_as_at_export() {
    let l = Loader::<NoSelector>::new()
        .env([("APP_PROFILE", "production")])
        .figment(Figment::from(Toml::string(APP_TOML).nested()))
        .profiles("APP_PROFILE", "production");
    let boot = l.load().unwrap_err();
    assert!(matches!(boot, Error::Declaration(_)), "{boot}");
    let export = l.export(&Meta::new("svc")).unwrap_err();
    for e in [boot.to_string(), export.to_string()] {
        assert!(
            e.contains("profile selector APP_PROFILE must be a declared variable"),
            "{e}"
        );
    }
}

// ---------------------------------------------------------------------------
// Export helpers

#[derive(Debug, Deserialize, Docuconf)]
#[allow(dead_code)]
struct Flagged {
    /// Turns on the new checkout.
    #[docuconf(default = false)]
    enable_checkout: bool,
}

#[test]
fn export_returns_its_warnings_and_meta_has_a_package_builder() {
    let out = Loader::<Flagged>::new()
        .export_with_warnings(&Meta::new("shop-api").package("shop"))
        .unwrap();
    assert!(out.cue.contains("package shop\n"), "{}", out.cue);
    assert_eq!(out.warnings.len(), 1);
    assert!(out.warnings[0].starts_with("ENABLE_CHECKOUT: looks like a feature flag"));
}

#[test]
fn assert_contract_accepts_a_current_file_and_rejects_a_stale_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("contract.cue");
    let meta = Meta::new("shop-api");
    std::fs::write(&path, docuconf::export::<Flagged>(&meta).unwrap()).unwrap();
    docuconf::assert_contract::<Flagged>(&meta, &path);

    std::fs::write(&path, "// stale\n").unwrap();
    let panic = std::panic::catch_unwind(|| docuconf::assert_contract::<Flagged>(&meta, &path))
        .unwrap_err();
    let msg = panic.downcast_ref::<String>().unwrap();
    assert!(
        msg.contains("is out of date; first difference at line 1"),
        "{msg}"
    );
    assert!(msg.contains("UPDATE_CONTRACT=1"), "{msg}");
}

// ---------------------------------------------------------------------------
// secrecy

#[cfg(feature = "secrecy")]
#[test]
fn secrecy_types_declare_secret_variables() {
    use secrecy::ExposeSecret;

    #[derive(Debug, Deserialize, Docuconf)]
    #[allow(dead_code)]
    struct WithSecrecy {
        /// API token for the payment provider.
        api_token: secrecy::SecretString,
        /// Optional webhook signing key.
        webhook_key: Option<secrecy::SecretString>,
    }
    let cue = docuconf::export::<WithSecrecy>(&Meta::new("pay")).unwrap();
    assert!(cue.contains("API_TOKEN: {"), "{cue}");
    let secrets = cue
        .lines()
        .filter(|l| l.trim_start().starts_with("secret:") && l.trim_end().ends_with("true"))
        .count();
    assert_eq!(secrets, 2, "{cue}");
    let c = Loader::<WithSecrecy>::new()
        .env([("API_TOKEN", "tok-hunter2")])
        .load()
        .unwrap();
    assert_eq!(c.api_token.expose_secret(), "tok-hunter2");
    assert!(!format!("{c:?}").contains("hunter2"));
}

#[test]
fn undeclared_variables_under_the_prefix_warn() {
    #[derive(Debug, Deserialize, Docuconf)]
    #[docuconf(prefix = "APP_")]
    #[allow(dead_code)]
    struct Prefixed {
        /// Cache entry lifetime in seconds.
        #[docuconf(default = 60)]
        cache_ttl: u32,
    }
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&warnings);
    Loader::<Prefixed>::new()
        .env([("APP_CACHE_TL", "5"), ("APP_COLOUR", "red"), ("OTHER", "x")])
        .on_warning(move |w| sink.lock().unwrap().push(w.to_string()))
        .load()
        .unwrap();
    assert_eq!(
        warnings.lock().unwrap().clone(),
        [
            "APP_CACHE_TL is set but not declared; did you mean APP_CACHE_TTL?",
            "APP_COLOUR is set but not declared (no variable under prefix APP_ has that name)",
        ]
    );
}
