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
//!         "PORT": {"type": "int", "description": "HTTP listen port", "details": "Behind the mesh, keep the default.", "default": 8080, "min": 1, "max": 65535},
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
//! The whole contract is loaded, as an app with config files would load
//! it (SPEC §4.4, §4.6, §4.7): every variable is layered from its default,
//! then the selected profile's default, then a config-file overlay, then
//! the environment; and every file input is read and checked, from under
//! `DOCUCONF_FILE_ROOT` when that is set. Each file is read once: to pick
//! up a change to an input declared `reload: watch`, load again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value as Json};

use crate::decl::{
    check_deprecated, compile_pattern, is_abs_path, is_env_name, is_input_name, key_set_bounds,
    DurationEncoding, FileDecl, FileKind, ItemKind, ListEncoding, VarDecl, VarKind,
};
use crate::duration::{format_go, parse_duration, parse_go};
use crate::env::{self, Env};
use crate::error::{Code, DeclarationError, Error, ValidationError, Violation};
use crate::files::{self, FileCx, Outcome};
use crate::types::{BinaryFile, ConfigDoc, KeySet, TextFile};
use crate::value::{self, Typed};

/// A contract loaded from its JSON form, ready to validate environments.
#[derive(Debug, Clone)]
pub struct Contract {
    name: String,
    vars: Vec<VarDecl>,
    files: Vec<FileDecl>,
    profiles: Option<ContractProfiles>,
    overlays: Vec<ContractOverlay>,
}

/// One of a contract's config-file overlays (SPEC §4.7).
#[derive(Debug, Clone)]
struct ContractOverlay {
    name: String,
    format: String,
    path: String,
    key_separator: String,
}

/// A variable's value from an overlay: the wire string (or items) it
/// stands for, or `None` when the overlay value was already reported.
enum OverlayValue {
    Raw(String),
    Items(Vec<String>),
    Bad,
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
    /// A `duration` value that is zero or positive.
    Duration(Duration),
    /// A negative `duration` value (the `go` encoding takes a sign, SPEC
    /// §5), as its magnitude: `-1m30s` is `NegativeDuration(90s)`.
    NegativeDuration(Duration),
    /// A `list` value: strings or ints.
    List(Vec<Value>),
    /// A `keySet` value: the keys, in order.
    KeySet(KeySet),
    /// A `json` value.
    Json(Json),
}

impl Value {
    fn from_typed(kind: &VarKind, t: Typed) -> Value {
        match t {
            Typed::Str(s) => Value::String(s),
            Typed::Int(i) => Value::Int(i),
            Typed::Float(f) => Value::Float(f),
            Typed::Bool(b) => Value::Bool(b),
            Typed::Dur(d) => {
                let abs = d.unsigned_abs();
                let abs = Duration::new((abs / 1_000_000_000) as u64, (abs % 1_000_000_000) as u32);
                if d < 0 {
                    Value::NegativeDuration(abs)
                } else {
                    Value::Duration(abs)
                }
            }
            Typed::List(l) if matches!(kind, VarKind::KeySet) => {
                Value::KeySet(KeySet::new(l.into_iter().filter_map(|k| match k {
                    Typed::Str(s) => Some(s),
                    _ => None,
                })))
            }
            Typed::List(l) => Value::List(
                l.into_iter()
                    .map(|x| Value::from_typed(&VarKind::String, x))
                    .collect(),
            ),
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
            Value::NegativeDuration(d) => Json::String(format!("-{}", format_go(*d))),
            Value::List(l) => l.iter().map(Value::to_json).collect(),
            Value::KeySet(k) => k.keys().iter().map(|s| Json::String(s.clone())).collect(),
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

    /// The duration, for a `duration` value that is not negative.
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

    /// The keys, for a `keySet` value.
    pub fn as_key_set(&self) -> Option<&KeySet> {
        match self {
            Value::KeySet(k) => Some(k),
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

/// A file input loaded in contract-first mode, checked as SPEC §11.2 item 7
/// says.
#[derive(Debug)]
#[non_exhaustive]
pub enum FileValue {
    /// A `config` file: its path and its data, checked against the
    /// contract's schema.
    Config {
        /// The file the data was read from.
        path: PathBuf,
        /// The data, as JSON.
        data: Json,
    },
    /// A `text` file.
    Text(TextFile),
    /// A `binary` file.
    Binary(BinaryFile),
    /// A `tls` key pair.
    #[cfg(feature = "tls")]
    Tls(crate::TlsKeyPair),
    /// A `caBundle`.
    #[cfg(feature = "tls")]
    CaBundle(crate::CaBundle),
    /// A `keystore`.
    #[cfg(feature = "keystore")]
    Keystore(crate::Keystore),
}

impl FileValue {
    fn from_loaded(b: Box<dyn std::any::Any + Send>) -> Option<FileValue> {
        let b = match b.downcast::<ConfigDoc>() {
            Ok(c) => {
                let c = *c;
                return Some(FileValue::Config {
                    path: c.path,
                    data: c.doc,
                });
            }
            Err(b) => b,
        };
        let b = match b.downcast::<TextFile>() {
            Ok(t) => return Some(FileValue::Text(*t)),
            Err(b) => b,
        };
        let b = match b.downcast::<BinaryFile>() {
            Ok(t) => return Some(FileValue::Binary(*t)),
            Err(b) => b,
        };
        #[cfg(feature = "tls")]
        let b = match b.downcast::<crate::TlsKeyPair>() {
            Ok(t) => return Some(FileValue::Tls(*t)),
            Err(b) => b,
        };
        #[cfg(feature = "tls")]
        let b = match b.downcast::<crate::CaBundle>() {
            Ok(t) => return Some(FileValue::CaBundle(*t)),
            Err(b) => b,
        };
        #[cfg(feature = "keystore")]
        let b = match b.downcast::<crate::Keystore>() {
            Ok(t) => return Some(FileValue::Keystore(*t)),
            Err(b) => b,
        };
        drop(b);
        None
    }

    /// The file as conformance JSON (SPEC §12): a config file's data, a
    /// text file's text, and `true` for any other file.
    pub fn to_json(&self) -> Json {
        match self {
            FileValue::Config { data, .. } => data.clone(),
            FileValue::Text(t) => Json::String(t.text().to_string()),
            #[allow(unreachable_patterns)]
            _ => Json::Bool(true),
        }
    }
}

/// The typed value of every variable in a contract, `None` for an optional
/// variable that is unset and has no default, and every file input, `None`
/// for an optional one that is absent. `Debug` hides secret values.
#[derive(Clone)]
pub struct Values {
    values: BTreeMap<String, Option<Value>>,
    files: BTreeMap<String, Option<Arc<FileValue>>>,
    secrets: BTreeSet<String>,
}

/// Values are equal when their variables are, and their files hold the
/// same data (compared as [`FileValue::to_json`]).
impl PartialEq for Values {
    fn eq(&self, other: &Self) -> bool {
        let files = |v: &Values| -> BTreeMap<String, Json> {
            v.files
                .iter()
                .map(|(k, f)| (k.clone(), f.as_ref().map_or(Json::Null, |f| f.to_json())))
                .collect()
        };
        self.values == other.values && self.secrets == other.secrets && files(self) == files(other)
    }
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

    /// A file input, `None` when it is absent or not declared.
    pub fn file(&self, name: &str) -> Option<&FileValue> {
        self.files.get(name).and_then(|f| f.as_deref())
    }

    /// Every declared file input, sorted by name.
    pub fn files(&self) -> impl Iterator<Item = (&str, Option<&FileValue>)> {
        self.files.iter().map(|(k, v)| (k.as_str(), v.as_deref()))
    }

    /// Whether the contract marks a variable or file input `secret`.
    pub fn is_secret(&self, name: &str) -> bool {
        self.secrets.contains(name)
    }

    /// Every variable and file input as one JSON object, `null` for unset
    /// ones (SPEC §12): a key set is an array of its keys, a config file
    /// its data, a text file its text, any other file `true`. It holds
    /// secret values: do not log it.
    pub fn to_json(&self) -> Json {
        let mut m: Map<String, Json> = self
            .values
            .iter()
            .map(|(k, v)| (k.clone(), v.as_ref().map_or(Json::Null, Value::to_json)))
            .collect();
        for (k, f) in &self.files {
            m.insert(k.clone(), f.as_ref().map_or(Json::Null, |f| f.to_json()));
        }
        Json::Object(m)
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
        for (k, v) in &self.files {
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
            (VarKind::Duration, Json::String(s)) => parse_go(s).ok().map(Typed::Dur),
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

/// An input's `deprecated` notice (SPEC §4.2), checked.
fn deprecation(f: &mut Fields) -> (Option<String>, Option<String>) {
    match f.get("deprecated") {
        None => (None, None),
        Some(Json::Object(d)) => {
            let msg = d.get("message").and_then(Json::as_str).unwrap_or_default();
            if let Some(p) = check_deprecated(msg) {
                f.bad(p);
            }
            (
                Some(msg.to_string()),
                d.get("replacedBy")
                    .and_then(Json::as_str)
                    .map(str::to_string),
            )
        }
        Some(_) => {
            f.bad("deprecated must be an object with a message".into());
            (None, None)
        }
    }
}

fn file_decl(name: &str, spec: &Json) -> Result<FileDecl, Vec<String>> {
    let Some(spec) = spec.as_object() else {
        return Err(vec![format!("file input {name}: must be an object")]);
    };
    let label = format!("file input {name}");
    let mut f = Fields {
        name: &label,
        spec,
        problems: Vec::new(),
    };
    if !is_input_name(name) {
        f.bad("name must be a DNS label (^[a-z]([-a-z0-9]{0,40}[a-z0-9])?$)".into());
    }
    let ty = f.str("type").unwrap_or_default();
    let kind = match ty {
        "config" => FileKind::Config {
            schema: f.get("schema").cloned().unwrap_or(Json::Bool(true)),
            bind: bind_any,
        },
        "tls" => FileKind::Tls,
        "caBundle" => FileKind::CaBundle,
        "keystore" => FileKind::Keystore,
        "text" => FileKind::Text,
        "binary" => FileKind::Binary,
        other => {
            f.bad(format!("unknown file type {other:?}"));
            FileKind::Binary
        }
    };
    #[cfg(not(feature = "tls"))]
    if matches!(kind, FileKind::Tls | FileKind::CaBundle) {
        f.bad(format!(
            "a {ty} file input needs docuconf's `tls` cargo feature"
        ));
    }
    #[cfg(not(feature = "keystore"))]
    if matches!(kind, FileKind::Keystore) {
        f.bad("a keystore file input needs docuconf's `keystore` cargo feature".into());
    }
    let format = match &kind {
        FileKind::Config { .. } => match f.str("format") {
            Some(x @ ("json" | "yaml" | "toml")) => Some(x.to_string()),
            other => {
                f.bad(format!(
                    "format {other:?} is not a config file format (json, yaml or toml)"
                ));
                None
            }
        },
        FileKind::Keystore => match f.str("format").unwrap_or("pkcs12") {
            "pkcs12" => Some("pkcs12".to_string()),
            other => {
                f.bad(format!(
                    "keystore format {other:?} is not supported; this SDK reads pkcs12"
                ));
                None
            }
        },
        _ => None,
    };
    let path = f.str("path").unwrap_or_default().to_string();
    if !is_abs_path(&path) {
        f.bad(format!("path {path:?} must be absolute and normalised"));
    }
    let path_env = f.str("pathEnv").map(str::to_string);
    if let Some(pe) = &path_env {
        if !is_env_name(pe) {
            f.bad(format!("pathEnv {pe:?} is not a variable name"));
        }
    }
    match f.str("reload") {
        None | Some("restart" | "watch") => {}
        Some(other) => f.bad(format!("reload {other:?} must be restart or watch")),
    }
    let max_size = f.uint("maxSize");
    let description = f.str("description").unwrap_or_default().to_string();
    let details = f.str("details").map(str::to_string);
    if let Some(p) = crate::decl::check_details(details.as_deref()) {
        f.bad(p);
    }
    let required = f.bool("required");
    let secret = f.bool("secret") || matches!(kind, FileKind::Tls | FileKind::Keystore);
    let (deprecated, replaced_by) = deprecation(&mut f);
    if deprecated.is_some() && required {
        f.bad("a required file input cannot be deprecated".into());
    }
    let min_remaining = match f.str("minRemaining") {
        Some(s) => match parse_duration(s) {
            Ok(d) => Some(d),
            Err(e) => {
                f.bad(format!("minRemaining {e}"));
                None
            }
        },
        None => None,
    };
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
    let decl = FileDecl {
        name: name.to_string(),
        key: vec![name.to_string()],
        field: name.to_string(),
        kind,
        description,
        details,
        required,
        secret,
        path,
        path_env,
        max_size,
        group: f.str("group").map(str::to_string),
        deprecated,
        replaced_by,
        format,
        dns_names: f.strs("dnsNames"),
        key_algorithms: f.strs("keyAlgorithms"),
        min_remaining,
        require_ca: f.bool("requireCA"),
        min_certificates: f.uint("minCertificates").unwrap_or(1),
        password_var: f.str("passwordVar").map(str::to_string),
        pattern,
        min_length: f.uint("minLength"),
        max_length: f.uint("maxLength"),
    };
    if f.problems.is_empty() {
        Ok(decl)
    } else {
        Err(f.problems)
    }
}

fn overlay_decl(name: &str, spec: &Json) -> Result<ContractOverlay, Vec<String>> {
    let Some(spec) = spec.as_object() else {
        return Err(vec![format!("overlay {name}: must be an object")]);
    };
    let label = format!("overlay {name}");
    let mut f = Fields {
        name: &label,
        spec,
        problems: Vec::new(),
    };
    if !is_input_name(name) {
        f.bad("name must be a DNS label".into());
    }
    let format = f.str("format").unwrap_or_default().to_string();
    if !["json", "yaml", "toml"].contains(&format.as_str()) {
        f.bad("format must be json, yaml or toml".into());
    }
    let path = f.str("path").unwrap_or_default().to_string();
    if !is_abs_path(&path) {
        f.bad(format!("path {path:?} must be absolute and normalised"));
    }
    let key_separator = f.str("keySeparator").unwrap_or(".").to_string();
    if key_separator != ":" && key_separator != "." {
        f.bad("keySeparator must be \":\" or \".\"".into());
    }
    match f.str("reload") {
        None | Some("restart" | "watch") => {}
        Some(other) => f.bad(format!("reload {other:?} must be restart or watch")),
    }
    if f.problems.is_empty() {
        Ok(ContractOverlay {
            name: name.to_string(),
            format,
            path,
            key_separator,
        })
    } else {
        Err(f.problems)
    }
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
        "keySet" => {
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
                    f.bad(format!("unknown key set encoding {other:?}"));
                    ListEncoding::Json
                }
            };
            VarKind::KeySet
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
    let (deprecated, replaced_by) = deprecation(&mut f);
    let description = f.str("description").unwrap_or_default().to_string();
    // Docs only: checked as SPEC §4.2 says, then never read.
    let details = f.str("details").map(str::to_string);
    if let Some(p) = crate::decl::check_details(details.as_deref()) {
        f.bad(p);
    }
    let required = f.bool("required");
    let mut secret = f.bool("secret");
    if deprecated.is_some() && required {
        f.bad("a required variable cannot be deprecated".into());
    }
    let (mut min_items, mut max_items) = (f.uint("minItems"), f.uint("maxItems"));
    let (mut key_min, mut key_max) = (item_min_length, item_max_length);
    if matches!(kind, VarKind::KeySet) {
        // A key set is always secret (SPEC §4.3).
        if f.get("secret").is_some() && !secret {
            f.bad("a keySet is always secret".into());
        }
        secret = true;
        let (min_keys, max_keys) = (f.uint("minKeys"), f.uint("maxKeys"));
        (key_min, key_max) = (f.uint("keyMinLength"), f.uint("keyMaxLength"));
        let mut problems = Vec::new();
        let (lo, hi) = key_set_bounds(
            min_keys,
            max_keys,
            key_min,
            key_max,
            &mut problems,
            ["minKeys", "maxKeys", "keyMinLength", "keyMaxLength"],
        );
        for p in problems {
            f.bad(p);
        }
        (min_items, max_items) = (Some(lo), Some(hi));
    }
    let mut decl = VarDecl {
        name: name.to_string(),
        key: vec![name.to_string()],
        field: name.to_string(),
        kind,
        description,
        details,
        required,
        secret,
        default: None,
        min,
        max,
        min_length: f.uint("minLength"),
        max_length: f.uint("maxLength"),
        pattern,
        schemes: f.strs("schemes"),
        min_items,
        max_items,
        item_min,
        item_max,
        item_min_length: key_min,
        item_max_length: key_max,
        list_encoding,
        duration_encoding,
        negative_ok: true,
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
        let mut files = Vec::new();
        match top.get("files") {
            None | Some(Json::Null) => {}
            Some(Json::Object(m)) => {
                for (k, spec) in m {
                    match file_decl(k, spec) {
                        Ok(d) => files.push(d),
                        Err(p) => problems.extend(p),
                    }
                }
            }
            Some(_) => problems.push("files must be an object".into()),
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &files {
            if let Some(pv) = &f.password_var {
                if !vars.iter().any(|v| &v.name == pv) {
                    problems.push(format!(
                        "file input {}: passwordVar {pv} is not a declared variable",
                        f.name
                    ));
                }
            }
        }
        let mut overlays = Vec::new();
        match top.get("overlays") {
            None | Some(Json::Null) => {}
            Some(Json::Object(m)) => {
                for (k, spec) in m {
                    match overlay_decl(k, spec) {
                        Ok(o) => overlays.push(o),
                        Err(p) => problems.extend(p),
                    }
                }
            }
            Some(_) => problems.push("overlays must be an object".into()),
        }
        overlays.sort_by(|a, b| a.name.cmp(&b.name));

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
                            if var.secret {
                                problems.push(format!(
                                    "profile {profile}: {k} is secret, and a secret has no value in a config file"
                                ));
                                continue;
                            }
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
            files,
            profiles,
            overlays,
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
        for w in env::typo_hints(&self.vars, "", &self.path_envs(), &env) {
            eprintln!("docuconf: {w}");
        }
        let mut warnings = Vec::new();
        let out = self.load_from(&env, &mut warnings);
        for w in warnings {
            eprintln!("docuconf: {w}");
        }
        out.map_err(|e| {
            crate::load::write_termination_log(&env.vars, &e);
            Error::Validation(e)
        })
    }

    /// Validates the given variables, as the whole environment, against the
    /// contract. Put `DOCUCONF_FILE_ROOT` among them to read file inputs and
    /// overlays from under a test directory. Nothing is written to the
    /// termination log, and warnings are dropped; see
    /// [`load_env_with_warnings`](Contract::load_env_with_warnings).
    pub fn load_env<I, K, V>(&self, vars: I) -> Result<Values, ValidationError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.load_env_with_warnings(vars).0
    }

    /// As [`load_env`](Contract::load_env), also returning the warnings
    /// [`load`](Contract::load) prints: a deprecated input that is set
    /// (naming the input and its message, never the value), or a variable
    /// set both in the environment and in an overlay.
    pub fn load_env_with_warnings<I, K, V>(
        &self,
        vars: I,
    ) -> (Result<Values, ValidationError>, Vec<String>)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let vars: HashMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let mut warnings = Vec::new();
        let out = self.load_from(&Env::from_map(vars), &mut warnings);
        (out, warnings)
    }

    fn path_envs(&self) -> Vec<String> {
        self.files
            .iter()
            .filter_map(|f| f.path_env.clone())
            .collect()
    }

    /// The profile in effect (SPEC §4.4): the selector's value when the
    /// environment sets it, as `#Validate` reads it (for a `string` selector
    /// the empty string is a value), or `profiles.default`.
    fn selected_profile<'a>(&'a self, p: &'a ContractProfiles, env: &'a Env) -> &'a str {
        let string = self
            .vars
            .iter()
            .find(|v| v.name == p.selector)
            .is_some_and(|v| matches!(v.kind, VarKind::String));
        match env.get(&p.selector) {
            Some(raw) if !raw.is_empty() || string => raw,
            _ => &p.default,
        }
    }

    /// Reads the overlays (SPEC §4.7): the value each one gives a variable,
    /// by variable name, the first overlay by name winning, and the
    /// violations of the overlay files and their values.
    fn overlay_values(
        &self,
        root: Option<&Path>,
        viols: &mut Vec<Violation>,
        warnings: &mut Vec<String>,
    ) -> HashMap<String, OverlayValue> {
        let mut out: HashMap<String, OverlayValue> = HashMap::new();
        let mut from: HashMap<String, String> = HashMap::new();
        let selector = self.profiles.as_ref().map(|p| p.selector.as_str());
        for o in &self.overlays {
            let path = match root {
                Some(r) => r.join(o.path.trim_start_matches('/')),
                None => PathBuf::from(&o.path),
            };
            let fail = |code, message| Violation {
                input: o.name.clone(),
                code,
                message,
            };
            let doc = match read_overlay(&path, &o.format) {
                Ok(None) => continue,
                Ok(Some(doc)) => doc,
                Err((code, msg)) => {
                    viols.push(fail(code, msg));
                    continue;
                }
            };
            for v in &self.vars {
                let Some(key) = &v.config_key else { continue };
                if Some(v.name.as_str()) == selector {
                    continue;
                }
                let found = key
                    .split(o.key_separator.as_str())
                    .try_fold(&doc, |cur, k| cur.as_object()?.get(k));
                let Some(val) = found.filter(|x| !x.is_null()) else {
                    continue;
                };
                if let Some(first) = from.get(&v.name) {
                    warnings.push(format!(
                        "{} is set in overlays {first} and {}; the first wins",
                        v.name, o.name
                    ));
                    continue;
                }
                from.insert(v.name.clone(), o.name.clone());
                let bad = |message: String| Violation {
                    input: v.name.clone(),
                    code: Code::InvalidType,
                    message,
                };
                if v.secret {
                    // Never print it: it is secret material in a ConfigMap.
                    viols.push(bad(format!(
                        "is secret, but overlay {} sets it at {key}; supply secrets through the environment",
                        o.name
                    )));
                    out.insert(v.name.clone(), OverlayValue::Bad);
                    continue;
                }
                match overlay_value(&v.kind, val) {
                    Ok(x) => {
                        out.insert(v.name.clone(), x);
                    }
                    Err(e) => {
                        viols.push(bad(format!("overlay {}, at {key}: {e}", o.name)));
                        out.insert(v.name.clone(), OverlayValue::Bad);
                    }
                }
            }
        }
        out
    }

    fn load_from(&self, env: &Env, warnings: &mut Vec<String>) -> Result<Values, ValidationError> {
        let root = crate::load::file_root(&env.vars);
        let profile_defaults = self
            .profiles
            .as_ref()
            .and_then(|p| p.defaults.get(self.selected_profile(p, env)));
        let mut violations = Vec::new();
        let overlays = self.overlay_values(root.as_deref(), &mut violations, warnings);
        let mut values = BTreeMap::new();
        let mut typed: HashMap<String, Typed> = HashMap::new();
        for var in &self.vars {
            let mut from_wire = false;
            let in_env = match env::read(var, env) {
                Err(v) => {
                    violations.push(v);
                    continue;
                }
                Ok(t) => t,
            };
            let overlay = overlays.get(&var.name);
            if in_env.is_some() {
                if let Some(w) = env::deprecation(var, env) {
                    warnings.push(w);
                }
                if matches!(overlay, Some(OverlayValue::Raw(_) | OverlayValue::Items(_))) {
                    warnings.push(format!(
                        "{} is set in the environment and in an overlay; the environment wins",
                        var.name
                    ));
                }
            }
            let found = match (in_env, overlay) {
                (Some(t), _) => {
                    from_wire = true;
                    Some(t)
                }
                (None, Some(OverlayValue::Bad)) => continue,
                (None, Some(ov)) => {
                    let parsed = match ov {
                        OverlayValue::Raw(raw) => value::parse_wire(var, raw),
                        OverlayValue::Items(items) => value::parse_items(
                            var.kind.item().expect("list-like"),
                            items.iter().map(String::as_str),
                        ),
                        OverlayValue::Bad => unreachable!(),
                    };
                    match parsed {
                        Ok(t) => {
                            if let Some(msg) = &var.deprecated {
                                warnings.push(format!(
                                    "{} is deprecated: {msg} (set by an overlay)",
                                    var.name
                                ));
                            }
                            from_wire = true;
                            Some(t)
                        }
                        Err((code, e)) => {
                            violations.push(Violation {
                                input: var.name.clone(),
                                code,
                                message: format!("overlay value {e}"),
                            });
                            continue;
                        }
                    }
                }
                (None, None) => profile_defaults
                    .and_then(|d| d.get(&var.name))
                    .or(var.default.as_ref())
                    .cloned(),
            };
            match found {
                None if var.required => violations.push(env::missing(var)),
                None => {
                    values.insert(var.name.clone(), None);
                }
                Some(t) => match env::finish(var, t, from_wire) {
                    Ok(t) => {
                        typed.insert(var.name.clone(), t.clone());
                        values.insert(var.name.clone(), Some(Value::from_typed(&var.kind, t)));
                    }
                    Err(v) => violations.extend(v),
                },
            }
        }

        let fcx = FileCx {
            root,
            env: &env.vars,
            now: SystemTime::now(),
            values: &typed,
        };
        let mut files = BTreeMap::new();
        for f in &self.files {
            match files::check(f, &fcx) {
                Outcome::Absent => {
                    files.insert(f.name.clone(), None);
                }
                Outcome::Failed(v) => violations.extend(v),
                Outcome::Loaded(b) => {
                    if let Some(w) = file_deprecation(f) {
                        warnings.push(w);
                    }
                    files.insert(f.name.clone(), FileValue::from_loaded(b).map(Arc::new));
                }
            }
        }
        if !violations.is_empty() {
            return Err(ValidationError { violations });
        }
        Ok(Values {
            values,
            files,
            secrets: self
                .vars
                .iter()
                .filter(|v| v.secret)
                .map(|v| v.name.clone())
                .chain(
                    self.files
                        .iter()
                        .filter(|f| f.secret)
                        .map(|f| f.name.clone()),
                )
                .collect(),
        })
    }
}

/// The warning for a deprecated file input that is present.
pub(crate) fn file_deprecation(f: &FileDecl) -> Option<String> {
    let msg = f.deprecated.as_ref()?;
    let by = f
        .replaced_by
        .as_ref()
        .map(|r| format!("; use {r}"))
        .unwrap_or_default();
    Some(format!("file input {} is deprecated: {msg}{by}", f.name))
}

/// Reads an overlay file as JSON. `Ok(None)` when it does not exist: an
/// overlay is optional. One that does not parse, or does not hold an
/// object, is `file_malformed`.
fn read_overlay(path: &Path, format: &str) -> Result<Option<Json>, (Code, String)> {
    let shown = path.display();
    let text = match std::fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err((Code::FileUnreadable, format!("cannot read {shown}: {e}"))),
    };
    let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&text);
    let Ok(text) = std::str::from_utf8(text) else {
        return Err((Code::FileMalformed, format!("{shown} is not valid UTF-8")));
    };
    let doc = files::parse_structured(format, text);
    match doc {
        Ok(d @ Json::Object(_)) => Ok(Some(d)),
        Ok(_) => Err((
            Code::FileMalformed,
            format!("{shown} does not hold an object"),
        )),
        Err(e) => Err((
            Code::FileMalformed,
            format!("{shown} is not valid {}: {e}", format.to_uppercase()),
        )),
    }
}

/// Converts a native overlay value to the wire string it stands for (SPEC
/// §4.7): a string as it is, a bool as `true` or `false`, a number with an
/// integral value as an integer and any other in shortest round-trip
/// decimal, a list item by item, and a `json` value as compact JSON.
fn overlay_value(kind: &VarKind, val: &Json) -> Result<OverlayValue, String> {
    match kind {
        VarKind::Json { .. } => serde_json::to_string(val)
            .map(OverlayValue::Raw)
            .map_err(|e| e.to_string()),
        k if k.is_listlike() => {
            let Json::Array(items) = val else {
                return Err(format!("is {}, not a list", json_kind(val)));
            };
            items
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    scalar_text(x)
                        .ok_or_else(|| format!("item {i} is {}, not a scalar", json_kind(x)))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(OverlayValue::Items)
        }
        _ => scalar_text(val)
            .map(OverlayValue::Raw)
            .ok_or_else(|| format!("is {}, not a scalar", json_kind(val))),
    }
}

fn scalar_text(x: &Json) -> Option<String> {
    match x {
        Json::String(s) => Some(s.clone()),
        Json::Bool(b) => Some(b.to_string()),
        Json::Number(n) => Some(if n.is_i64() || n.is_u64() {
            n.to_string()
        } else {
            let f = n.as_f64().unwrap_or(f64::NAN);
            if f.fract() == 0.0 && f.abs() < 9.223_372_036_854_776e18 {
                (f as i64).to_string()
            } else {
                f.to_string()
            }
        }),
        _ => None,
    }
}

fn json_kind(x: &Json) -> &'static str {
    match x {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "a list",
        Json::Object(_) => "an object",
    }
}
