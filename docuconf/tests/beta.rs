//! The beta features in declaration mode and contract-first mode: the
//! `keySet` type, the `deprecated` rules, and strict wire parsing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use docuconf::{Code, Contract, Docuconf, Error, KeySet, Loader, Meta};
use serde::Deserialize;

const OLD: &str = "old-webhook-key-0123456789abcdef0123";
const NEW: &str = "new-webhook-key-0123456789abcdef0123";

#[derive(Deserialize, Docuconf, Debug)]
struct Webhooks {
    /// Keys that verify the signature on incoming webhooks.
    #[docuconf(key_min_length = 32, key_max_length = 256)]
    webhook_keys: KeySet,

    /// Keys callers present, as a JSON array.
    #[docuconf(encoding = "json", max_keys = 3)]
    api_keys: Option<KeySet>,
}

fn load<C: Docuconf + serde::de::DeserializeOwned>(env: &[(&str, &str)]) -> Result<C, Error> {
    Loader::<C>::new()
        .env(env.iter().copied())
        .on_warning(|_| {})
        .load()
}

fn codes<C: Docuconf + serde::de::DeserializeOwned + std::fmt::Debug>(
    env: &[(&str, &str)],
    input: &str,
) -> (Vec<Code>, String) {
    let e = load::<C>(env).unwrap_err();
    (
        e.violations()
            .iter()
            .filter(|v| v.input == input)
            .map(|v| v.code)
            .collect(),
        e.to_string(),
    )
}

#[test]
fn key_set_loads_in_order() {
    let both = format!("{OLD},{NEW}");
    let w: Webhooks = load(&[("WEBHOOK_KEYS", &both), ("API_KEYS", r#"["a","b,c"]"#)]).unwrap();
    assert_eq!(w.webhook_keys.keys(), [OLD, NEW]);
    assert!(w.webhook_keys.contains(NEW));
    assert!(w.webhook_keys.contains(OLD.as_bytes()));
    assert!(!w.webhook_keys.contains(&NEW[..NEW.len() - 1]));
    assert!(!w.webhook_keys.contains(""));
    assert_eq!(w.api_keys.unwrap().keys(), ["a", "b,c"]);

    // verify tries every key, even after a match.
    let mut tried = 0;
    assert!(w.webhook_keys.verify(|k| {
        tried += 1;
        k == OLD.as_bytes()
    }));
    assert_eq!(tried, 2);
    assert!(!w.webhook_keys.verify(|_| false));
}

#[test]
fn key_set_is_redacted() {
    let both = format!("{OLD},{NEW}");
    let w: Webhooks = load(&[("WEBHOOK_KEYS", &both)]).unwrap();
    let shown = format!(
        "{w:?} {} {}",
        w.webhook_keys,
        serde_json::to_string(&w.webhook_keys).unwrap()
    );
    assert!(!shown.contains(OLD) && !shown.contains(NEW), "{shown}");
    assert!(shown.contains("KeySet(***)"), "{shown}");
}

#[test]
fn key_set_errors_never_show_a_key() {
    for (raw, code) in [
        (format!("{OLD},"), Code::OutOfRange),
        (format!("{OLD},new-webhook-key"), Code::OutOfRange),
        (format!("{OLD},{NEW},{NEW}"), Code::TooManyItems),
        (format!("{}x", "k".repeat(256)), Code::OutOfRange),
    ] {
        let (c, text) = codes::<Webhooks>(&[("WEBHOOK_KEYS", &raw)], "WEBHOOK_KEYS");
        assert_eq!(c, [code], "{raw}: {text}");
        assert!(
            !text.contains(OLD) && !text.contains("new-webhook-key"),
            "{text}"
        );
    }
    let (c, _) = codes::<Webhooks>(&[("WEBHOOK_KEYS", OLD), ("API_KEYS", "[]")], "API_KEYS");
    assert_eq!(c, [Code::TooFewItems]);
    let (c, _) = codes::<Webhooks>(
        &[("WEBHOOK_KEYS", OLD), ("API_KEYS", r#"["a",""]"#)],
        "API_KEYS",
    );
    assert_eq!(c, [Code::OutOfRange]);
    let (c, _) = codes::<Webhooks>(&[], "WEBHOOK_KEYS");
    assert_eq!(c, [Code::MissingRequired]);
}

#[test]
fn key_set_exports_as_a_secret_key_set() {
    let cue = docuconf::export::<Webhooks>(&Meta::new("webhooks")).unwrap();
    for want in [
        "type:         \"keySet\"",
        "secret:       true",
        "encoding:     \"csv\"",
        "separator:    \",\"",
        "minKeys:      1",
        "maxKeys:      2",
        "keyMinLength: 32",
        "keyMaxLength: 256",
        "maxKeys:     3",
    ] {
        assert!(cue.contains(want), "missing {want}:\n{cue}");
    }
}

#[derive(Deserialize, Docuconf, Debug)]
#[allow(dead_code)]
struct BadKeys {
    /// Keys with impossible bounds.
    #[docuconf(min_keys = 0, key_min_length = 0)]
    keys: KeySet,
}

#[test]
fn key_set_bounds_are_checked() {
    let e = docuconf::check_declaration::<BadKeys>().unwrap_err();
    let text = e.to_string();
    assert!(text.contains("min_keys must be at least 1"), "{text}");
    assert!(text.contains("key_min_length must be at least 1"), "{text}");
}

#[derive(Deserialize, Docuconf, Debug)]
#[allow(dead_code)]
struct DeprecatedRequired {
    /// Port the service used to listen on.
    #[docuconf(deprecated = "Use PORT instead")]
    old_port: u16,
}

#[test]
fn deprecated_rules() {
    let e = docuconf::check_declaration::<DeprecatedRequired>().unwrap_err();
    assert!(
        e.to_string()
            .contains("a required variable cannot be deprecated"),
        "{e}"
    );
    // A blank or too-long message, or `required`, is a compile error
    // (src/compile_errors.rs); in a contract, a declaration error.
    let long = "x".repeat(501);
    let e = Contract::from_json(&format!(
        r#"{{"apiVersion": "docuconf.dev/v1alpha1", "kind": "ConfigContract",
            "metadata": {{"name": "svc"}},
            "vars": {{"OLD": {{"type": "int", "description": "Old port",
                             "deprecated": {{"message": "{long}"}}}}}}}}"#
    ))
    .unwrap_err();
    assert!(e.to_string().contains("at most 500"), "{e}");
}

#[derive(Deserialize, Docuconf, Debug)]
#[allow(dead_code)]
struct Deprecated {
    /// Token of the retired billing API.
    #[docuconf(
        deprecated = "The billing API no longer takes a token",
        replaced_by = "BILLING_KEY"
    )]
    old_token: Option<docuconf::Secret<String>>,
}

#[test]
fn deprecated_input_that_is_set_warns_without_its_value() {
    let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let w = warnings.clone();
    Loader::<Deprecated>::new()
        .env([("OLD_TOKEN", "tok-0123456789")])
        .on_warning(move |m| w.lock().unwrap().push(m.to_string()))
        .load()
        .unwrap();
    let got = warnings.lock().unwrap().join("\n");
    assert!(got.contains("OLD_TOKEN"), "{got}");
    assert!(
        got.contains("The billing API no longer takes a token"),
        "{got}"
    );
    assert!(got.contains("BILLING_KEY"), "{got}");
    assert!(!got.contains("tok-0123456789"), "{got}");
}

#[derive(Deserialize, Docuconf, Debug)]
#[allow(dead_code)]
struct Strict {
    /// A ratio with no bounds.
    ratio: Option<f64>,
    /// A count with no bounds.
    count: Option<i32>,
    /// A switch.
    flag: Option<bool>,
    /// A timeout.
    #[serde(default, with = "docuconf::humantime_serde::option")]
    wait: Option<Duration>,
    /// Names in csv.
    #[docuconf(encoding = "csv")]
    names: Option<Vec<String>>,
}

#[test]
fn strict_parsing_in_declaration_mode() {
    for (k, v) in [
        ("RATIO", "inf"),
        ("RATIO", "NaN"),
        ("RATIO", ".5"),
        ("RATIO", "5."),
        ("RATIO", "0x1p4"),
        ("RATIO", " 1.5"),
        ("COUNT", "0x10"),
        ("COUNT", "1_000"),
        ("COUNT", "5\n"),
        ("FLAG", "1"),
        ("FLAG", "yes"),
        ("WAIT", "5"),
        ("WAIT", "1d"),
    ] {
        let (c, text) = codes::<Strict>(&[(k, v)], k);
        assert_eq!(c, [Code::InvalidType], "{k}={v:?}: {text}");
    }
    let s: Strict = load(&[
        ("RATIO", "25e-2"),
        ("COUNT", "+007"),
        ("FLAG", "False"),
        ("WAIT", "+1.5h"),
        ("NAMES", "a, b ,"),
    ])
    .unwrap();
    assert_eq!(s.ratio, Some(0.25));
    assert_eq!(s.count, Some(7));
    assert_eq!(s.flag, Some(false));
    assert_eq!(s.wait, Some(Duration::from_secs(5400)));
    assert_eq!(s.names.unwrap(), ["a", " b ", ""]);
    // Go's grammar takes a sign; a std::time::Duration cannot hold it.
    let (c, _) = codes::<Strict>(&[("WAIT", "-5s")], "WAIT");
    assert_eq!(c, [Code::OutOfRange]);
    let (c, _) = codes::<Strict>(&[("COUNT", "2147483648")], "COUNT");
    assert_eq!(c, [Code::OutOfRange]);
}

#[test]
fn contract_first_negative_duration_and_key_set() {
    let c = Contract::from_json(
        r#"{"apiVersion": "docuconf.dev/v1alpha1", "kind": "ConfigContract",
            "metadata": {"name": "svc"},
            "vars": {
              "SKEW": {"type": "duration", "description": "Clock skew allowed"},
              "KEYS": {"type": "keySet", "description": "Keys that verify", "secret": true}
            }}"#,
    )
    .unwrap();
    let v = c.load_env([("SKEW", "-1m30s"), ("KEYS", "a,b")]).unwrap();
    assert_eq!(
        v.get("SKEW"),
        Some(&docuconf::contract::Value::NegativeDuration(
            Duration::from_secs(90)
        ))
    );
    assert_eq!(v.to_json()["SKEW"], "-1m30s");
    assert_eq!(
        v.get("KEYS").unwrap().as_key_set().unwrap().keys(),
        ["a", "b"]
    );
    assert!(!format!("{v:?}").contains("\"a\""));

    let e = Contract::from_json(
        r#"{"apiVersion": "docuconf.dev/v1alpha1", "kind": "ConfigContract",
            "metadata": {"name": "svc"},
            "vars": {"KEYS": {"type": "keySet", "description": "Keys that verify", "secret": false}}}"#,
    )
    .unwrap_err();
    assert!(e.to_string().contains("always secret"), "{e}");
    let e = Contract::from_json(
        r#"{"apiVersion": "docuconf.dev/v1alpha1", "kind": "ConfigContract",
            "metadata": {"name": "svc"},
            "vars": {"OLD": {"type": "int", "description": "Old port", "required": true,
                             "deprecated": {"message": " "}}}}"#,
    )
    .unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("cannot be deprecated") && text.contains("blank"),
        "{text}"
    );
}

#[derive(Deserialize, Docuconf, Debug)]
#[allow(dead_code)]
struct PlainKeys {
    /// Keys that verify, with no length bound but the empty key's.
    #[docuconf(max_keys = 3)]
    keys: KeySet,

    /// Tags, none of them empty.
    #[docuconf(encoding = "csv", item_min_length = 1)]
    tags: Option<Vec<String>>,

    /// Tokens, none of them empty.
    #[docuconf(encoding = "csv", item_min_length = 1)]
    tokens: Option<docuconf::Secret<Vec<String>>>,
}

/// An empty key is `key N is empty`, an empty list item `item N is empty`,
/// counted from 1, in both modes, and never shows a key or item.
#[test]
fn empty_key_and_item_wording() {
    let contract = Contract::from_json(
        r#"{"apiVersion": "docuconf.dev/v1alpha1", "kind": "ConfigContract",
            "metadata": {"name": "svc"},
            "vars": {
              "KEYS": {"type": "keySet", "description": "Keys that verify", "secret": true, "maxKeys": 3},
              "TAGS": {"type": "list", "items": "string", "description": "Tags", "encoding": "csv", "itemMinLength": 1},
              "TOKENS": {"type": "list", "items": "string", "description": "Tokens", "encoding": "csv", "itemMinLength": 1, "secret": true}
            }}"#,
    )
    .unwrap();
    let check = |name: &str, env: &[(&str, &str)], want: &str| {
        let decl = load::<PlainKeys>(env).unwrap_err();
        let Error::Validation(decl) = decl else {
            panic!("{decl}")
        };
        let first = contract.load_env(env.iter().copied()).unwrap_err();
        for e in [decl, first] {
            let v: Vec<_> = e.violations.iter().filter(|v| v.input == name).collect();
            assert_eq!(v.len(), 1, "{e}");
            assert_eq!((v[0].code, v[0].message.as_str()), (Code::OutOfRange, want));
            for secret in ["old", "new", "tok"] {
                assert!(!e.to_string().contains(secret), "{e}");
            }
        }
    };
    check("KEYS", &[("KEYS", "old,")], "key 2 is empty");
    check("KEYS", &[("KEYS", ",new")], "key 1 is empty");
    check("KEYS", &[("KEYS", "a,,b")], "key 2 is empty");
    check(
        "TAGS",
        &[("KEYS", "a"), ("TAGS", "x,,y")],
        "item 2 is empty",
    );
    check(
        "TOKENS",
        &[("KEYS", "a"), ("TOKENS", ",tok")],
        "item 1 is empty",
    );
}
