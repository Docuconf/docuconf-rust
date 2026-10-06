//! Reading declared variables from the environment, shared by the
//! figment-bound loader and contract-first mode, so both apply exactly the
//! same parsing and checks.

use std::collections::{BTreeSet, HashMap};

use crate::decl::{ListEncoding, VarDecl, VarKind};
use crate::error::{Code, Violation};
use crate::value::{self, Typed};

/// A snapshot of the environment.
#[derive(Debug, Clone, Default)]
pub(crate) struct Env {
    pub vars: HashMap<String, String>,
    pub not_utf8: BTreeSet<String>,
}

enum Raw<'a> {
    Absent,
    NotUtf8,
    Value(&'a str),
}

impl Env {
    /// The process environment, as it is now.
    pub(crate) fn process() -> Env {
        Env::from_map(
            std::env::vars_os()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect(),
        )
    }

    pub(crate) fn from_map(vars: HashMap<String, String>) -> Env {
        Env {
            vars,
            not_utf8: BTreeSet::new(),
        }
    }

    /// Adds `key` unless the environment already has it.
    pub(crate) fn add_default(&mut self, key: String, value: String) {
        if !self.not_utf8.contains(&key) {
            self.vars.entry(key).or_insert(value);
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    fn raw(&self, name: &str) -> Raw<'_> {
        match self.vars.get(name) {
            Some(v) => Raw::Value(v),
            None if self.not_utf8.contains(name) => Raw::NotUtf8,
            None => Raw::Absent,
        }
    }
}

fn violation(var: &VarDecl, code: Code, message: String) -> Violation {
    Violation {
        input: var.name.clone(),
        code,
        message,
    }
}

/// How a raw value appears in a message: quoted, or `value` for a secret.
fn shown(var: &VarDecl, raw: &str) -> String {
    if var.secret {
        "value".to_string()
    } else {
        Typed::Str(raw.to_string()).show()
    }
}

/// The scheme of an injector reference (Bank-Vaults `vault:`, 1Password
/// `op://`, vals `ref+`) when `raw` is one.
fn unresolved_reference(raw: &str) -> Option<&'static str> {
    ["vault:", "op://", "ref+"]
        .into_iter()
        .find(|scheme| raw.starts_with(scheme))
}

/// Reads and parses one variable from the environment in its wire
/// encoding. `Ok(None)` when it is unset: absent, or empty for any type but
/// `string` (SPEC §5).
pub(crate) fn read(var: &VarDecl, env: &Env) -> Result<Option<Typed>, Violation> {
    let not_utf8 = || {
        violation(
            var,
            Code::InvalidType,
            "value is not valid UTF-8".to_string(),
        )
    };
    // The raw strings: one, or one per item of an indexed list.
    let raws: Vec<&str> =
        if matches!(var.kind, VarKind::List(_)) && var.list_encoding == ListEncoding::Indexed {
            let mut items = Vec::new();
            loop {
                match env.raw(&format!("{}__{}", var.name, items.len())) {
                    Raw::Absent => break,
                    Raw::NotUtf8 => return Err(not_utf8()),
                    Raw::Value(v) => items.push(v),
                }
            }
            if items.is_empty() {
                return Ok(None);
            }
            items
        } else {
            match env.raw(&var.name) {
                Raw::Absent => return Ok(None),
                Raw::NotUtf8 => return Err(not_utf8()),
                Raw::Value(v) if v.is_empty() && !matches!(var.kind, VarKind::String) => {
                    return Ok(None)
                }
                Raw::Value(v) => vec![v],
            }
        };

    if let Some(msg) = &var.deprecated {
        let by = var
            .replaced_by
            .as_ref()
            .map(|r| format!("; use {r}"))
            .unwrap_or_default();
        log::warn!("docuconf: {} is deprecated: {msg}{by}", var.name);
    }
    // An injector reference that is still there means the injector did not
    // run (SPEC §4.5.1, §11.2). Name the scheme, never the value.
    if var.secret {
        if let Some(scheme) = raws.iter().find_map(|r| unresolved_reference(r)) {
            return Err(violation(
                var,
                Code::InvalidType,
                format!(
                    "holds an unresolved {scheme} reference; the injector that should resolve it did not run"
                ),
            ));
        }
    }
    let parsed = match (&var.kind, raws.as_slice()) {
        (VarKind::List(item), _) if var.list_encoding == ListEncoding::Indexed => {
            value::parse_items(*item, raws.iter().copied())
        }
        (_, [raw]) => value::parse_wire(var, raw),
        _ => unreachable!("only an indexed list has several raw values"),
    };
    parsed.map(Some).map_err(|(code, e)| {
        let what = match raws.as_slice() {
            [raw] => shown(var, raw),
            _ => "value".to_string(),
        };
        violation(var, code, format!("{what} {e}"))
    })
}

/// Checks a variable's final value (from the environment, a config file or
/// its default) against its constraints.
pub(crate) fn finish(var: &VarDecl, t: Typed) -> Result<Typed, Vec<Violation>> {
    let problems = value::check(var, &t);
    if problems.is_empty() {
        return Ok(t);
    }
    let hint = match (&t, var.secret) {
        (Typed::Str(s), true) if s.ends_with('\n') => {
            " (the value ends in a newline: was the secret created from a file?)"
        }
        _ => "",
    };
    Err(problems
        .into_iter()
        .map(|(code, msg)| violation(var, code, format!("{msg}{hint}")))
        .collect())
}

/// The violation for a required variable nothing sets.
pub(crate) fn missing(var: &VarDecl) -> Violation {
    violation(
        var,
        Code::MissingRequired,
        "is required but not set".to_string(),
    )
}
