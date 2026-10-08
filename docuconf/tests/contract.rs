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
            ("IDX__HOST", "not an item"),
            ("IDX__01", "not an item either"),
        ])
        .unwrap();
    assert_eq!(v.to_json()["CSV"], json!([1, 2, 3]));
    assert_eq!(v.to_json()["JSON"], json!(["a,b", "c"]));
    assert_eq!(
        v.get("IDX").and_then(Value::as_list),
        Some(&[Value::Int(7), Value::Int(8)][..])
    );

    // Items are numbered from 0 with no gap (SPEC §5).
    for env in [
        &[("IDX__0", "1"), ("IDX__2", "3")][..],
        &[("IDX__1", "2")],
        &[("IDX__0", "1"), ("IDX__99999999999999999999999", "2")],
    ] {
        let e = c.load_env(env.iter().copied()).unwrap_err();
        assert_eq!(e.codes_for("IDX"), [Code::InvalidType], "{env:?}");
    }
    let e = c.load_env([("IDX__0", "1"), ("IDX__2", "3")]).unwrap_err();
    assert!(e.to_string().contains("IDX__1 is not set"), "{e}");

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
fn integers_beyond_64_bits_are_out_of_range() {
    let c = contract(json!({
        "N": {"type": "int", "description": "A number"},
        "L": {"type": "list", "description": "Numbers", "items": "int"},
    }));
    let e = c
        .load_env([
            ("N", "99999999999999999999"),
            ("L", "1,-9223372036854775809"),
        ])
        .unwrap_err();
    assert_eq!(e.codes_for("N"), [Code::OutOfRange]);
    assert_eq!(e.codes_for("L"), [Code::OutOfRange]);
    let e = c.load_env([("N", "1e3"), ("L", "1,2.0")]).unwrap_err();
    assert_eq!(e.codes_for("N"), [Code::InvalidType]);
    assert_eq!(e.codes_for("L"), [Code::InvalidType]);
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

#[test]
fn length_limits_on_urls_json_and_list_items() {
    let c = contract(json!({
        "CALLBACK": {"type": "url", "description": "Callback URL", "schemes": ["https"], "maxLength": 24},
        "LIMITS": {"type": "json", "description": "Run limits", "maxLength": 16},
        "BRANCHES": {"type": "list", "description": "Branch codes", "items": "string", "itemMinLength": 2, "itemMaxLength": 4},
        "CODES": {"type": "list", "description": "Codes as JSON", "items": "string", "encoding": "json", "itemMaxLength": 4},
        "TOKEN": {"type": "url", "description": "Secret URL", "secret": true, "maxLength": 10},
    }));
    let v = c
        .load_env([
            ("CALLBACK", "https://例え.jp/日本語の道/一二三四"),
            ("LIMITS", r#"{"n":"日本語の道路xy"}"#),
            ("BRANCHES", "BE,ZÜ01,GE02"),
            ("CODES", r#"["😀😀😀😀"]"#),
        ])
        .unwrap();
    assert_eq!(v.to_json()["BRANCHES"], json!(["BE", "ZÜ01", "GE02"]));
    assert_eq!(v.to_json()["CODES"], json!(["😀😀😀😀"]));

    let e = c
        .load_env([
            ("CALLBACK", "https://a.example/runs/42"),
            ("LIMITS", r#"{ "max": 123456 }"#),
            ("BRANCHES", "BE,ZÜRICH"),
            ("CODES", r#"["BE","GENEVA"]"#),
            ("TOKEN", "https://user:hunter2@x"),
        ])
        .unwrap_err();
    for var in ["CALLBACK", "LIMITS", "BRANCHES", "CODES", "TOKEN"] {
        assert_eq!(e.codes_for(var), [Code::OutOfRange], "{var}: {e}");
    }
    assert!(!e.to_string().contains("hunter2"), "{e}");
    // Separators are not counted.
    assert_eq!(
        c.load_env([("BRANCHES", "B")])
            .unwrap_err()
            .codes_for("BRANCHES"),
        [Code::OutOfRange]
    );
}

#[test]
fn rejects_item_lengths_on_int_lists_and_bad_length_defaults() {
    let e = Contract::from_value(&json!({
        "apiVersion": "docuconf.dev/v1alpha1",
        "kind": "ConfigContract",
        "metadata": {"name": "svc"},
        "vars": {
            "PORTS": {"type": "list", "description": "Ports", "items": "int", "itemMaxLength": 5},
            "CODES": {"type": "list", "description": "Codes", "items": "string", "itemMinLength": 5, "itemMaxLength": 4},
            "SITE": {"type": "url", "description": "Site", "maxLength": 10, "default": "https://example.com"},
            "LIMITS": {"type": "json", "description": "Limits", "maxLength": 9, "default": {"a": "<&>"}},
            "BRANCHES": {"type": "list", "description": "Branches", "items": "string", "itemMaxLength": 4, "default": ["ZÜRICH"]},
        },
    }))
    .unwrap_err();
    let all = e.problems.join("\n");
    for want in [
        "PORTS: itemMinLength and itemMaxLength only apply to a list of strings",
        "CODES: itemMinLength 5 is above itemMaxLength 4",
        "SITE: default \"https://example.com\" is 19 characters, above maxLength 10",
        // {"a":"<&>"} is 11 characters: no HTML escaping.
        "LIMITS: default {\"a\":\"<&>\"} is 11 characters of JSON, above maxLength 9",
        "BRANCHES: default [\"ZÜRICH\"] has item 0 \"ZÜRICH\" of 6 characters, above itemMaxLength 4",
    ] {
        assert!(all.contains(want), "missing {want:?} in\n{all}");
    }
    // At the limit, without HTML escaping, the default is fine.
    contract(
        json!({"LIMITS": {"type": "json", "description": "Limits", "maxLength": 11, "default": {"a": "<&>"}}}),
    );
}

#[test]
fn details_load_and_are_checked() {
    // Docs only: a contract with details loads, and they change nothing.
    let c = contract(json!({
        "PORT": {"type": "int", "description": "HTTP listen port", "details": "Behind the mesh, keep the **default**.\n\n- one\n- two", "default": 8080},
        "LIMIT": {"type": "int", "description": "Exactly at the limit", "details": "日本".repeat(2000)},
    }));
    let v = c.load_env([("LIMIT", "1")]).unwrap();
    assert_eq!(v.get("PORT").and_then(Value::as_int), Some(8080));

    for (details, want) in [
        (json!(" \n\t"), "PORT: details must not be blank"),
        (
            json!("日本".repeat(2000) + "日"),
            "PORT: details are 4001 characters",
        ),
        (json!(42), "PORT: details must be a string"),
    ] {
        let e = Contract::from_value(&json!({
            "apiVersion": "docuconf.dev/v1alpha1",
            "kind": "ConfigContract",
            "metadata": {"name": "svc"},
            "vars": {"PORT": {"type": "int", "description": "HTTP listen port", "details": details}},
        }))
        .unwrap_err();
        assert!(e.to_string().contains(want), "{e}");
    }
}
