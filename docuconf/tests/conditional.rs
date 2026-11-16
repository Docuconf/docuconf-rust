//! Contract-first mode: `requiredIf` (SPEC §4.2.1), roles (SPEC §4.9) and
//! `includes` (SPEC §4.8).

use docuconf::{Code, Contract};
use serde_json::{json, Value as Json};

fn contract(extra: Json) -> Result<Contract, docuconf::DeclarationError> {
    let mut doc = json!({
        "apiVersion": "docuconf.dev/v1alpha1",
        "kind": "ConfigContract",
        "metadata": {"name": "relay"},
    });
    for (k, v) in extra.as_object().unwrap() {
        doc[k] = v.clone();
    }
    Contract::from_value(&doc)
}

fn relay() -> Contract {
    contract(json!({
        "vars": {
            "APP_ENV": {"type": "string", "description": "Selects the profile", "default": "development"},
            "TLS_ENABLED": {"type": "bool", "description": "Serve HTTPS", "default": false},
            "CA_SUBJECT": {"type": "string", "description": "Client CA subject", "requiredIf": {"TLS_ENABLED": true}},
            "LOG_FORMAT": {"type": "enum", "description": "Log format", "values": ["text", "json"], "default": "text"},
            "LOG_SCHEMA_URL": {"type": "url", "description": "Log schema", "requiredIf": {"LOG_FORMAT": "json"}},
            "PORT": {"type": "int", "description": "Listen port", "default": 8080},
            "ACME_EMAIL": {"type": "string", "description": "ACME contact", "requiredIf": {"PORT": 443}},
        },
        "files": {
            "allowlist": {"type": "text", "description": "Allowed clients", "path": "/etc/relay/allow.txt",
                          "requiredIf": {"TLS_ENABLED": true}},
        },
        "profiles": {"selector": "APP_ENV", "default": "development",
                     "defaults": {"production": {"TLS_ENABLED": true}}},
    }))
    .unwrap()
}

fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn conditional_inputs_are_optional_while_their_conditions_are_off() {
    let dir = root();
    let v = relay()
        .load_env([("DOCUCONF_FILE_ROOT", dir.path().to_str().unwrap())])
        .unwrap();
    assert_eq!(v.get("CA_SUBJECT"), None);
    assert!(v.file("allowlist").is_none());
}

#[test]
fn messages_name_the_condition_with_the_value_as_json() {
    let dir = root();
    let e = relay()
        .load_env([
            ("DOCUCONF_FILE_ROOT", dir.path().to_str().unwrap()),
            ("TLS_ENABLED", "true"),
            ("LOG_FORMAT", "json"),
            ("PORT", "443"),
        ])
        .unwrap_err();
    let got: Vec<(String, Code, String)> = e
        .violations
        .iter()
        .map(|v| (v.input.clone(), v.code, v.message.clone()))
        .collect();
    let path = dir.path().join("etc/relay/allow.txt");
    assert_eq!(
        got,
        [
            (
                "ACME_EMAIL".to_string(),
                Code::MissingRequired,
                "is required when PORT is 443".to_string()
            ),
            (
                "CA_SUBJECT".to_string(),
                Code::MissingRequired,
                "is required when TLS_ENABLED is true".to_string()
            ),
            (
                "LOG_SCHEMA_URL".to_string(),
                Code::MissingRequired,
                r#"is required when LOG_FORMAT is "json""#.to_string()
            ),
            (
                "allowlist".to_string(),
                Code::FileMissing,
                format!(
                    "{} does not exist, and is required when TLS_ENABLED is true",
                    path.display()
                )
            ),
        ]
    );
}

#[test]
fn conditions_compare_the_loaded_value() {
    let dir = root();
    let root = dir.path().to_str().unwrap();
    // The selected profile turns TLS on.
    let e = relay()
        .load_env([("DOCUCONF_FILE_ROOT", root), ("APP_ENV", "production")])
        .unwrap_err();
    assert_eq!(e.codes_for("CA_SUBJECT"), [Code::MissingRequired]);
    assert_eq!(e.codes_for("allowlist"), [Code::FileMissing]);
    // The environment beats the profile.
    relay()
        .load_env([
            ("DOCUCONF_FILE_ROOT", root),
            ("APP_ENV", "production"),
            ("TLS_ENABLED", "false"),
        ])
        .unwrap();
    // A condition variable with a violation of its own has no value.
    let e = relay()
        .load_env([("DOCUCONF_FILE_ROOT", root), ("TLS_ENABLED", "yes")])
        .unwrap_err();
    assert_eq!(e.violations.len(), 1, "{e}");
    assert_eq!(e.codes_for("TLS_ENABLED"), [Code::InvalidType]);
}

fn shop() -> Contract {
    contract(json!({
        "roles": ["web", "worker"],
        "vars": {
            "DOCUCONF_ROLE": {"type": "enum", "description": "The workload's role", "required": true, "values": ["web", "worker"]},
            "ALLOWED_ORIGINS": {"type": "list", "items": "string", "description": "Origins", "required": true, "roles": ["web"]},
            "QUEUES": {"type": "list", "items": "string", "description": "Queues", "required": true, "roles": ["worker"]},
            "WEB_TLS": {"type": "bool", "description": "Serve HTTPS", "default": true},
            "CA_SUBJECT": {"type": "string", "description": "Client CA subject", "roles": ["web"],
                           "requiredIf": {"WEB_TLS": true}},
        },
        "files": {
            "robots": {"type": "text", "description": "robots.txt", "required": true, "roles": ["web"],
                       "path": "/etc/shop/robots.txt"},
        },
    }))
    .unwrap()
}

#[test]
fn a_role_requires_only_its_own_inputs() {
    let dir = root();
    let root = dir.path().to_str().unwrap();
    let c = shop();
    assert_eq!(
        c.roles(),
        Some(&["web".to_string(), "worker".to_string()][..])
    );

    let v = c
        .load_env([
            ("DOCUCONF_FILE_ROOT", root),
            ("DOCUCONF_ROLE", "worker"),
            ("QUEUES", "mail"),
        ])
        .unwrap();
    assert_eq!(v.get("ALLOWED_ORIGINS"), None);
    assert!(v.file("robots").is_none());

    let e = c
        .load_env([("DOCUCONF_FILE_ROOT", root), ("DOCUCONF_ROLE", "web")])
        .unwrap_err();
    let mut inputs: Vec<&str> = e.violations.iter().map(|v| v.input.as_str()).collect();
    inputs.sort();
    assert_eq!(inputs, ["ALLOWED_ORIGINS", "CA_SUBJECT", "robots"], "{e}");
}

#[test]
fn without_a_valid_role_the_role_is_the_one_problem() {
    let dir = root();
    let root = dir.path().to_str().unwrap();
    let e = shop().load_env([("DOCUCONF_FILE_ROOT", root)]).unwrap_err();
    assert_eq!(e.violations.len(), 1, "{e}");
    assert_eq!(e.codes_for("DOCUCONF_ROLE"), [Code::MissingRequired]);

    let e = shop()
        .load_env([("DOCUCONF_FILE_ROOT", root), ("DOCUCONF_ROLE", "cron")])
        .unwrap_err();
    assert_eq!(e.violations.len(), 1, "{e}");
    assert_eq!(e.codes_for("DOCUCONF_ROLE"), [Code::NotInEnum]);
}

#[test]
fn includes_are_read() {
    let c = contract(json!({
        "includes": ["go-runtime@1", "otel@1"],
        "vars": {"GOMAXPROCS": {"type": "int", "description": "Go threads", "min": 1}},
    }))
    .unwrap();
    assert_eq!(c.includes(), ["go-runtime@1", "otel@1"]);
    assert_eq!(
        c.load_env([("GOMAXPROCS", "4")])
            .unwrap()
            .get("GOMAXPROCS")
            .and_then(|v| v.as_int()),
        Some(4)
    );
}

fn rejected(extra: Json) -> String {
    match contract(extra) {
        Ok(_) => panic!("the contract was accepted"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn rejects_broken_conditions() {
    let base = |input: Json| {
        json!({"vars": {
            "TLS_ENABLED": {"type": "bool", "description": "Serve HTTPS"},
            "PORT": {"type": "int", "description": "Listen port", "min": 1, "max": 65535},
            "MODE": {"type": "enum", "description": "Run mode", "values": ["a", "b"]},
            "TIMEOUT": {"type": "duration", "description": "Timeout"},
            "TOKEN": {"type": "string", "description": "API token", "secret": true},
            "INPUT": input,
        }})
    };
    let cases = [
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {}}),
            "requiredIf must name one variable and its value",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"TLS_ENABLED": true, "PORT": 1}}),
            "requiredIf must name one variable and its value",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"TLS_ENABLED": [true]}}),
            "a string, integer or boolean value",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"NOPE": true}}),
            "requiredIf must name another declared variable, not \"NOPE\"",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"INPUT": "x"}}),
            "requiredIf must name another declared variable, not \"INPUT\"",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"TOKEN": "x"}}),
            "requiredIf names TOKEN, which is not a plain string, int, bool or enum variable",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"TIMEOUT": "1m"}}),
            "requiredIf names TIMEOUT, which is not a plain",
        ),
        // A bool is not an int, nor an int a bool.
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"PORT": true}}),
            "requiredIf: PORT is true is not a value PORT can have",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"TLS_ENABLED": 1}}),
            "requiredIf: TLS_ENABLED is 1 is not a value TLS_ENABLED can have",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"PORT": 0}}),
            "requiredIf: PORT is 0 is not a value PORT can have: ",
        ),
        (
            json!({"type": "string", "description": "Conditional", "requiredIf": {"MODE": "c"}}),
            r#"requiredIf: MODE is "c" is not a value MODE can have: "#,
        ),
        (
            json!({"type": "string", "description": "Conditional", "required": true, "requiredIf": {"TLS_ENABLED": true}}),
            "INPUT: an input with requiredIf is neither required nor has a default",
        ),
        (
            json!({"type": "string", "description": "Conditional", "default": "x", "requiredIf": {"TLS_ENABLED": true}}),
            "INPUT: an input with requiredIf is neither required nor has a default",
        ),
        (
            json!({"type": "string", "description": "Conditional", "deprecated": {"message": "Going away"},
                   "requiredIf": {"TLS_ENABLED": true}}),
            "INPUT: a conditionally required input cannot be deprecated",
        ),
    ];
    for (input, want) in cases {
        let e = rejected(base(input.clone()));
        assert!(e.contains(want), "{input}: want {want:?} in {e}");
    }

    // File inputs too.
    let e = rejected(json!({
        "vars": {"TLS_ENABLED": {"type": "bool", "description": "Serve HTTPS"}},
        "files": {"ca": {"type": "text", "description": "Client CA", "path": "/etc/ca/ca.txt",
                         "required": true, "requiredIf": {"TLS_ENABLED": true}}},
    }));
    assert!(
        e.contains("file input ca: an input with requiredIf is neither required"),
        "{e}"
    );
}

#[test]
fn rejects_broken_roles_and_includes() {
    let role_var = json!({"type": "enum", "description": "The workload's role", "required": true, "values": ["web", "worker"]});
    let cases = [
        (json!({"roles": []}), "roles must be a list of at least one role name"),
        (json!({"roles": "web"}), "roles must be a list of at least one role name"),
        (json!({"roles": ["Web"]}), "role \"Web\" must be a DNS label"),
        (json!({"roles": ["web-"]}), "role \"web-\" must be a DNS label"),
        (json!({"roles": ["web", "web"]}), "role web is listed twice"),
        (
            json!({"vars": {"PORT": {"type": "int", "description": "Listen port", "roles": ["web"]}}}),
            "PORT: has roles, but the contract declares none",
        ),
        (
            json!({"roles": ["web", "worker"], "vars": {
                "DOCUCONF_ROLE": role_var.clone(),
                "PORT": {"type": "int", "description": "Listen port", "roles": ["cron"]}}}),
            "PORT: role cron is not one of the contract's roles, web, worker",
        ),
        (
            json!({"roles": ["web", "worker"], "vars": {"DOCUCONF_ROLE": role_var.clone()},
                   "files": {"robots": {"type": "text", "description": "robots.txt", "path": "/etc/r/robots.txt", "roles": []}}}),
            "file input robots: roles must be a list of at least one role name",
        ),
        (
            json!({"roles": ["web", "worker"]}),
            "DOCUCONF_ROLE: a contract with roles declares it as a required enum whose values are the roles, web, worker",
        ),
        (
            json!({"roles": ["web", "worker"], "vars": {"DOCUCONF_ROLE":
                {"type": "enum", "description": "The workload's role", "values": ["web", "worker"]}}}),
            "DOCUCONF_ROLE: a contract with roles declares it",
        ),
        (
            json!({"roles": ["web", "worker"], "vars": {"DOCUCONF_ROLE":
                {"type": "enum", "description": "The workload's role", "required": true, "values": ["worker", "web"]}}}),
            "DOCUCONF_ROLE: a contract with roles declares it",
        ),
        (
            json!({"roles": ["web", "worker"], "vars": {"DOCUCONF_ROLE":
                {"type": "enum", "description": "The workload's role", "required": true, "values": ["web", "worker"], "roles": ["web"]}}}),
            "DOCUCONF_ROLE: a contract with roles declares it",
        ),
        (json!({"includes": "otel@1"}), "includes must be a list of fragment names"),
        (json!({"includes": ["otel"]}), "includes: \"otel\" is not a fragment name, name@major"),
        (json!({"includes": ["otel@0"]}), "includes: \"otel@0\" is not a fragment name"),
        (json!({"includes": ["otel@1", "otel@1"]}), "includes: otel@1 is listed twice"),
    ];
    for (extra, want) in cases {
        let e = rejected(extra.clone());
        assert!(e.contains(want), "{extra}: want {want:?} in {e}");
    }
}
