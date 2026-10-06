//! Typed values: parsing the wire form (SPEC §5), reading values from
//! figment layers, and checking constraints.

use std::time::Duration;

use figment::value::{Num, Value};

use crate::__private::Lit;
use crate::decl::{ItemKind, VarDecl, VarKind};
use crate::duration::{format_go, parse_duration};
use crate::error::Code;

/// A value of one of the contract types.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Typed {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Dur(Duration),
    List(Vec<Typed>),
    Json(serde_json::Value),
}

impl Typed {
    /// The value for a message: quoted strings, Go durations, JSON lists.
    pub(crate) fn show(&self) -> String {
        match self {
            Typed::Str(s) => serde_json::to_string(s).unwrap_or_default(),
            Typed::Int(i) => i.to_string(),
            Typed::Float(f) => f.to_string(),
            Typed::Bool(b) => b.to_string(),
            Typed::Dur(d) => format_go(*d),
            Typed::List(_) | Typed::Json(_) => {
                serde_json::to_string(&self.to_json()).unwrap_or_default()
            }
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Typed::Str(s) => s.clone().into(),
            Typed::Int(i) => (*i).into(),
            Typed::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Typed::Bool(b) => (*b).into(),
            Typed::Dur(d) => format_go(*d).into(),
            Typed::List(l) => l.iter().map(Typed::to_json).collect(),
            Typed::Json(j) => j.clone(),
        }
    }

    /// The figment value the host deserializes the field from.
    pub(crate) fn to_figment(&self) -> Value {
        match self {
            Typed::Str(s) => Value::from(s.clone()),
            Typed::Int(i) => Value::from(*i),
            Typed::Float(f) => Value::from(*f),
            Typed::Bool(b) => Value::from(*b),
            Typed::Dur(d) => Value::from(format_go(*d)),
            Typed::List(l) => Value::from(l.iter().map(Typed::to_figment).collect::<Vec<_>>()),
            Typed::Json(j) => Value::serialize(j).unwrap_or_else(|_| Value::from(j.to_string())),
        }
    }
}

/// Converts a `default = ...` literal for a variable of `kind`.
pub(crate) fn lit_value(kind: &VarKind, l: &Lit) -> Result<Typed, String> {
    match (kind, l) {
        (VarKind::String | VarKind::Url | VarKind::Enum(_), Lit::Str(s)) => {
            Ok(Typed::Str(s.to_string()))
        }
        (VarKind::Int { .. }, Lit::Int(i)) => i64::try_from(*i)
            .map(Typed::Int)
            .map_err(|_| format!("{i} is outside the 64-bit integer range")),
        (VarKind::Float, Lit::Int(i)) => Ok(Typed::Float(*i as f64)),
        (VarKind::Float, Lit::Float(f)) => Ok(Typed::Float(*f)),
        (VarKind::Bool, Lit::Bool(b)) => Ok(Typed::Bool(*b)),
        (VarKind::Duration, Lit::Str(s)) => parse_duration(s)
            .map(Typed::Dur)
            .map_err(|e| format!("{s:?} {e}")),
        (VarKind::List(item), Lit::List(items)) => items
            .iter()
            .map(|l| match (item, l) {
                (ItemKind::String, Lit::Str(s)) => Ok(Typed::Str(s.to_string())),
                (ItemKind::Int { .. }, Lit::Int(i)) => i64::try_from(*i)
                    .map(Typed::Int)
                    .map_err(|_| format!("{i} is outside the 64-bit integer range")),
                _ => Err(format!("list item {l:?} does not match the item type")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Typed::List),
        (VarKind::Json { .. }, Lit::Str(s)) => serde_json::from_str(s)
            .map(Typed::Json)
            .map_err(|e| format!("is not valid JSON: {e}")),
        (k, l) => Err(format!(
            "{l:?} is not a {} value{}",
            k.type_name(),
            match k {
                VarKind::Duration => " (write it as a string such as \"30s\")",
                VarKind::Json { .. } => " (write it as a JSON string)",
                _ => "",
            }
        )),
    }
}

/// Converts a `min = ...` / `max = ...` literal.
pub(crate) fn lit_bound(kind: &VarKind, l: &Lit) -> Result<Typed, String> {
    match kind {
        VarKind::Int { .. } | VarKind::Float | VarKind::Duration => lit_value(kind, l),
        k => Err(format!("does not apply to a {} variable", k.type_name())),
    }
}

/// Parses the raw environment string for a variable (SPEC §5): values are
/// never trimmed, booleans are `true`/`false` in any case, lists are JSON
/// arrays (what figment's own `Env` provider parses), durations are Go
/// syntax as `humantime` reads it.
pub(crate) fn parse_wire(kind: &VarKind, raw: &str) -> Result<Typed, String> {
    match kind {
        VarKind::String | VarKind::Url | VarKind::Enum(_) => Ok(Typed::Str(raw.to_string())),
        VarKind::Int { .. } => raw
            .parse::<i64>()
            .map(Typed::Int)
            .map_err(|_| "is not a 64-bit integer".into()),
        VarKind::Float => match raw.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Typed::Float(f)),
            _ => Err("is not a finite number".into()),
        },
        VarKind::Bool => {
            if raw.eq_ignore_ascii_case("true") {
                Ok(Typed::Bool(true))
            } else if raw.eq_ignore_ascii_case("false") {
                Ok(Typed::Bool(false))
            } else {
                Err("is not true or false".into())
            }
        }
        VarKind::Duration => parse_duration(raw).map(Typed::Dur),
        VarKind::List(item) => {
            let arr: Vec<serde_json::Value> = serde_json::from_str(raw)
                .map_err(|_| "is not a JSON array such as [\"a\",\"b\"]".to_string())?;
            arr.iter()
                .map(|x| json_item(*item, x))
                .collect::<Result<Vec<_>, _>>()
                .map(Typed::List)
        }
        VarKind::Json { .. } => serde_json::from_str(raw)
            .map(Typed::Json)
            .map_err(|_| "is not valid JSON".into()),
    }
}

fn json_item(item: ItemKind, x: &serde_json::Value) -> Result<Typed, String> {
    match item {
        ItemKind::String => x
            .as_str()
            .map(|s| Typed::Str(s.to_string()))
            .ok_or_else(|| "has an item that is not a string".to_string()),
        ItemKind::Int { .. } => x
            .as_i64()
            .map(Typed::Int)
            .ok_or_else(|| "has an item that is not an integer".to_string()),
    }
}

fn num_i64(n: &Num) -> Option<i64> {
    // JSON and YAML parsers give non-negative integers as unsigned, which
    // figment's to_i128 does not convert.
    match n.to_i128() {
        Some(i) => i64::try_from(i).ok(),
        None => n.to_u128().and_then(|u| i64::try_from(u).ok()),
    }
}

/// Reads a variable's value from a merged figment layer (an app config file
/// or the declaration's defaults).
pub(crate) fn from_figment(kind: &VarKind, v: &Value) -> Result<Typed, String> {
    let wrong = || format!("is not a {} value", kind.type_name());
    match (kind, v) {
        (VarKind::String | VarKind::Url | VarKind::Enum(_), Value::String(_, s)) => {
            Ok(Typed::Str(s.clone()))
        }
        (VarKind::Int { .. }, Value::Num(_, n)) => num_i64(n).map(Typed::Int).ok_or_else(wrong),
        (VarKind::Float, Value::Num(_, n)) => match n
            .to_f64()
            .or_else(|| n.to_i128().map(|i| i as f64))
            .or_else(|| n.to_u128().map(|u| u as f64))
        {
            Some(f) if f.is_finite() => Ok(Typed::Float(f)),
            _ => Err(wrong()),
        },
        (VarKind::Bool, Value::Bool(_, b)) => Ok(Typed::Bool(*b)),
        (VarKind::Duration, Value::String(_, s)) => parse_duration(s).map(Typed::Dur),
        (VarKind::List(item), Value::Array(_, items)) => items
            .iter()
            .map(|x| match (item, x) {
                (ItemKind::String, Value::String(_, s)) => Ok(Typed::Str(s.clone())),
                (ItemKind::Int { .. }, Value::Num(_, n)) => {
                    num_i64(n).map(Typed::Int).ok_or_else(wrong)
                }
                _ => Err(wrong()),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Typed::List),
        (VarKind::Json { .. }, v) => serde_json::to_value(v)
            .map(Typed::Json)
            .map_err(|e| e.to_string()),
        _ => Err(wrong()),
    }
}

/// Checks a typed value against the variable's constraints. Each message
/// starts with the value (or "value" for a secret).
pub(crate) fn check(var: &VarDecl, t: &Typed) -> Vec<(Code, String)> {
    let shown = if var.secret {
        "value".to_string()
    } else {
        t.show()
    };
    let mut out = Vec::new();
    let mut push = |c: Code, m: String| out.push((c, format!("{shown} {m}")));
    match (&var.kind, t) {
        (VarKind::String, Typed::Str(s)) => {
            let n = s.chars().count() as u64;
            if let Some(lo) = var.min_length {
                if n < lo {
                    push(
                        Code::OutOfRange,
                        format!("is {n} characters, below minLength {lo}"),
                    );
                }
            }
            if let Some(hi) = var.max_length {
                if n > hi {
                    push(
                        Code::OutOfRange,
                        format!("is {n} characters, above maxLength {hi}"),
                    );
                }
            }
            if let Some((p, re)) = &var.pattern {
                if !re.is_match(s) {
                    push(Code::PatternMismatch, format!("does not match pattern {p}"));
                }
            }
        }
        (VarKind::Int { .. }, Typed::Int(i)) => {
            if let Some(Typed::Int(lo)) = var.min {
                if *i < lo {
                    push(Code::OutOfRange, format!("is below min {lo}"));
                }
            }
            if let Some(Typed::Int(hi)) = var.max {
                if *i > hi {
                    push(Code::OutOfRange, format!("is above max {hi}"));
                }
            }
        }
        (VarKind::Float, Typed::Float(f)) => {
            if let Some(Typed::Float(lo)) = var.min {
                if *f < lo {
                    push(Code::OutOfRange, format!("is below min {lo}"));
                }
            }
            if let Some(Typed::Float(hi)) = var.max {
                if *f > hi {
                    push(Code::OutOfRange, format!("is above max {hi}"));
                }
            }
        }
        (VarKind::Duration, Typed::Dur(d)) => {
            if let Some(Typed::Dur(lo)) = var.min {
                if *d < lo {
                    push(Code::OutOfRange, format!("is below min {}", format_go(lo)));
                }
            }
            if let Some(Typed::Dur(hi)) = var.max {
                if *d > hi {
                    push(Code::OutOfRange, format!("is above max {}", format_go(hi)));
                }
            }
        }
        (VarKind::Url, Typed::Str(s)) => match url_scheme(s) {
            None => push(
                Code::InvalidType,
                "is not a URL of the form scheme://...".into(),
            ),
            Some(scheme) => {
                if !var.schemes.is_empty() && !var.schemes.iter().any(|x| x == &scheme) {
                    push(
                        Code::InvalidScheme,
                        format!("has scheme {scheme}, not one of {}", var.schemes.join(", ")),
                    );
                }
            }
        },
        (VarKind::Enum(values), Typed::Str(s)) => {
            if !values.iter().any(|v| v == s) {
                push(
                    Code::NotInEnum,
                    format!("is not one of {}", values.join(", ")),
                );
            }
        }
        (VarKind::List(item), Typed::List(items)) => {
            let n = items.len() as u64;
            if let Some(lo) = var.min_items {
                if n < lo {
                    push(
                        Code::TooFewItems,
                        format!("has {n} items, below minItems {lo}"),
                    );
                }
            }
            if let Some(hi) = var.max_items {
                if n > hi {
                    push(
                        Code::TooManyItems,
                        format!("has {n} items, above maxItems {hi}"),
                    );
                }
            }
            if let ItemKind::Int { .. } = item {
                let lo = var.item_min.unwrap_or(i64::MIN);
                let hi = var.item_max.unwrap_or(i64::MAX);
                // One violation per variable: the first item out of bounds.
                if let Some(i) = items.iter().find_map(|x| match x {
                    Typed::Int(i) if *i < lo || *i > hi => Some(*i),
                    _ => None,
                }) {
                    let m = if i < lo {
                        format!("has item {i}, below itemMin {lo}")
                    } else {
                        format!("has item {i}, above itemMax {hi}")
                    };
                    push(Code::OutOfRange, m);
                }
            }
        }
        (VarKind::Json { schema, bind }, Typed::Json(j)) => {
            for msg in crate::schema::validate(schema, j, var.secret) {
                push(Code::SchemaMismatch, msg);
            }
            if out.is_empty() {
                if let Err(e) = bind(j) {
                    let why = if var.secret {
                        String::new()
                    } else {
                        format!(": {e}")
                    };
                    out.push((
                        Code::SchemaMismatch,
                        format!("{shown} does not bind to the app's type{why}"),
                    ));
                }
            }
        }
        (VarKind::Bool, Typed::Bool(_)) => {}
        (k, _) => push(
            Code::InvalidType,
            format!("is not a {} value", k.type_name()),
        ),
    }
    out
}

/// The scheme of `s` when it parses as an absolute URL with `scheme://`.
fn url_scheme(s: &str) -> Option<String> {
    let (scheme, rest) = s.split_once("://")?;
    if rest.is_empty() || rest.chars().any(char::is_whitespace) {
        return None;
    }
    let u = url::Url::parse(s).ok()?;
    (u.scheme().eq_ignore_ascii_case(scheme)).then(|| u.scheme().to_string())
}
