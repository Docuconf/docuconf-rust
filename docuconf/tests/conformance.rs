//! The shared docuconf conformance suite (SPEC §12), run through
//! contract-first mode.
//!
//! `cases.json` comes from docuconf-go: `$DOCUCONF_CONFORMANCE`, or
//! `../docuconf-go/conformance/cases.json` next to this repository. The
//! suite is skipped when the file is missing, unless
//! `DOCUCONF_REQUIRE_CONFORMANCE=1`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use docuconf::Contract;
use serde_json::Value as Json;

/// Capability tags this SDK supports (conformance/README.md). Cases that
/// require any other tag are skipped.
const SUPPORTED: &[&str] = &["int64", "json-schema"];

fn cases_path() -> PathBuf {
    match std::env::var_os("DOCUCONF_CONFORMANCE") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docuconf-go/conformance/cases.json"),
    }
}

/// JSON equality with numbers compared as the suite asks: integers
/// exactly, floats numerically (`3` equals `3.0`).
fn same(want: &Json, got: &Json) -> bool {
    match (want, got) {
        (Json::Number(a), Json::Number(b)) => match (a.as_i64(), b.as_i64()) {
            (Some(x), Some(y)) => x == y,
            _ => match (a.as_u64(), b.as_u64()) {
                (Some(x), Some(y)) => x == y,
                _ => a.as_f64() == b.as_f64(),
            },
        },
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
        }
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w)))
        }
        _ => want == got,
    }
}

/// Runs one case; `Err` explains the failure.
fn run(case: &Json) -> Result<(), String> {
    let contract =
        Contract::from_value(&case["contract"]).map_err(|e| format!("contract rejected: {e}"))?;
    let env: Vec<(String, String)> = case["env"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
        .collect();
    let result = contract.load_env(env.clone());

    if let Some(expect) = case.get("expect").and_then(Json::as_object) {
        let values = result.map_err(|e| format!("expected values, got {e}"))?;
        let got = values.to_json();
        let mut diffs = Vec::new();
        for (name, want) in expect {
            let g = got.get(name).cloned().unwrap_or(Json::Null);
            if !same(want, &g) {
                diffs.push(format!("{name}: want {want}, got {g}"));
            }
        }
        return if diffs.is_empty() {
            Ok(())
        } else {
            Err(diffs.join("; "))
        };
    }

    let Some(errors) = case.get("errors").and_then(Json::as_array) else {
        return Err("case has neither expect nor errors".into());
    };
    let err = match result {
        Ok(v) => return Err(format!("expected errors, loaded {v:?}")),
        Err(e) => e,
    };
    let want: BTreeSet<(String, String)> = errors
        .iter()
        .map(|e| {
            (
                e["var"].as_str().unwrap_or_default().to_string(),
                e["code"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let got: BTreeSet<(String, String)> = err
        .violations
        .iter()
        .map(|v| (v.input.clone(), v.code.as_str().to_string()))
        .collect();
    if want != got {
        return Err(format!("want errors {want:?}, got {got:?}\n      {err}"));
    }
    // No error output may hold a secret's raw value. The termination log
    // holds exactly this text.
    let text = err.to_string();
    let vars = case["contract"]["vars"].as_object();
    for (name, raw) in &env {
        let secret = vars
            .and_then(|v| v.get(name))
            .and_then(|v| v.get("secret"))
            .and_then(Json::as_bool)
            .unwrap_or(false);
        if secret && !raw.is_empty() && text.contains(raw.as_str()) {
            return Err(format!("error output contains the value of secret {name}"));
        }
    }
    Ok(())
}

#[test]
fn conformance_suite() {
    let path = cases_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            if std::env::var("DOCUCONF_REQUIRE_CONFORMANCE").as_deref() == Ok("1") {
                panic!(
                    "conformance cases not found at {} ({e}); set DOCUCONF_CONFORMANCE",
                    path.display()
                );
            }
            eprintln!(
                "skipping the conformance suite: {} not found (set DOCUCONF_CONFORMANCE)",
                path.display()
            );
            return;
        }
    };
    let doc: Json = serde_json::from_str(&data).expect("cases.json is not JSON");
    assert_eq!(doc["version"], 1, "unsupported cases.json version");
    let cases = doc["cases"].as_array().expect("cases.json has no cases");

    let (mut passed, mut skipped, mut failures) = (0, 0, Vec::new());
    for case in cases {
        let id = case["id"].as_str().unwrap_or("<no id>");
        let missing: Vec<&str> = case["requires"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .filter(|t| !SUPPORTED.contains(t))
            .collect();
        if !missing.is_empty() {
            skipped += 1;
            eprintln!("skip {id}: requires {}", missing.join(", "));
            continue;
        }
        match run(case) {
            Ok(()) => passed += 1,
            Err(why) => failures.push(format!("{id}: {why}")),
        }
    }
    eprintln!(
        "conformance: {passed} passed, {skipped} skipped, {} failed ({} cases)",
        failures.len(),
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "{} conformance case(s) failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
