//! Contract-first mode: a contract given as JSON, no Rust declaration.

use std::time::Duration;

use docuconf::contract::Value;
use docuconf::{Code, Contract};
use serde_json::json;

fn contract(vars: serde_json::Value) -> Contract {
    Contract::from_value(&json!({
        "apiVersion": "docuconf.dev/v1alpha1",
        "kind": "ConfigContract",
        "metadata": {"name": "svc"},
        "vars": vars,
    }))
    .unwrap()
}

#[test]
fn parses_every_list_encoding() {
    let c = contract(json!({
        "CSV": {"type": "list", "description": "Semicolon list", "items": "int", "encoding": "csv", "separator": ";"},
        "JSON": {"type": "list", "description": "JSON list", "items": "string", "encoding": "json"},
        "IDX": {"type": "list", "description": "Indexed list", "items": "int", "encoding": "indexed", "itemMax": 9},
    }));
    let v = c
        .load_env([
            ("CSV", "1;2;3"),
            ("JSON", r#"["a,b","c"]"#),
            ("IDX__0", "7"),
            ("IDX__1", "8"),
            ("IDX__3", "ignored: not contiguous"),
        ])
        .unwrap();
    assert_eq!(v.to_json()["CSV"], json!([1, 2, 3]));
    assert_eq!(v.to_json()["JSON"], json!(["a,b", "c"]));
    assert_eq!(
        v.get("IDX").and_then(Value::as_list),
        Some(&[Value::Int(7), Value::Int(8)][..])
    );

    let e = c.load_env([("IDX__0", "10")]).unwrap_err();
    assert_eq!(e.codes_for("IDX"), [Code::OutOfRange]);
    let e = c.load_env([("CSV", "1;x")]).unwrap_err();
    assert_eq!(e.codes_for("CSV"), [Code::InvalidType]);
}

#[test]
fn parses_every_duration_encoding() {
    let c = contract(json!({
        "GO": {"type": "duration", "description": "Go form", "encoding": "go"},
        "ISO": {"type": "duration", "description": "ISO form", "encoding": "iso8601"},
        "SECS": {"type": "duration", "description": "Seconds", "encoding": "seconds", "max": "2m"},
        "SPAN": {"type": "duration", "description": "TimeSpan", "encoding": "timespan"},
    }));
    let v = c
        .load_env([
            ("GO", "1m30s"),
            ("ISO", "PT1.5S"),
            ("SECS", "0.25"),
            ("SPAN", "1.02:03:04.5"),
        ])
        .unwrap();
    let d = |n: &str| v.get(n).and_then(Value::as_duration).unwrap();
    assert_eq!(d("GO"), Duration::from_secs(90));
    assert_eq!(d("ISO"), Duration::from_millis(1500));
    assert_eq!(d("SECS"), Duration::from_millis(250));
    assert_eq!(v.to_json()["SPAN"], "26h3m4s500ms");

    let e = c.load_env([("SECS", "121")]).unwrap_err();
    assert_eq!(e.codes_for("SECS"), [Code::OutOfRange]);
    assert!(e.to_string().contains("is above max 2m"), "{e}");
}

#[test]
fn defaults_profiles_and_absent_optionals() {
    let c = Contract::from_value(&json!({
        "apiVersion": "docuconf.dev/v1alpha1",
        "kind": "ConfigContract",
        "metadata": {"name": "svc"},
        "vars": {
            "APP_ENV": {"type": "string", "description": "Profile selector", "default": "production"},
            "API": {"type": "url", "description": "Upstream API", "required": true},
            "PAGE": {"type": "int", "description": "Page size", "default": 20},
            "REGION": {"type": "enum", "description": "Cloud region", "values": ["eu", "us"]},
        },
        "profiles": {
            "selector": "APP_ENV",
            "default": "production",
            "defaults": {"production": {"API": "https://api.example.com"}, "staging": {"PAGE": 5}},
        },
    }))
    .unwrap();
    let v = c.load_env(Vec::<(String, String)>::new()).unwrap();
    assert_eq!(
        v.get("API").and_then(Value::as_str),
        Some("https://api.example.com")
    );
    assert_eq!(v.get("PAGE").and_then(Value::as_int), Some(20));
    assert_eq!(v.to_json()["REGION"], serde_json::Value::Null);

    // The staging profile has no API, so the platform must set it.
    let e = c.load_env([("APP_ENV", "staging")]).unwrap_err();
    assert_eq!(e.codes_for("API"), [Code::MissingRequired]);
    let v = c
        .load_env([("APP_ENV", "staging"), ("API", "https://x.internal")])
        .unwrap();
    assert_eq!(v.get("PAGE").and_then(Value::as_int), Some(5));
}

#[test]
fn secrets_are_hidden() {
    let c = contract(json!({
        "TOKEN": {"type": "string", "description": "API token", "secret": true, "minLength": 20},
    }));
    let e = c.load_env([("TOKEN", "hunter2")]).unwrap_err();
    assert_eq!(e.codes_for("TOKEN"), [Code::OutOfRange]);
    assert!(!e.to_string().contains("hunter2"), "{e}");
    let v = c.load_env([("TOKEN", "hunter2-hunter2-hunter2")]).unwrap();
    assert!(v.is_secret("TOKEN"));
    assert!(!format!("{v:?}").contains("hunter2"));
}

#[test]
fn rejects_a_broken_contract() {
    let e = Contract::from_value(&json!({
        "apiVersion": "docuconf.dev/v1alpha1",
        "kind": "ConfigContract",
        "metadata": {"name": "svc"},
        "vars": {
            "lower": {"type": "string", "description": "Bad name"},
            "PORT": {"type": "int", "description": "Port", "default": 0, "min": 1},
            "NAMES": {"type": "list", "description": "Names", "items": "string", "itemMin": 1},
            "WHEN": {"type": "duration", "description": "When", "encoding": "fortnights"},
            "LOOK": {"type": "string", "description": "Lookahead", "pattern": "(?=x)"},
        },
    }))
    .unwrap_err();
    let all = e.problems.join("\n");
    for want in [
        "lower: variable name must match",
        "PORT: default 0 is below min 1",
        "NAMES: itemMin and itemMax only apply to a list of ints",
        "WHEN: unknown duration encoding \"fortnights\"",
        "LOOK: pattern \"(?=x)\" is not valid RE2",
    ] {
        assert!(all.contains(want), "missing {want:?} in\n{all}");
    }
    assert!(Contract::from_json("{").is_err());
    assert!(Contract::from_json(r#"{"kind": "Other"}"#).is_err());
}
