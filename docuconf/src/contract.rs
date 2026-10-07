//! Contract-first mode (SPEC §11.2 item 11): validate an environment
//! against a contract given as JSON, with no Rust declaration, and get the
//! typed values back.
//!
//! The contract is the JSON form of a `contract.cue` (`cue export
//! contract.cue`). Every variable is parsed in the encoding the contract
//! records, and checked by the same code that checks a
//! `#[derive(Docuconf)]` struct, so the two modes accept exactly the same
//! values.
//!
//! ```
//! let contract = docuconf::Contract::from_json(r#"{
//!     "apiVersion": "docuconf.dev/v1alpha1",
//!     "kind": "ConfigContract",
//!     "metadata": {"name": "billing-api"},
//!     "vars": {
//!         "PORT": {"type": "int", "description": "HTTP listen port", "default": 8080, "min": 1, "max": 65535},
//!         "TIMEOUT": {"type": "duration", "description": "Upstream timeout", "encoding": "iso8601", "required": true}
//!     }
//! }"#).unwrap();
//!
//! let values = contract.load_env([("TIMEOUT", "PT1.5S")]).unwrap();
//! assert_eq!(values.get("PORT").and_then(|v| v.as_int()), Some(8080));
//! assert_eq!(values.to_json()["TIMEOUT"], "1s500ms");
//!
//! let err = contract.load_env([("PORT", "0")]).unwrap_err();
//! assert_eq!(err.codes_for("PORT"), [docuconf::Code::OutOfRange]);
//! assert_eq!(err.codes_for("TIMEOUT"), [docuconf::Code::MissingRequired]);
//! ```
//!
//! Only variables are loaded. File inputs and overlays in the contract are
//! ignored; profiles are honoured (the selected profile's defaults apply
//! when a variable is unset).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::time::Duration;

use serde_json::{Map, Value as Json};

use crate::decl::{
    compile_pattern, is_env_name, DurationEncoding, ItemKind, ListEncoding, VarDecl, VarKind,
};
use crate::duration::{format_go, parse_duration};
use crate::env::{self, Env};
use crate::error::{DeclarationError, Error, ValidationError};
use crate::value::{self, Typed};

/// A contract loaded from its JSON form, ready to validate environments.
#[derive(Debug, Clone)]
pub struct Contract {
    name: String,
    vars: Vec<VarDecl>,
    profiles: Option<ContractProfiles>,
}

#[derive(Debug, Clone)]
struct ContractProfiles {
    selector: String,
    default: String,
    defaults: BTreeMap<String, BTreeMap<String, Typed>>,
}

/// One typed value of a contract variable.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// A `string`, `url` or `enum` value.
    String(String),
    /// An `int` value.
    Int(i64),
    /// A `float` value.
    Float(f64),
    /// A `bool` value.
    Bool(bool),
    /// A `duration` value.
    Duration(Duration),
    /// A `list` value: strings or ints.
    List(Vec<Value>),
    /// A `json` value.
    Json(Json),
}

impl Value {
    fn from_typed(t: Typed) -> Value {
        match t {
            Typed::Str(s) => Value::String(s),
            Typed::Int(i) => Value::Int(i),
            Typed::Float(f) => Value::Float(f),
            Typed::Bool(b) => Value::Bool(b),
            Typed::Dur(d) => Value::Duration(d),
            Typed::List(l) => Value::List(l.into_iter().map(Value::from_typed).collect()),
            Typed::Json(j) => Value::Json(j),
        }
    }

    /// The value as JSON: durations in canonical Go form (`1m30s`), lists as
    /// arrays.
    pub fn to_json(&self) -> Json {
        match self {
            Value::String(s) => Json::String(s.clone()),
            Value::Int(i) => (*i).into(),
            Value::Float(f) => serde_json::Number::from_f64(*f)
                .map(Json::Number)
                .unwrap_or(Json::Null),
            Value::Bool(b) => (*b).into(),
            Value::Duration(d) => Json::String(format_go(*d)),
            Value::List(l) => l.iter().map(Value::to_json).collect(),
            Value::Json(j) => j.clone(),
        }
    }

    /// The string, for a `string`, `url` or `enum` value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, for an `int` value.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The number, for a `float` value.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// The boolean, for a `bool` value.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The duration, for a `duration` value.
    pub fn as_duration(&self) -> Option<Duration> {
        match self {
            Value::Duration(d) => Some(*d),
            _ => None,
        }
    }

    /// The items, for a `list` value.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    /// The document, for a `json` value.
    pub fn as_json(&self) -> Option<&Json> {
        match self {
            Value::Json(j) => Some(j),
            _ => None,
        }
    }
}

/// The typed value of every variable in a contract, `None` for an optional
/// variable that is unset and has no default. `Debug` hides secret values.
#[derive(Clone, PartialEq)]
pub struct Values {
    values: BTreeMap<String, Option<Value>>,
    secrets: BTreeSet<String>,
}

impl Values {
    /// The value of a variable, `None` when it is unset or not declared.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name).and_then(Option::as_ref)
    }

    /// Every declared variable and its value, sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&Value>)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_ref()))
    }

    /// Whether the contract marks a variable `secret`.
    pub fn is_secret(&self, name: &str) -> bool {
        self.secrets.contains(name)
    }

    /// Every variable as one JSON object, `null` for unset ones. It holds
    /// secret values: do not log it.
    pub fn to_json(&self) -> Json {
        Json::Object(
            self.values
                .iter()
                .map(|(k, v)| (k.clone(), v.as_ref().map_or(Json::Null, Value::to_json)))
                .collect(),
        )
    }
}

impl fmt::Debug for Values {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut m = f.debug_map();
        for (k, v) in &self.values {
            match v {
                Some(_) if self.secrets.contains(k) => m.entry(k, &"***"),
                v => m.entry(k, v),
            };
        }
        m.finish()
    }
}

/// Bind check for `json` variables: contract-first mode has no Rust type to
/// bind to, so only the JSON Schema applies.
fn bind_any(_: &Json) -> Result<(), String> {
    Ok(())
}

struct Fields<'a> {
    name: &'a str,
    spec: &'a Map<String, Json>,
    problems: Vec<String>,
}

impl<'a> Fields<'a> {
    fn bad(&mut self, msg: String) {
        self.problems.push(format!("{}: {msg}", self.name));
    }

    fn get(&self, k: &str) -> Option<&'a Json> {
        self.spec.get(k).filter(|v| !v.is_null())
    }

    fn str(&mut self, k: &str) -> Option<&'a str> {
        match self.get(k) {
            None => None,
            Some(Json::String(s)) => Some(s),
            Some(_) => {
                self.bad(format!("{k} must be a string"));
                None
            }
        }
    }

    fn bool(&mut self, k: &str) -> bool {
        match self.get(k) {
            None => false,
            Some(Json::Bool(b)) => *b,
            Some(_) => {
                self.bad(format!("{k} must be a boolean"));
                false
            }
        }
    }

    fn uint(&mut self, k: &str) -> Option<u64> {
        let v = self.get(k)?;
        let n = v.as_u64();
        if n.is_none() {
            self.bad(format!("{k} must be a non-negative integer"));
        }
        n
    }

    fn int(&mut self, k: &str) -> Option<i64> {
        let v = self.get(k)?;
        let n = v.as_i64();
        if n.is_none() {
            self.bad(format!("{k} must be a 64-bit integer"));
        }
        n
    }

    fn strs(&mut self, k: &str) -> Vec<String> {
        match self.get(k) {
            None => Vec::new(),
            Some(Json::Array(a)) if a.iter().all(Json::is_string) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            Some(_) => {
                self.bad(format!("{k} must be a list of strings"));
                Vec::new()
            }
        }
    }

    /// A `min`/`max` bound of the variable's own type.
    fn bound(&mut self, k: &str, kind: &VarKind) -> Option<Typed> {
        let v = self.get(k)?;
        let t = match (kind, v) {
            (VarKind::Int { .. }, v) => v.as_i64().map(Typed::Int),
            (VarKind::Float, v) => v.as_f64().map(Typed::Float),
            (VarKind::Duration, Json::String(s)) => parse_duration(s).ok().map(Typed::Dur),
            _ => {
                self.bad(format!(
                    "{k} does not apply to a {} variable",
                    kind.type_name()
                ));
                return None;
            }
        };
        if t.is_none() {
            self.bad(format!("{k} is not a {} value", kind.type_name()));
        }
        t
    }
}

/// Converts a typed JSON value from the contract (a default or a profile
/// default) with the same reader as values from the app's config files.
fn typed(kind: &VarKind, v: &Json) -> Result<Typed, String> {
    let fv = figment::value::Value::serialize(v).map_err(|e| e.to_string())?;
    value::from_figment(kind, &fv)
}

fn var_decl(name: &str, spec: &Json) -> Result<VarDecl, Vec<String>> {
    let Some(spec) = spec.as_object() else {
        return Err(vec![format!("{name}: must be an object")]);
    };
    let mut f = Fields {
        name,
        spec,
        problems: Vec::new(),
    };
    if !is_env_name(name) {
        f.bad("variable name must match ^[A-Z][A-Z0-9_]*$".into());
    }
    let ty = f.str("type").unwrap_or_default();
    let mut list_encoding = ListEncoding::Json;
    let mut duration_encoding = DurationEncoding::Go;
    let kind = match ty {
        "string" => VarKind::String,
        "int" => VarKind::Int {
            min: i64::MIN,
            max: i64::MAX,
        },
        "float" => VarKind::Float,
        "bool" => VarKind::Bool,
        "url" => VarKind::Url,
        "enum" => {
            let values = f.strs("values");
            if values.is_empty() {
                f.bad("an enum needs at least one value".into());
            }
            VarKind::Enum(values)
        }
        "duration" => {
            duration_encoding = match f.str("encoding").unwrap_or("go") {
                "go" => DurationEncoding::Go,
                "iso8601" => DurationEncoding::Iso8601,
                "seconds" => DurationEncoding::Seconds,
                "timespan" => DurationEncoding::Timespan,
                other => {
                    f.bad(format!("unknown duration encoding {other:?}"));
                    DurationEncoding::Go
                }
            };
            VarKind::Duration
        }
        "list" => {
            let item = match f.str("items") {
                Some("string") => ItemKind::String,
                Some("int") => ItemKind::Int {
                    min: i64::MIN,
                    max: i64::MAX,
                },
                other => {
                    f.bad(format!(
                        "list items {other:?} must be \"string\" or \"int\""
                    ));
                    ItemKind::String
                }
            };
            list_encoding = match f.str("encoding").unwrap_or("csv") {
                "csv" => {
                    let sep = f.str("separator").unwrap_or(",");
                    if sep.is_empty() {
                        f.bad("separator must not be empty".into());
                    }
                    ListEncoding::Csv(sep.to_string())
                }
                "json" => ListEncoding::Json,
                "indexed" => ListEncoding::Indexed,
                other => {
                    f.bad(format!("unknown list encoding {other:?}"));
                    ListEncoding::Json
                }
            };
            VarKind::List(item)
        }
        "json" => VarKind::Json {
            schema: f.get("schema").cloned().unwrap_or(Json::Bool(true)),
            bind: bind_any,
        },
        other => {
            f.bad(format!("unknown type {other:?}"));
            VarKind::String
        }
    };
    let (min, max) = match kind {
        VarKind::Int { .. } | VarKind::Float | VarKind::Duration => {
            (f.bound("min", &kind), f.bound("max", &kind))
        }
        _ => (None, None),
    };
    let (item_min, item_max) = match kind {
        VarKind::List(ItemKind::Int { .. }) => (f.int("itemMin"), f.int("itemMax")),
        _ => {
            if f.get("itemMin").is_some() || f.get("itemMax").is_some() {
                f.bad("itemMin and itemMax only apply to a list of ints".into());
            }
            (None, None)
        }
    };
    let (item_min_length, item_max_length) = match kind {
        VarKind::List(ItemKind::String) => (f.uint("itemMinLength"), f.uint("itemMaxLength")),
        _ => {
            if f.get("itemMinLength").is_some() || f.get("itemMaxLength").is_some() {
                f.bad("itemMinLength and itemMaxLength only apply to a list of strings".into());
            }
            (None, None)
        }
    };
    if let (Some(lo), Some(hi)) = (item_min_length, item_max_length) {
        if lo > hi {
            f.bad(format!("itemMinLength {lo} is above itemMaxLength {hi}"));
        }
    }
    let pattern = match f.str("pattern") {
        Some(p) => match compile_pattern(p) {
            Ok(re) => Some((p.to_string(), re)),
            Err(e) => {
                f.bad(e);
                None
            }
        },
        None => None,
    };
    let (deprecated, replaced_by) = match f.get("deprecated") {
        Some(Json::Object(d)) => (
            d.get("message").and_then(Json::as_str).map(str::to_string),
            d.get("replacedBy")
                .and_then(Json::as_str)
                .map(str::to_string),
        ),
        _ => (None, None),
    };
    let description = f.str("description").unwrap_or_default().to_string();
    let required = f.bool("required");
    let secret = f.bool("secret");
    let mut decl = VarDecl {
        name: name.to_string(),
        key: vec![name.to_string()],
        field: name.to_string(),
        kind,
        description,
        required,
        secret,
        default: None,
        min,
        max,
        min_length: f.uint("minLength"),
        max_length: f.uint("maxLength"),
        pattern,
        schemes: f.strs("schemes"),
        min_items: f.uint("minItems"),
        max_items: f.uint("maxItems"),
        item_min,
        item_max,
        item_min_length,
        item_max_length,
        list_encoding,
        duration_encoding,
        group: f.str("group").map(str::to_string),
        examples: f.strs("examples"),
        deprecated,
        replaced_by,
        config_key: f.str("configKey").map(str::to_string),
    };
    if let Some(d) = f.get("default") {
        if decl.required {
            f.bad("a required variable must not have a default".into());
        }
        match typed(&decl.kind, d) {
            Ok(t) => {
                for (_, msg) in value::check(&decl, &t) {
                    f.bad(format!("default {msg}"));
                }
                decl.default = Some(t);
            }
            Err(e) => f.bad(format!("default {e}")),
        }
    }
    if f.problems.is_empty() {
        Ok(decl)
    } else {
        Err(f.problems)
    }
}

impl Contract {
    /// Reads a contract from its JSON form (`cue export contract.cue`).
    pub fn from_json(json: &str) -> Result<Contract, DeclarationError> {
        let v: Json = serde_json::from_str(json).map_err(|e| DeclarationError {
            problems: vec![format!("contract is not valid JSON: {e}")],
        })?;
        Contract::from_value(&v)
    }

    /// Reads a contract from a parsed JSON value.
    pub fn from_value(v: &Json) -> Result<Contract, DeclarationError> {
        let err = |p: String| DeclarationError { problems: vec![p] };
        let top = v
            .as_object()
            .ok_or_else(|| err("contract must be a JSON object".into()))?;
        if top.get("kind").and_then(Json::as_str) != Some("ConfigContract") {
            return Err(err("contract kind must be \"ConfigContract\"".into()));
        }
        match top.get("apiVersion").and_then(Json::as_str) {
            Some("docuconf.dev/v1alpha1") => {}
            other => {
                return Err(err(format!(
                    "contract apiVersion {other:?} is not supported; this SDK reads \"docuconf.dev/v1alpha1\""
                )))
            }
        }
        let name = top
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        let mut problems = Vec::new();
        let mut vars = Vec::new();
        match top.get("vars") {
            None | Some(Json::Null) => {}
            Some(Json::Object(m)) => {
                for (k, spec) in m {
                    match var_decl(k, spec) {
                        Ok(d) => vars.push(d),
                        Err(p) => problems.extend(p),
                    }
                }
            }
            Some(_) => problems.push("vars must be an object".into()),
        }
        vars.sort_by(|a, b| a.name.cmp(&b.name));

        let profiles = match top.get("profiles") {
            Some(Json::Object(p)) => {
                let selector = p.get("selector").and_then(Json::as_str).unwrap_or_default();
                let default = p.get("default").and_then(Json::as_str).unwrap_or_default();
                if !vars.iter().any(|v| v.name == selector) {
                    problems.push(format!(
                        "profile selector {selector:?} must be a declared variable"
                    ));
                }
                let mut defaults = BTreeMap::new();
                if let Some(Json::Object(d)) = p.get("defaults") {
                    for (profile, vals) in d {
                        let mut out = BTreeMap::new();
                        for (k, val) in vals.as_object().into_iter().flatten() {
                            let Some(var) = vars.iter().find(|v| &v.name == k) else {
                                problems.push(format!(
                                    "profile {profile}: {k} is not a declared variable"
                                ));
                                continue;
                            };
                            match typed(&var.kind, val) {
                                Ok(t) => {
                                    for (_, msg) in value::check(var, &t) {
                                        problems.push(format!("profile {profile}: {k}: {msg}"));
                                    }
                                    out.insert(k.clone(), t);
                                }
                                Err(e) => problems.push(format!("profile {profile}: {k}: {e}")),
                            }
                        }
                        defaults.insert(profile.clone(), out);
                    }
                }
                Some(ContractProfiles {
                    selector: selector.to_string(),
                    default: default.to_string(),
                    defaults,
                })
            }
            _ => None,
        };
        if !problems.is_empty() {
            return Err(DeclarationError { problems });
        }
        Ok(Contract {
            name,
            vars,
            profiles,
        })
    }

    /// The contract's `metadata.name`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Validates the process environment against the contract. On failure,
    /// every violation is reported together and written to the termination
    /// log (`/dev/termination-log` or `DOCUCONF_TERMINATION_LOG`), as
    /// [`Loader::load`](crate::Loader::load) does.
    pub fn load(&self) -> Result<Values, Error> {
        let env = Env::process();
        for w in env::typo_hints(&self.vars, "", &[], &env) {
            eprintln!("docuconf: {w}");
        }
        for var in &self.vars {
            if let Some(w) = env::deprecation(var, &env) {
                eprintln!("docuconf: {w}");
            }
        }
        self.load_from(&env).map_err(|e| {
            crate::load::write_termination_log(&env.vars, &e);
            Error::Validation(e)
        })
    }

    /// Validates the given variables, as the whole environment, against the
    /// contract. Nothing is written to the termination log.
    pub fn load_env<I, K, V>(&self, vars: I) -> Result<Values, ValidationError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let vars: HashMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self.load_from(&Env::from_map(vars))
    }

    fn load_from(&self, env: &Env) -> Result<Values, ValidationError> {
        let profile_defaults = self.profiles.as_ref().and_then(|p| {
            let selected = env
                .get(&p.selector)
                .filter(|s| !s.is_empty())
                .unwrap_or(&p.default);
            p.defaults.get(selected)
        });
        let mut violations = Vec::new();
        let mut values = BTreeMap::new();
        for var in &self.vars {
            let mut from_env = false;
            let found = match env::read(var, env) {
                Err(v) => {
                    violations.push(v);
                    continue;
                }
                Ok(Some(t)) => {
                    from_env = true;
                    Some(t)
                }
                Ok(None) => profile_defaults
                    .and_then(|d| d.get(&var.name))
                    .or(var.default.as_ref())
                    .cloned(),
            };
            match found {
                None if var.required => violations.push(env::missing(var)),
                None => {
                    values.insert(var.name.clone(), None);
                }
                Some(t) => match env::finish(var, t, from_env) {
                    Ok(t) => {
                        values.insert(var.name.clone(), Some(Value::from_typed(t)));
                    }
                    Err(v) => violations.extend(v),
                },
            }
        }
        if !violations.is_empty() {
            return Err(ValidationError { violations });
        }
        Ok(Values {
            values,
            secrets: self
                .vars
                .iter()
                .filter(|v| v.secret)
                .map(|v| v.name.clone())
                .collect(),
        })
    }
}
