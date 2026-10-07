//! Boot-time loading of variables: parsing, constraints, secrets and the
//! all-violations-together report.

mod common;

use std::time::Duration;

use common::{Gateway, LogLevel, RateLimits, World};
use docuconf::{Code, Docuconf, Error};
use serde::Deserialize;

fn violations(w: &World) -> docuconf::ValidationError {
    match w.load() {
        Err(Error::Validation(v)) => v,
        Err(e) => panic!("expected violations, got {e}"),
        Ok(g) => panic!("expected violations, loaded {g:?}"),
    }
}

#[test]
fn loads_a_valid_environment() {
    let mut w = World::new();
    w.set("LOG_LEVEL", "warn")
        .set("PORT", "9090")
        .set("REQUEST_TIMEOUT", "1m30s")
        .set("EXTRA_PORTS", "[8443,9443]")
        .set("TRACE_SAMPLE_RATIO", "0.25")
        .set("DEBUG", "TRUE")
        .set("RATE_LIMITS", r#"{"perMinute":100,"burst":20}"#)
        .set("CACHE__TTL", "250ms")
        .set("MEMORY_LIMIT", "1073741824");
    let g = w.load().unwrap();
    assert_eq!(g.log_level, LogLevel::Warn);
    assert_eq!(g.port, 9090);
    assert_eq!(g.request_timeout, Duration::from_secs(90));
    assert_eq!(g.extra_ports, Some(vec![8443, 9443]));
    assert_eq!(g.trace_sample_ratio, 0.25);
    assert!(g.debug);
    assert_eq!(
        *g.rate_limits,
        RateLimits {
            per_minute: 100,
            burst: Some(20)
        }
    );
    assert_eq!(g.cache.ttl, Duration::from_millis(250));
    assert_eq!(g.cache.size, 1000);
    assert_eq!(g.mem_limit, Some(1 << 30));
    assert_eq!(g.allowed_origins.len(), 2);
    assert_eq!(
        g.database_url.expose(),
        "postgres://app:hunter2@db:5432/app"
    );
    assert_eq!(g.stripe_api_base.as_str(), "https://api.stripe.com/");
    assert_eq!(g.routes.routes[0].upstream, "https://api.internal");
    assert_eq!(g.license.text(), common::LICENSE);
    assert_eq!(g.serving_tls.cert_chain().len(), 1);
    assert!(g.partner_keystore.as_ref().unwrap().private_key().is_some());
    assert_eq!(g.upstream_ca.as_ref().unwrap().certificates().len(), 1);
    assert_eq!(g.geoip.as_ref().unwrap().bytes(), &[0, 1, 2, 3]);
}

#[test]
fn defaults_apply_when_unset() {
    let g = World::new().load().unwrap();
    assert_eq!(g.log_level, LogLevel::Info);
    assert_eq!(g.port, 8080);
    assert_eq!(g.request_timeout, Duration::from_secs(30));
    assert_eq!(g.rate_limits.per_minute, 60);
    assert_eq!(g.cache.ttl, Duration::from_secs(300));
    assert_eq!(g.extra_ports, None);
    assert_eq!(g.region, None);
}

#[test]
fn empty_is_unset_except_for_strings() {
    let mut w = World::new();
    w.set("PORT", "").set("DEBUG", "").set("POD_NAMESPACE", "");
    let g = w.load().unwrap();
    assert_eq!(g.port, 8080, "empty int takes its default");
    assert!(!g.debug);
    assert_eq!(g.pod_namespace, "", "empty string is a present value");

    // An empty required non-string is missing.
    let mut w = World::new();
    w.set("ALLOWED_ORIGINS", "");
    let v = violations(&w);
    assert_eq!(v.codes_for("ALLOWED_ORIGINS"), [Code::MissingRequired]);
}

#[test]
fn values_are_never_trimmed() {
    let mut w = World::new();
    w.set("POD_NAMESPACE", " edge\n").set("PORT", " 9090");
    let v = violations(&w);
    assert_eq!(v.codes_for("PORT"), [Code::InvalidType]);
    w.set("PORT", "9090");
    assert_eq!(w.load().unwrap().pod_namespace, " edge\n");
}

#[test]
fn a_value_that_is_not_utf8_is_invalid_type_not_missing() {
    use std::os::unix::ffi::OsStrExt;

    // Read from the process environment, where a value need not be UTF-8.
    // It used to be dropped, so a required variable looked unset.
    #[derive(Debug, Deserialize, Docuconf)]
    #[allow(dead_code)]
    struct Cfg {
        /// Port read from a non-UTF-8 value.
        docuconf_test_non_utf8_port: u16,
        /// Optional name read from a non-UTF-8 value.
        docuconf_test_non_utf8_name: Option<String>,
    }
    std::env::set_var(
        "DOCUCONF_TEST_NON_UTF8_PORT",
        std::ffi::OsStr::from_bytes(b"80\xff"),
    );
    std::env::set_var(
        "DOCUCONF_TEST_NON_UTF8_NAME",
        std::ffi::OsStr::from_bytes(b"caf\xe9"),
    );
    let e = docuconf::Loader::<Cfg>::new()
        .termination_log(false)
        .load()
        .unwrap_err();
    let v = &e.violations();
    assert_eq!(v.len(), 2, "{e}");
    for x in v.iter() {
        assert_eq!(x.code, Code::InvalidType, "{e}");
        assert!(x.message.contains("not valid UTF-8"), "{e}");
    }
    std::env::remove_var("DOCUCONF_TEST_NON_UTF8_PORT");
    std::env::remove_var("DOCUCONF_TEST_NON_UTF8_NAME");
}

#[test]
fn strings_that_look_like_numbers_stay_strings() {
    // figment's Env provider would read these as a number and a bool.
    let mut w = World::new();
    w.set("POD_NAMESPACE", "8080");
    assert_eq!(w.load().unwrap().pod_namespace, "8080");
    w.set("POD_NAMESPACE", "true");
    assert_eq!(w.load().unwrap().pod_namespace, "true");
    w.set("POD_NAMESPACE", "\"quoted\"");
    assert_eq!(w.load().unwrap().pod_namespace, "\"quoted\"");
}

#[test]
fn bad_int() {
    let mut w = World::new();
    w.set("PORT", "80x");
    let v = violations(&w);
    assert_eq!(v.violations.len(), 1, "{v}");
    assert_eq!(v.violations[0].code, Code::InvalidType);
    assert_eq!(v.violations[0].input, "PORT");
    assert!(v.violations[0].message.contains("\"80x\""), "{v}");

    // An integer outside the 64-bit range is out_of_range, not mistyped
    // (SPEC §5).
    w.set("PORT", "9223372036854775808");
    assert_eq!(violations(&w).codes_for("PORT"), [Code::OutOfRange]);
    w.set("PORT", "-99999999999999999999");
    assert_eq!(violations(&w).codes_for("PORT"), [Code::OutOfRange]);
    w.set("PORT", "--1");
    assert_eq!(violations(&w).codes_for("PORT"), [Code::InvalidType]);
    w.set("PORT", "1.5");
    assert_eq!(violations(&w).codes_for("PORT"), [Code::InvalidType]);
}

#[test]
fn out_of_range() {
    let mut w = World::new();
    w.set("PORT", "70000");
    let v = violations(&w);
    assert_eq!(v.codes_for("PORT"), [Code::OutOfRange]);
    assert!(v.to_string().contains("70000 is above max 65535"), "{v}");

    let mut w = World::new();
    w.set("PORT", "0")
        .set("REQUEST_TIMEOUT", "10m")
        .set("TRACE_SAMPLE_RATIO", "1.5");
    let v = violations(&w);
    assert_eq!(v.codes_for("PORT"), [Code::OutOfRange]);
    assert_eq!(v.codes_for("REQUEST_TIMEOUT"), [Code::OutOfRange]);
    assert_eq!(v.codes_for("TRACE_SAMPLE_RATIO"), [Code::OutOfRange]);
    assert!(v.to_string().contains("10m is above max 5m"), "{v}");
}

#[test]
fn list_items_outside_their_bounds() {
    let mut w = World::new();
    w.set("SHARDS", "[0,1024]").set("EXTRA_PORTS", "[8443,0]");
    let v = violations(&w);
    assert_eq!(v.codes_for("SHARDS"), [Code::OutOfRange]);
    assert_eq!(v.codes_for("EXTRA_PORTS"), [Code::OutOfRange]);
    let text = v.to_string();
    assert!(text.contains("has item 1024, above itemMax 1023"), "{text}");
    assert!(text.contains("has item 0, below itemMin 1"), "{text}");

    // Beyond the 64-bit range.
    let mut w = World::new();
    w.set("SHARDS", "[1,99999999999999999999]");
    assert_eq!(violations(&w).codes_for("SHARDS"), [Code::OutOfRange]);
    w.set("SHARDS", "[1,1.5]");
    assert_eq!(violations(&w).codes_for("SHARDS"), [Code::InvalidType]);

    // Beyond the item type (u16) without a user bound.
    let mut w = World::new();
    w.set("EXTRA_PORTS", "[70000]");
    assert_eq!(violations(&w).codes_for("EXTRA_PORTS"), [Code::OutOfRange]);

    let mut w = World::new();
    w.set("SHARDS", "[0,1023]");
    assert_eq!(w.load().unwrap().shards, Some(vec![0, 1023]));
}

#[test]
fn floats_reject_nan_and_infinity() {
    for bad in ["NaN", "inf", "-infinity"] {
        let mut w = World::new();
        w.set("TRACE_SAMPLE_RATIO", bad);
        assert_eq!(
            violations(&w).codes_for("TRACE_SAMPLE_RATIO"),
            [Code::InvalidType],
            "{bad}"
        );
    }
}

#[test]
fn bools_are_true_or_false() {
    let mut w = World::new();
    w.set("DEBUG", "False");
    assert!(!w.load().unwrap().debug);
    w.set("DEBUG", "yes");
    assert_eq!(violations(&w).codes_for("DEBUG"), [Code::InvalidType]);
}

#[test]
fn enum_pattern_scheme_and_lists() {
    let mut w = World::new();
    w.set("LOG_LEVEL", "verbose")
        .set("REGION", "Europe")
        .set("STRIPE_API_BASE", "http://api.stripe.com")
        .set("ALLOWED_ORIGINS", "[]")
        .set("EXTRA_PORTS", "[1,2,3,4,5]");
    let v = violations(&w);
    assert_eq!(v.codes_for("LOG_LEVEL"), [Code::NotInEnum]);
    assert_eq!(v.codes_for("REGION"), [Code::PatternMismatch]);
    assert_eq!(v.codes_for("STRIPE_API_BASE"), [Code::InvalidScheme]);
    assert_eq!(v.codes_for("ALLOWED_ORIGINS"), [Code::TooFewItems]);
    assert_eq!(v.codes_for("EXTRA_PORTS"), [Code::TooManyItems]);
}

#[test]
fn lists_are_json_arrays() {
    let mut w = World::new();
    w.set(
        "ALLOWED_ORIGINS",
        "https://a.example.com,https://b.example.com",
    );
    assert_eq!(
        violations(&w).codes_for("ALLOWED_ORIGINS"),
        [Code::InvalidType]
    );
    w.set("EXTRA_PORTS", r#"["8443"]"#).set(
        "ALLOWED_ORIGINS",
        r#"["https://a.example.com,with,commas"]"#,
    );
    let v = violations(&w);
    assert_eq!(v.codes_for("EXTRA_PORTS"), [Code::InvalidType]);
    assert!(v.codes_for("ALLOWED_ORIGINS").is_empty());
    w.set("EXTRA_PORTS", "[70000]");
    assert_eq!(violations(&w).codes_for("EXTRA_PORTS"), [Code::OutOfRange]);
}

#[test]
fn pattern_matches_anywhere() {
    #[derive(Debug, Deserialize, Docuconf)]
    struct P {
        /// A value containing a digit.
        #[docuconf(pattern = "[0-9]")]
        code: String,
    }
    let ok: P = docuconf::Loader::new()
        .env([("CODE", "abc1def")])
        .termination_log(false)
        .load()
        .unwrap();
    assert_eq!(ok.code, "abc1def");
    let err = docuconf::Loader::<P>::new()
        .env([("CODE", "abcdef")])
        .termination_log(false)
        .load()
        .unwrap_err();
    assert!(err.has(Code::PatternMismatch));
}

#[test]
fn json_variables_are_checked_against_their_schema() {
    let mut w = World::new();
    w.set("RATE_LIMITS", r#"{"perMinute":0}"#);
    assert_eq!(
        violations(&w).codes_for("RATE_LIMITS"),
        [Code::SchemaMismatch]
    );
    w.set("RATE_LIMITS", "{perMinute: 1}");
    assert_eq!(violations(&w).codes_for("RATE_LIMITS"), [Code::InvalidType]);
}

#[test]
fn missing_required_var() {
    let mut w = World::new();
    w.unset("POD_NAMESPACE").unset("DATABASE_URL");
    let v = violations(&w);
    assert_eq!(v.codes_for("POD_NAMESPACE"), [Code::MissingRequired]);
    assert_eq!(v.codes_for("DATABASE_URL"), [Code::MissingRequired]);
}

#[test]
fn secrets_are_never_printed() {
    let mut w = World::new();
    w.set("DATABASE_URL", "mysql://app:hunter2@db/app")
        .set("METRICS_TOKEN", "tok-hunter2\n")
        .set("PARTNER_KEYSTORE_PASSWORD", "hunter2-wrong");
    let v = violations(&w);
    let text = v.to_string();
    assert_eq!(v.codes_for("DATABASE_URL"), [Code::InvalidScheme]);
    assert_eq!(v.codes_for("METRICS_TOKEN"), [Code::OutOfRange]);
    assert_eq!(v.codes_for("partner-keystore"), [Code::KeystoreUnreadable]);
    assert!(!text.contains("hunter2"), "secret leaked: {text}");
    assert!(text.contains("ends in a newline"), "{text}");

    let g = World::new().load().unwrap();
    let debug = format!("{g:?}");
    assert!(
        !debug.contains("hunter2"),
        "secret leaked in Debug: {debug}"
    );
    assert!(!debug.contains("s3cret-pass"));
    assert!(!debug.contains("PRIVATE KEY"));
}

#[test]
fn reports_every_violation_together() {
    let mut w = World::new();
    w.unset("POD_NAMESPACE")
        .set("PORT", "abc")
        .set("LOG_LEVEL", "loud")
        .set("REQUEST_TIMEOUT", "forever")
        .set("ALLOWED_ORIGINS", "[]");
    w.remove("/etc/gateway/license/license.key");
    w.write("/etc/gateway/routes/routes.yaml", b"routes: [");
    let v = violations(&w);
    let mut got: Vec<(String, Code)> = v
        .violations
        .iter()
        .map(|x| (x.input.clone(), x.code))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("ALLOWED_ORIGINS".into(), Code::TooFewItems),
            ("LOG_LEVEL".into(), Code::NotInEnum),
            ("POD_NAMESPACE".into(), Code::MissingRequired),
            ("PORT".into(), Code::InvalidType),
            ("REQUEST_TIMEOUT".into(), Code::InvalidType),
            ("license".into(), Code::FileMissing),
            ("routes".into(), Code::FileMalformed),
        ],
        "{v}"
    );
    let text = v.to_string();
    assert!(
        text.starts_with("docuconf: 7 configuration problems:"),
        "{text}"
    );
    assert!(
        text.contains("PORT: \"abc\" is not a 64-bit integer (invalid_type)"),
        "{text}"
    );
}

#[test]
fn unknown_variables_are_ignored() {
    let mut w = World::new();
    w.set("HOSTNAME", "pod-1")
        .set("KUBERNETES_SERVICE_HOST", "10.0.0.1");
    w.load().unwrap();
}

#[test]
fn writes_the_termination_log() {
    let mut w = World::new();
    let log = w.dir.path().join("termination-log");
    w.set("DOCUCONF_TERMINATION_LOG", log.to_str().unwrap())
        .set("PORT", "x")
        .set("DATABASE_URL", "ftp://hunter2@x");
    let err = docuconf::Loader::<Gateway>::new()
        .env(w.env.clone())
        .now(common::now())
        .load()
        .unwrap_err();
    let written = std::fs::read_to_string(&log).unwrap();
    assert_eq!(written, format!("{err}\n"));
    assert!(written.contains("PORT"));
    assert!(!written.contains("hunter2"));
}

#[test]
fn unresolved_injector_references_are_reported_without_the_value() {
    let mut w = World::new();
    let log = w.dir.path().join("termination-log");
    w.set("DOCUCONF_TERMINATION_LOG", log.to_str().unwrap())
        .set("DATABASE_URL", "vault:secret/data/gateway/db#hunter2")
        .set("METRICS_TOKEN", "op://vault-hunter2/metrics/token")
        .set("PARTNER_KEYSTORE_PASSWORD", "ref+awsssm://hunter2/partner")
        // Not a secret: a value that happens to look like a reference is
        // just a value.
        .set("POD_NAMESPACE", "vault:edge");
    let err = docuconf::Loader::<Gateway>::new()
        .env(w.env.clone())
        .now(common::now())
        .load()
        .unwrap_err();
    let Error::Validation(v) = &err else {
        panic!("expected violations, got {err}")
    };
    assert_eq!(v.codes_for("DATABASE_URL"), [Code::InvalidType]);
    assert_eq!(v.codes_for("METRICS_TOKEN"), [Code::InvalidType]);
    assert_eq!(
        v.codes_for("PARTNER_KEYSTORE_PASSWORD"),
        [Code::InvalidType]
    );
    assert!(v.codes_for("POD_NAMESPACE").is_empty());
    let text = err.to_string();
    assert!(
        text.contains(
            "DATABASE_URL: holds an unresolved vault: reference; the injector that should resolve it did not run (invalid_type)"
        ),
        "{text}"
    );
    assert!(text.contains("METRICS_TOKEN: holds an unresolved op:// reference"));
    assert!(text.contains("PARTNER_KEYSTORE_PASSWORD: holds an unresolved ref+ reference"));
    let written = std::fs::read_to_string(&log).unwrap();
    for leaked in [text.as_str(), written.as_str()] {
        assert!(!leaked.contains("hunter2"), "value leaked: {leaked}");
        assert!(!leaked.contains("secret/data"), "value leaked: {leaked}");
    }
}

#[test]
fn dotenv_is_opt_in_and_the_environment_wins() {
    let w = World::new();
    let file = w.dir.path().join(".env");
    std::fs::write(&file, "PORT=7070\nPOD_NAMESPACE=from-dotenv\nDEBUG=true\n").unwrap();
    let g = w.loader::<Gateway>().dotenv(&file).load().unwrap();
    assert_eq!(g.port, 7070);
    assert_eq!(
        g.pod_namespace, "edge",
        "the real environment overrides .env"
    );
    assert!(g.debug);
    assert_eq!(w.load().unwrap().port, 8080, "no .env unless asked");
}

#[test]
fn prefix_and_nesting_follow_figment_env_rules() {
    #[derive(Debug, Deserialize, Docuconf)]
    #[docuconf(prefix = "APP_")]
    struct App {
        /// Listen port.
        #[docuconf(default = 80)]
        port: u16,
        db: Db,
    }
    #[derive(Debug, Deserialize, Docuconf)]
    struct Db {
        /// Pool size.
        #[docuconf(default = 4, min = 1)]
        pool_size: u32,
        /// Database host name.
        #[serde(rename = "hostname")]
        host: String,
    }
    let app: App = docuconf::Loader::new()
        .env([
            ("APP_PORT", "81"),
            ("APP_DB__POOL_SIZE", "8"),
            ("APP_DB__HOSTNAME", "db.internal"),
        ])
        .termination_log(false)
        .load()
        .unwrap();
    assert_eq!(app.port, 81);
    assert_eq!(app.db.pool_size, 8);
    assert_eq!(app.db.host, "db.internal");
    let cue = docuconf::export::<App>(&docuconf::Meta::new("app")).unwrap();
    for name in ["APP_PORT:", "APP_DB__POOL_SIZE:", "APP_DB__HOSTNAME:"] {
        assert!(cue.contains(name), "{name} missing from\n{cue}");
    }
}

#[test]
fn unsigned_numbers_from_json_and_yaml_files_bind() {
    use docuconf::figment::providers::{Format, Json, Yaml};
    use docuconf::figment::Figment;

    #[derive(Debug, Deserialize, Docuconf)]
    struct Pool {
        /// Number of pooled connections.
        #[docuconf(default = 4, min = 1)]
        size: u32,
        /// Fraction of connections kept warm.
        #[docuconf(default = 0.5)]
        warm: f64,
    }
    for fig in [
        Figment::from(Json::string(r#"{"size": 16, "warm": 1}"#)),
        Figment::from(Yaml::string("size: 16\nwarm: 1\n")),
    ] {
        let p = docuconf::Loader::<Pool>::new()
            .figment(fig)
            .env(Vec::<(String, String)>::new())
            .termination_log(false)
            .load()
            .unwrap();
        assert_eq!(p.size, 16);
        assert_eq!(p.warm, 1.0);
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct RunLimits {
    max: u64,
}

#[derive(Debug, Deserialize, Docuconf)]
#[allow(dead_code)]
struct Lengths {
    /// Where to report each run.
    #[docuconf(schemes = "https", max_length = 24)]
    callback: Option<String>,
    /// Database connection string.
    #[docuconf(max_length = 30)]
    db_url: Option<docuconf::Secret<docuconf::url::Url>>,
    /// Run limits as a JSON object.
    #[docuconf(max_length = 16)]
    limits: Option<docuconf::Json<serde_json::Value>>,
    /// Branch codes, two to four characters each.
    #[docuconf(item_min_length = 2, item_max_length = 4)]
    branches: Option<Vec<String>>,
}

fn load_lengths(env: &[(&str, &str)]) -> Result<Lengths, Error> {
    docuconf::Loader::<Lengths>::new()
        .env(env.iter().map(|(k, v)| (k.to_string(), v.to_string())))
        .termination_log(false)
        .load()
}

fn length_violations(env: &[(&str, &str)]) -> docuconf::ValidationError {
    match load_lengths(env) {
        Err(Error::Validation(v)) => v,
        Err(e) => panic!("expected violations, got {e}"),
        Ok(g) => panic!("expected violations, loaded {g:?}"),
    }
}

#[test]
fn url_max_length_counts_characters() {
    // 24 characters, exactly maxLength.
    let c = load_lengths(&[("CALLBACK", "https://a.example/runs/4")]).unwrap();
    assert_eq!(c.callback.as_deref(), Some("https://a.example/runs/4"));
    // 24 characters but 46 bytes.
    load_lengths(&[("CALLBACK", "https://例え.jp/日本語の道/一二三四")]).unwrap();

    for raw in [
        "https://a.example/runs/42",
        "https://例え.jp/日本語の道/一二三四五",
    ] {
        let v = length_violations(&[("CALLBACK", raw)]);
        assert_eq!(v.codes_for("CALLBACK"), [Code::OutOfRange], "{raw}");
        assert!(
            v.to_string()
                .contains("is 25 characters, above maxLength 24"),
            "{v}"
        );
    }

    // A secret reports its length, never its value.
    let v = length_violations(&[("DB_URL", "postgres://app:s3cr3t@db:5432/app")]);
    assert_eq!(v.codes_for("DB_URL"), [Code::OutOfRange]);
    let text = v.to_string();
    assert!(
        text.contains("value is 33 characters, above maxLength 30"),
        "{text}"
    );
    assert!(!text.contains("s3cr3t"), "{text}");
}

#[test]
fn json_max_length_measures_the_value_as_received() {
    load_lengths(&[("LIMITS", r#"{"max":12345678}"#)]).unwrap();
    // 16 characters, more bytes; an emoji is 1 code point (2 UTF-16 units).
    load_lengths(&[("LIMITS", r#"{"n":"日本語の道路xy"}"#)]).unwrap();
    load_lengths(&[("LIMITS", r#"{"n":"😀😀😀😀😀😀😀😀"}"#)]).unwrap();

    let v = length_violations(&[("LIMITS", r#"{"max":123456789}"#)]);
    assert_eq!(v.codes_for("LIMITS"), [Code::OutOfRange]);
    assert!(
        v.to_string()
            .contains("is 17 characters of JSON, above maxLength 16"),
        "{v}"
    );
    // Whitespace counts: the compact form would fit, the value as received
    // does not.
    let v = length_violations(&[("LIMITS", r#"{ "max": 123456 }"#)]);
    assert_eq!(v.codes_for("LIMITS"), [Code::OutOfRange]);
}

#[test]
fn json_max_length_from_a_config_file_measures_compact_json() {
    use docuconf::figment::providers::{Format, Toml};
    use docuconf::figment::Figment;

    let load = |toml: &str| {
        docuconf::Loader::<Lengths>::new()
            .figment(Figment::from(Toml::string(toml)))
            .env(Vec::<(String, String)>::new())
            .termination_log(false)
            .load()
    };
    // {"max":12345678} is 16 characters compact.
    let c = load("limits = { max = 12345678 }").unwrap();
    assert_eq!(c.limits.unwrap().0["max"], 12345678);
    match load("limits = { max = 123456789 }") {
        Err(Error::Validation(v)) => assert_eq!(v.codes_for("LIMITS"), [Code::OutOfRange]),
        other => panic!("expected out_of_range, got {other:?}"),
    }
}

#[test]
fn list_item_lengths_count_characters_after_splitting() {
    let c = load_lengths(&[("BRANCHES", r#"["BE","ZÜ01","GE02"]"#)]).unwrap();
    assert_eq!(c.branches.unwrap(), ["BE", "ZÜ01", "GE02"]);
    load_lengths(&[("BRANCHES", r#"["日本語x","😀😀"]"#)]).unwrap();

    let v = length_violations(&[("BRANCHES", r#"["BE","ZÜRICH"]"#)]);
    assert_eq!(v.codes_for("BRANCHES"), [Code::OutOfRange]);
    assert!(
        v.to_string()
            .contains("has item 1 \"ZÜRICH\" of 6 characters, above itemMaxLength 4"),
        "{v}"
    );
    let v = length_violations(&[("BRANCHES", r#"["BE","B"]"#)]);
    assert_eq!(v.codes_for("BRANCHES"), [Code::OutOfRange]);
    assert!(
        v.to_string()
            .contains("has item 1 \"B\" of 1 characters, below itemMinLength 2"),
        "{v}"
    );
}
