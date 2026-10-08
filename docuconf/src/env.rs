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
/// its default) against its constraints. `from_env` is true when the value
/// was read from the environment by [`read`].
pub(crate) fn finish(var: &VarDecl, t: Typed, from_env: bool) -> Result<Typed, Vec<Violation>> {
    let problems = value::check_value(var, &t, from_env);
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

/// The warning for a deprecated variable that is set, if `var` is one.
pub(crate) fn deprecation(var: &VarDecl, env: &Env) -> Option<String> {
    let msg = var.deprecated.as_ref()?;
    let set = env.vars.get(&var.name).is_some_and(|v| !v.is_empty())
        || (var.list_encoding == ListEncoding::Indexed && env.indexed_items(&var.name).0 > 0);
    if !set {
        return None;
    }
    let by = var
        .replaced_by
        .as_ref()
        .map(|r| format!("; use {r}"))
        .unwrap_or_default();
    Some(format!("{} is deprecated: {msg}{by}", var.name))
}

/// Variables every process has, never typos of a declared name.
const SYSTEM: &[&str] = &[
    "HOME", "HOSTNAME", "LANG", "LANGUAGE", "LOGNAME", "MAIL", "OLDPWD", "PATH", "PWD", "SHELL",
    "SHLVL", "TERM", "TMPDIR", "TZ", "USER",
];

/// Levenshtein distance, giving up above `max`.
fn distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    let d = prev[b.len()];
    (d <= max).then_some(d)
}

/// Warnings for set variables that are not declared but are within edit
/// distance 2 of a declared name (1 for names shorter than 8 characters,
/// where 2 edits turn one ordinary name into another): `DATABSE_URL is set
/// but not declared; did you mean DATABASE_URL?`. With a prefix, only
/// variables under it are considered, and every undeclared one is
/// reported. Values are never shown.
pub(crate) fn typo_hints(
    vars: &[VarDecl],
    prefix: &str,
    extra: &[String],
    env: &Env,
) -> Vec<String> {
    let declared: Vec<&str> = vars
        .iter()
        .map(|v| v.name.as_str())
        .chain(extra.iter().map(String::as_str))
        .collect();
    let mut keys: Vec<&String> = env.vars.keys().chain(env.not_utf8.iter()).collect();
    keys.sort();
    let mut out = Vec::new();
    for key in keys {
        if declared.contains(&key.as_str())
            || !crate::decl::is_env_name(key)
            || !key.starts_with(prefix)
            || key.starts_with("DOCUCONF_")
            || SYSTEM.contains(&key.as_str())
            || declared.iter().any(|d| key.starts_with(&format!("{d}__")))
        {
            continue;
        }
        let best = declared
            .iter()
            .filter(|d| d.starts_with(prefix))
            .filter_map(|d| {
                let max = if d.len() >= 8 { 2 } else { 1 };
                distance(key, d, max).map(|n| (n, *d))
            })
            .min();
        if let Some((_, d)) = best {
            out.push(format!("{key} is set but not declared; did you mean {d}?"));
        } else if !prefix.is_empty() {
            // Under the app's own prefix, any undeclared variable is a
            // mistake or a leftover.
            out.push(format!(
                "{key} is set but not declared (no variable under prefix {prefix} has that name)"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::distance;

    #[test]
    fn edit_distance() {
        assert_eq!(distance("DATABSE_URL", "DATABASE_URL", 2), Some(1));
        assert_eq!(distance("REQUEST_TIMEOUT", "REQUESTTIMEOUT", 2), Some(1));
        assert_eq!(distance("PORT", "PORT", 2), Some(0));
        assert_eq!(distance("HOST", "PORT", 2), Some(2));
        assert_eq!(distance("PATH", "PORT", 2), None);
        assert_eq!(distance("A", "ABCD", 2), None);
    }
}
