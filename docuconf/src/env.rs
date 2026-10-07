//! Reading declared variables from the environment, shared by the
//! figment-bound loader and contract-first mode, so both apply exactly the
//! same parsing and checks.

use std::collections::{BTreeSet, HashMap};

use crate::decl::{ListEncoding, VarDecl, VarKind};
use crate::error::{Code, Violation};
use crate::value::{self, Typed};

/// A snapshot of the environment. Values that are not valid UTF-8 are kept
/// by name, so they are reported as `invalid_type` rather than looking
/// unset.
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
        let mut env = Env::default();
        for (k, v) in std::env::vars_os() {
            // A name that is not UTF-8 cannot be a declared variable.
            let Ok(k) = k.into_string() else { continue };
            match v.into_string() {
                Ok(v) => {
                    env.vars.insert(k, v);
                }
                Err(_) => {
                    env.not_utf8.insert(k);
                }
            }
        }
        env
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

    /// The items of the indexed list `name` (SPEC §5): how many there are
    /// from `name__0` on, and the first missing index when a higher one is
    /// set. Only a decimal suffix with no leading zero is an item, so
    /// `name__HOST` and `name__01` are not.
    pub(crate) fn indexed_items(&self, name: &str) -> (usize, Option<usize>) {
        let prefix = format!("{name}__");
        let mut set = BTreeSet::new();
        let mut beyond = false;
        for key in self.vars.keys().chain(self.not_utf8.iter()) {
            let Some(n) = key.strip_prefix(&prefix) else {
                continue;
            };
            if !is_index(n) {
                continue;
            }
            match n.parse::<usize>() {
                Ok(i) => {
                    set.insert(i);
                }
                // Too large to be anything but past a gap.
                Err(_) => beyond = true,
            }
        }
        let count = (0..).find(|i| !set.contains(i)).unwrap_or(0);
        let gap = (beyond || set.len() > count).then_some(count);
        (count, gap)
    }

    fn raw(&self, name: &str) -> Raw<'_> {
        match self.vars.get(name) {
            Some(v) => Raw::Value(v),
            None if self.not_utf8.contains(name) => Raw::NotUtf8,
            None => Raw::Absent,
        }
    }
}

/// A decimal index with no leading zero.
fn is_index(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
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
    let raws: Vec<&str> = if matches!(var.kind, VarKind::List(_))
        && var.list_encoding == ListEncoding::Indexed
    {
        let (count, gap) = env.indexed_items(&var.name);
        if let Some(missing) = gap {
            return Err(violation(
                    var,
                    Code::InvalidType,
                    format!(
                        "items must be numbered from {name}__0 with no gap, but {name}__{missing} is not set",
                        name = var.name
                    ),
                ));
        }
        if count == 0 {
            return Ok(None);
        }
        let mut items = Vec::with_capacity(count);
        for i in 0..count {
            match env.raw(&format!("{}__{i}", var.name)) {
                Raw::Value(v) => items.push(v),
                Raw::NotUtf8 => return Err(not_utf8()),
                Raw::Absent => unreachable!("indexed_items counted {}__{i}", var.name),
            }
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
