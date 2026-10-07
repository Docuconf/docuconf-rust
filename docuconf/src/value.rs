//! Typed values: parsing the wire form (SPEC §5), reading values from
//! figment layers, and checking constraints.

use std::time::Duration;

use figment::value::{Num, Value};

use crate::__private::Lit;
use crate::decl::{ItemKind, ListEncoding, VarDecl, VarKind};
use crate::duration::{format_go, parse_duration, parse_encoded};
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

/// A parse failure: `invalid_type`, or `out_of_range` for an integer
/// outside the 64-bit range (SPEC §5). The message never holds the value.
pub(crate) type ParseError = (Code, String);

/// Parses the raw environment string for a variable in its wire encoding
/// (SPEC §5): values are never trimmed, booleans are `true`/`false` in any
/// case, lists and durations follow the variable's encoding (`json` lists
/// and `go` durations for a figment-bound struct, whatever the contract
/// says in contract-first mode). An `indexed` list spans several
/// variables; see [`parse_items`].
pub(crate) fn parse_wire(var: &VarDecl, raw: &str) -> Result<Typed, ParseError> {
    let invalid = |m: &str| (Code::InvalidType, m.to_string());
    match &var.kind {
        VarKind::String | VarKind::Url | VarKind::Enum(_) => Ok(Typed::Str(raw.to_string())),
        VarKind::Int { .. } => parse_int(raw).map(Typed::Int),
        VarKind::Float => match raw.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Typed::Float(f)),
            _ => Err(invalid("is not a finite number")),
        },
        VarKind::Bool => {
            if raw.eq_ignore_ascii_case("true") {
                Ok(Typed::Bool(true))
            } else if raw.eq_ignore_ascii_case("false") {
                Ok(Typed::Bool(false))
            } else {
                Err(invalid("is not true or false"))
            }
        }
        VarKind::Duration => parse_encoded(var.duration_encoding, raw)
            .map(Typed::Dur)
            .map_err(|e| (Code::InvalidType, e)),
        VarKind::List(item) => match &var.list_encoding {
            ListEncoding::Json => {
                let arr: Vec<serde_json::Value> = serde_json::from_str(raw)
                    .map_err(|_| invalid("is not a JSON array such as [\"a\",\"b\"]"))?;
                arr.iter()
                    .enumerate()
                    .map(|(i, x)| json_item(*item, i, x))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Typed::List)
            }
            ListEncoding::Csv(sep) => parse_items(*item, raw.split(sep.as_str())),
            ListEncoding::Indexed => parse_items(*item, [raw]),
        },
        VarKind::Json { .. } => {
            let doc = serde_json::from_str(raw).map_err(|_| invalid("is not valid JSON"))?;
            // maxLength bounds the value as received, whitespace included,
            // not re-encoded: that is what a fixed-width field has to hold.
            if let Some(hi) = var.max_length {
                let n = raw.chars().count() as u64;
                if n > hi {
                    return Err((
                        Code::OutOfRange,
                        format!("is {n} characters of JSON, above maxLength {hi}"),
                    ));
                }
            }
            Ok(Typed::Json(doc))
        }
    }
}

/// Parses list items given as separate strings (a split `csv` value, or the
/// variables of an `indexed` list).
pub(crate) fn parse_items<'a>(
    item: ItemKind,
    raws: impl IntoIterator<Item = &'a str>,
) -> Result<Typed, ParseError> {
    raws.into_iter()
        .enumerate()
        .map(|(i, raw)| match item {
            ItemKind::String => Ok(Typed::Str(raw.to_string())),
            ItemKind::Int { .. } => parse_int(raw)
                .map(Typed::Int)
                .map_err(|(c, m)| (c, format!("has item {i} that {m}"))),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Typed::List)
}

/// A base-10 integer: `invalid_type` when it is not an integer at all,
/// `out_of_range` when it is one outside the 64-bit range (SPEC §5).
fn parse_int(raw: &str) -> Result<i64, ParseError> {
    raw.parse::<i64>().map_err(|_| {
        let digits = raw.strip_prefix(['-', '+']).unwrap_or(raw);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            (
                Code::OutOfRange,
                "is outside the 64-bit integer range".to_string(),
            )
        } else {
            (Code::InvalidType, "is not a 64-bit integer".to_string())
        }
    })
}

fn json_item(item: ItemKind, i: usize, x: &serde_json::Value) -> Result<Typed, ParseError> {
    match item {
        ItemKind::String => x
            .as_str()
            .map(|s| Typed::Str(s.to_string()))
            .ok_or_else(|| {
                (
                    Code::InvalidType,
                    format!("has item {i} that is not a string"),
                )
            }),
        ItemKind::Int { .. } => {
            if let Some(n) = x.as_i64() {
                return Ok(Typed::Int(n));
            }
            // An integer too large for 64 bits (serde_json reads it as u64
            // or as a whole f64) is out of range rather than mistyped.
            let whole_beyond = x.is_u64()
                || x.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.abs() >= 9.2e18);
            Err(if whole_beyond {
                (
                    Code::OutOfRange,
                    format!("has item {i} that is outside the 64-bit integer range"),
                )
            } else {
                (
                    Code::InvalidType,
                    format!("has item {i} that is not a 64-bit integer"),
                )
            })
        }
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
/// starts with the value (or "value" for a secret). A `json` value's
/// `maxLength` is measured on its compact JSON.
pub(crate) fn check(var: &VarDecl, t: &Typed) -> Vec<(Code, String)> {
    check_value(var, t, false)
}

/// As [`check`]. `from_wire` is true for a value parsed from the
/// environment, whose `json` length [`parse_wire`] already measured as
/// received.
pub(crate) fn check_value(var: &VarDecl, t: &Typed, from_wire: bool) -> Vec<(Code, String)> {
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
                } else if let Some(hi) = var.max_length {
                    let n = s.chars().count() as u64;
                    if n > hi {
                        push(
                            Code::OutOfRange,
                            format!("is {n} characters, above maxLength {hi}"),
                        );
                    }
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
            if let ItemKind::String = item {
                // One violation per variable: the first item out of bounds.
                let lo = var.item_min_length.unwrap_or(0);
                let hi = var.item_max_length.unwrap_or(u64::MAX);
                if let Some((i, s, n)) = items.iter().enumerate().find_map(|(i, x)| match x {
                    Typed::Str(s) => {
                        let n = s.chars().count() as u64;
                        (n < lo || n > hi).then_some((i, s, n))
                    }
                    _ => None,
                }) {
                    let item = if var.secret {
                        String::new()
                    } else {
                        format!(" {}", Typed::Str(s.clone()).show())
                    };
                    let m = if n < lo {
                        format!("has item {i}{item} of {n} characters, below itemMinLength {lo}")
                    } else {
                        format!("has item {i}{item} of {n} characters, above itemMaxLength {hi}")
                    };
                    push(Code::OutOfRange, m);
                }
            }
        }
        (VarKind::Json { schema, bind }, Typed::Json(j)) => {
            if let (Some(hi), false) = (var.max_length, from_wire) {
                // The compact JSON the platform renders: no insignificant
                // whitespace and no escaping beyond what JSON requires.
                let n = serde_json::to_string(j).unwrap_or_default().chars().count() as u64;
                if n > hi {
                    push(
                        Code::OutOfRange,
                        format!("is {n} characters of JSON, above maxLength {hi}"),
                    );
                }
            }
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
