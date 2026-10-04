#![allow(missing_docs)]
//! The declaration model: what a `#[derive(Docuconf)]` struct declares,
//! checked for mistakes before any value is read (SPEC §11.2 item 2).

use std::collections::BTreeMap;
use std::time::Duration;

use regex::Regex;

use crate::__private::{FieldAttrs, Lit};
use crate::duration::{format_go, parse_duration};
use crate::error::DeclarationError;
use crate::value::{self, Typed};

/// Checks a value against a schema and binds it to the app's type.
pub type BindFn = fn(&serde_json::Value) -> Result<(), String>;

/// The contract type of a variable.
#[derive(Debug, Clone)]
pub enum VarKind {
    String,
    Int {
        min: i64,
        max: i64,
    },
    Float,
    Bool,
    Duration,
    Url,
    Enum(Vec<String>),
    List(ItemKind),
    Json {
        schema: serde_json::Value,
        bind: BindFn,
    },
}

/// The element type of a `list` variable.
#[derive(Debug, Clone, Copy)]
pub enum ItemKind {
    String,
    Int { min: i64, max: i64 },
}

/// The contract type of a file input.
#[derive(Debug, Clone)]
pub enum FileKind {
    Config {
        schema: serde_json::Value,
        bind: BindFn,
    },
    Tls,
    CaBundle,
    Keystore,
    Text,
    Binary,
}

impl VarKind {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            VarKind::String => "string",
            VarKind::Int { .. } => "int",
            VarKind::Float => "float",
            VarKind::Bool => "bool",
            VarKind::Duration => "duration",
            VarKind::Url => "url",
            VarKind::Enum(_) => "enum",
            VarKind::List(_) => "list",
            VarKind::Json { .. } => "json",
        }
    }
}

impl FileKind {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            FileKind::Config { .. } => "config",
            FileKind::Tls => "tls",
            FileKind::CaBundle => "caBundle",
            FileKind::Keystore => "keystore",
            FileKind::Text => "text",
            FileKind::Binary => "binary",
        }
    }
}

/// One declared environment variable.
#[derive(Debug, Clone)]
pub(crate) struct VarDecl {
    pub name: String,
    pub key: Vec<String>,
    pub field: String,
    pub kind: VarKind,
    pub description: String,
    pub required: bool,
    pub secret: bool,
    pub default: Option<Typed>,
    pub min: Option<Typed>,
    pub max: Option<Typed>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub pattern: Option<(String, Regex)>,
    pub schemes: Vec<String>,
    pub min_items: Option<u64>,
    pub max_items: Option<u64>,
    pub group: Option<String>,
    pub examples: Vec<String>,
    pub deprecated: Option<String>,
    pub replaced_by: Option<String>,
    pub config_key: Option<String>,
}

impl VarDecl {
    pub(crate) fn key_path(&self) -> String {
        self.key.join(".")
    }
}

/// One declared file input.
#[derive(Debug, Clone)]
pub(crate) struct FileDecl {
    pub name: String,
    pub key: Vec<String>,
    pub field: String,
    pub kind: FileKind,
    pub description: String,
    pub required: bool,
    pub secret: bool,
    pub path: String,
    pub path_env: Option<String>,
    pub max_size: Option<u64>,
    pub group: Option<String>,
    pub deprecated: Option<String>,
    pub replaced_by: Option<String>,
    pub format: Option<String>,
    pub dns_names: Vec<String>,
    pub key_algorithms: Vec<String>,
    pub min_remaining: Option<Duration>,
    pub require_ca: bool,
    pub min_certificates: u64,
    pub password_var: Option<String>,
    pub pattern: Option<(String, Regex)>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
}

impl FileDecl {
    pub(crate) fn key_path(&self) -> String {
        self.key.join(".")
    }

    /// The directory the platform mounts: the path itself for a TLS key
    /// pair, its parent otherwise.
    pub(crate) fn mount_dir(&self) -> String {
        if matches!(self.kind, FileKind::Tls) {
            return self.path.clone();
        }
        match self.path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(i) => self.path[..i].to_string(),
        }
    }
}

/// A checked declaration.
#[derive(Debug, Clone)]
pub(crate) struct Declaration {
    pub vars: Vec<VarDecl>,
    pub files: Vec<FileDecl>,
    pub warnings: Vec<String>,
}

impl Declaration {
    pub(crate) fn var(&self, name: &str) -> Option<&VarDecl> {
        self.vars.iter().find(|v| v.name == name)
    }
}

/// Collects declarations while the derived code walks the struct.
#[derive(Debug)]
pub struct DeclCx {
    prefix: String,
    keys: Vec<String>,
    optional: bool,
    secret: bool,
    vars: Vec<VarDecl>,
    files: Vec<FileDecl>,
    problems: Vec<String>,
    warnings: Vec<String>,
}

/// Mount directories a file input must never hide (files.cue #ReservedDirs).
const RESERVED_DIRS: &[&str] = &[
    "/",
    "/app",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/etc/pki",
    "/etc/ssl",
    "/etc/ssl/certs",
    "/home",
    "/lib",
    "/lib64",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/tmp",
    "/usr",
    "/usr/lib",
    "/usr/local",
    "/usr/share",
    "/var",
    "/var/lib",
    "/var/run",
];

const COMMON_ATTRS: &[&str] = &[
    "description",
    "desc",
    "env",
    "required",
    "secret",
    "default",
    "group",
    "examples",
    "deprecated",
    "replaced_by",
    "config_key",
];

const FILE_COMMON_ATTRS: &[&str] = &[
    "description",
    "desc",
    "name",
    "required",
    "secret",
    "group",
    "deprecated",
    "replaced_by",
    "path",
    "path_env",
    "reload",
    "max_size",
];

pub(crate) fn is_env_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some('A'..='Z'))
        && c.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_input_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 42
        && b[0].is_ascii_lowercase()
        && (b.len() == 1 || b[b.len() - 1] != b'-')
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn is_abs_path(p: &str) -> bool {
    p.starts_with('/')
        && p.len() > 1
        && !p.ends_with('/')
        && !p.contains("//")
        && p.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b'-'))
        && !p.split('/').any(|seg| seg == "." || seg == "..")
}

fn compile_pattern(p: &str) -> Result<Regex, String> {
    Regex::new(p).map_err(|e| {
        let msg = e.to_string();
        let last = msg.lines().last().unwrap_or("").trim().to_string();
        format!("pattern {p:?} is not valid RE2: {last}")
    })
}

fn parse_size(l: &Lit) -> Result<u64, String> {
    match l {
        Lit::Int(i) if *i > 0 => u64::try_from(*i).map_err(|_| "maxSize is too large".into()),
        Lit::Str(s) => {
            let (num, mult) = [
                ("Ki", 1u64 << 10),
                ("Mi", 1 << 20),
                ("Gi", 1 << 30),
                ("K", 1000),
                ("M", 1_000_000),
                ("G", 1_000_000_000),
            ]
            .iter()
            .find_map(|(suf, m)| s.strip_suffix(suf).map(|n| (n, *m)))
            .unwrap_or((s, 1));
            num.trim()
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .and_then(|n| n.checked_mul(mult))
                .ok_or_else(|| format!("maxSize {s:?} is not a size such as 65536 or \"64Ki\""))
        }
        _ => Err("maxSize must be a positive integer or a string such as \"64Ki\"".into()),
    }
}

impl DeclCx {
    pub(crate) fn new(prefix: &str) -> Self {
        DeclCx {
            prefix: prefix.to_string(),
            keys: Vec::new(),
            optional: false,
            secret: false,
            vars: Vec::new(),
            files: Vec::new(),
            problems: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Marks the field being declared as optional (`Option<T>`).
    pub fn with_optional(&mut self, f: impl FnOnce(&mut DeclCx)) {
        let old = std::mem::replace(&mut self.optional, true);
        f(self);
        self.optional = old;
    }

    /// Marks the field being declared as secret (`Secret<T>`).
    pub fn with_secret(&mut self, f: impl FnOnce(&mut DeclCx)) {
        let old = std::mem::replace(&mut self.secret, true);
        f(self);
        self.secret = old;
    }

    fn take_flags(&mut self) -> (bool, bool) {
        (
            std::mem::take(&mut self.optional),
            std::mem::take(&mut self.secret),
        )
    }

    /// Declares a nested group of inputs (a field whose type also derives
    /// `Docuconf`). Its variables are named `PARENT__CHILD`, as figment's
    /// `Env::split("__")` does.
    pub fn nested(&mut self, a: &FieldAttrs, f: fn(&mut DeclCx)) {
        let (optional, secret) = self.take_flags();
        let label = format!("{} ({})", a.key, a.field);
        if optional {
            self.problems.push(format!(
                "{label}: an optional group (Option<Struct>) is not supported; make the fields optional instead"
            ));
        }
        if secret {
            self.problems.push(format!(
                "{label}: Secret<T> needs a variable type, not a struct"
            ));
        }
        if let Some(bad) = a.set.first() {
            self.problems.push(format!(
                "{label}: attribute `{bad}` does not apply to a nested group"
            ));
        }
        let old_prefix = self.prefix.clone();
        self.prefix = format!("{}{}__", self.prefix, a.key.to_uppercase());
        self.keys.push(a.key.to_string());
        f(self);
        self.keys.pop();
        self.prefix = old_prefix;
    }

    /// Declares one environment variable.
    pub fn var(&mut self, a: &FieldAttrs, kind: VarKind) {
        let (optional, wrapped_secret) = self.take_flags();
        let name = match a.env {
            Some(e) => e.to_string(),
            None => format!("{}{}", self.prefix, a.key.to_uppercase()),
        };
        let label = format!("{name} ({})", a.field);
        let mut problems = Vec::new();

        // Attributes that turn a String into a url or enum variable.
        let mut kind = kind;
        if matches!(kind, VarKind::String) {
            if !a.values.is_empty() {
                kind = VarKind::Enum(a.values.iter().map(|s| s.to_string()).collect());
            } else if a.url || !a.schemes.is_empty() {
                kind = VarKind::Url;
            }
        }

        let allowed: &[&str] = match &kind {
            VarKind::String => &["min_length", "max_length", "pattern"],
            VarKind::Int { .. } | VarKind::Float | VarKind::Duration => &["min", "max"],
            VarKind::Url => &["schemes", "url"],
            VarKind::Enum(_) => &["values"],
            VarKind::List(_) => &["min_items", "max_items"],
            VarKind::Bool | VarKind::Json { .. } => &[],
        };
        for attr in a.set {
            if !COMMON_ATTRS.contains(attr) && !allowed.contains(attr) {
                problems.push(format!(
                    "attribute `{attr}` does not apply to a {} variable",
                    kind.type_name()
                ));
            }
        }

        if !is_env_name(&name) {
            problems.push(
                "variable name must match ^[A-Z][A-Z0-9_]*$; rename the field or set #[docuconf(env = \"...\")]"
                    .to_string(),
            );
        }
        let description = a.description.trim().to_string();
        if description.chars().count() < 5 {
            problems.push(if description.is_empty() {
                "needs a description: add a /// doc comment".to_string()
            } else {
                format!("description {description:?} is shorter than 5 characters")
            });
        }
        if let VarKind::Enum(values) = &kind {
            if values.is_empty() {
                problems.push("an enum needs at least one value".into());
            }
        }
        if matches!(kind, VarKind::Duration)
            && !a.serde_with.is_some_and(|w| w.contains("humantime"))
        {
            problems.push(
                "a Duration field needs #[serde(with = \"docuconf::humantime_serde\")] so values such as \"1m30s\" deserialize"
                    .into(),
            );
        }

        let secret = a.secret || wrapped_secret;
        let has_default = a.default.is_some();
        if a.required && has_default {
            problems.push("a required variable must not have a default".into());
        }
        if secret && has_default {
            problems.push("a secret variable must not have a default".into());
        }
        if secret && !a.examples.is_empty() {
            problems.push("a secret variable must not have examples".into());
        }
        if a.serde_default && !optional && !has_default {
            problems.push(
                "#[serde(default)] hides the default from the contract; use #[docuconf(default = ...)] or Option<T>"
                    .into(),
            );
        }
        let required = a.required || (!optional && !has_default);

        // Constraints, with integer bounds narrowed to the Rust type.
        let mut min = None;
        let mut max = None;
        let bound = |l: Option<Lit>, which: &str, problems: &mut Vec<String>| -> Option<Typed> {
            let l = l?;
            match value::lit_bound(&kind, &l) {
                Ok(t) => Some(t),
                Err(e) => {
                    problems.push(format!("{which}: {e}"));
                    None
                }
            }
        };
        if let VarKind::Int {
            min: tmin,
            max: tmax,
        } = kind
        {
            let umin = bound(a.min, "min", &mut problems);
            let umax = bound(a.max, "max", &mut problems);
            let lo = match umin {
                Some(Typed::Int(v)) => v.max(tmin),
                _ => tmin,
            };
            let hi = match umax {
                Some(Typed::Int(v)) => v.min(tmax),
                _ => tmax,
            };
            if lo != i64::MIN {
                min = Some(Typed::Int(lo));
            }
            if hi != i64::MAX {
                max = Some(Typed::Int(hi));
            }
        } else {
            min = bound(a.min, "min", &mut problems);
            max = bound(a.max, "max", &mut problems);
        }

        let pattern = match a.pattern {
            Some(p) => match compile_pattern(p) {
                Ok(re) => Some((p.to_string(), re)),
                Err(e) => {
                    problems.push(e);
                    None
                }
            },
            None => None,
        };
        if let Some(r) = a.replaced_by {
            if !is_env_name(r) {
                problems.push(format!("replaced_by {r:?} is not a variable name"));
            }
        }
        if a.replaced_by.is_some() && a.deprecated.is_none() {
            problems.push("replaced_by needs deprecated = \"message\"".into());
        }

        let mut decl = VarDecl {
            name: name.clone(),
            key: self
                .keys
                .iter()
                .cloned()
                .chain([a.key.to_string()])
                .collect(),
            field: a.field.to_string(),
            kind,
            description,
            required,
            secret,
            default: None,
            min,
            max,
            min_length: a.min_length,
            max_length: a.max_length,
            pattern,
            schemes: a.schemes.iter().map(|s| s.to_string()).collect(),
            min_items: a.min_items,
            max_items: a.max_items,
            group: a.group.map(str::to_string),
            examples: a.examples.iter().map(|s| s.to_string()).collect(),
            deprecated: a.deprecated.map(str::to_string),
            replaced_by: a.replaced_by.map(str::to_string),
            config_key: a.config_key.map(str::to_string),
        };
        if let Some(l) = &a.default {
            match value::lit_value(&decl.kind, l) {
                Ok(t) => {
                    for (_, msg) in value::check(&decl, &t) {
                        problems.push(format!("default {msg}"));
                    }
                    decl.default = Some(t);
                }
                Err(e) => problems.push(format!("default: {e}")),
            }
        }
        if FEATURE_FLAG.is_match(&name) {
            self.warnings.push(format!(
                "{name}: looks like a feature flag; flags that change without a rollout belong in a flag service (SPEC §10)"
            ));
        }
        for p in problems {
            self.problems.push(format!("{label}: {p}"));
        }
        self.vars.push(decl);
    }

    /// Declares one file input.
    pub fn file(&mut self, a: &FieldAttrs, kind: FileKind) {
        let (optional, wrapped_secret) = self.take_flags();
        let name = match a.name {
            Some(n) => n.to_string(),
            None => a.key.replace('_', "-"),
        };
        let label = format!("file input {name} ({})", a.field);
        let mut problems = Vec::new();

        let allowed: &[&str] = match &kind {
            FileKind::Config { .. } => &["format"],
            FileKind::Tls => &["dns_names", "key_algorithms", "min_remaining", "require_ca"],
            FileKind::CaBundle => &["min_certificates"],
            FileKind::Keystore => &["format", "password_var"],
            FileKind::Text => &["pattern", "min_length", "max_length"],
            FileKind::Binary => &[],
        };
        for attr in a.set {
            if !FILE_COMMON_ATTRS.contains(attr) && !allowed.contains(attr) {
                problems.push(format!(
                    "attribute `{attr}` does not apply to a {} file input",
                    kind.type_name()
                ));
            }
        }
        if !is_input_name(&name) {
            problems.push(
                "input name must be a DNS label (^[a-z]([-a-z0-9]{0,40}[a-z0-9])?$); set #[docuconf(name = \"...\")]"
                    .into(),
            );
        }
        let description = a.description.trim().to_string();
        if description.chars().count() < 5 {
            problems.push(if description.is_empty() {
                "needs a description: add a /// doc comment".to_string()
            } else {
                format!("description {description:?} is shorter than 5 characters")
            });
        }
        let path = a.path.unwrap_or("").to_string();
        if path.is_empty() {
            problems.push("needs #[docuconf(path = \"/absolute/path\")]".into());
        } else if !is_abs_path(&path) {
            problems.push(format!(
                "path {path:?} must be absolute and normalised (no \"..\", \".\", \"//\" or trailing slash)"
            ));
        }
        if let Some(pe) = a.path_env {
            if !is_env_name(pe) {
                problems.push(format!("path_env {pe:?} is not a variable name"));
            }
        }
        match a.reload {
            None | Some("restart") => {}
            Some("watch") => problems.push(
                "reload = \"watch\" is not implemented by this SDK version; use \"restart\"".into(),
            ),
            Some(other) => {
                problems.push(format!("reload {other:?} must be \"restart\" or \"watch\""))
            }
        }
        let max_size = match &a.max_size {
            Some(l) => match parse_size(l) {
                Ok(n) => Some(n),
                Err(e) => {
                    problems.push(e);
                    None
                }
            },
            None => None,
        };
        let ext = path.rsplit('.').next().unwrap_or("");
        let format = match &kind {
            FileKind::Config { .. } => {
                let f = a.format.map(str::to_string).or_else(|| {
                    match ext {
                        "json" => Some("json"),
                        "yaml" | "yml" => Some("yaml"),
                        "toml" => Some("toml"),
                        _ => None,
                    }
                    .map(str::to_string)
                });
                match f.as_deref() {
                    Some("json" | "yaml" | "toml") => {}
                    Some(other) => problems.push(format!(
                        "format {other:?} must be \"json\", \"yaml\" or \"toml\""
                    )),
                    None => problems
                        .push("needs #[docuconf(format = \"json\" | \"yaml\" | \"toml\")]".into()),
                }
                f
            }
            FileKind::Keystore => {
                let f = a.format.unwrap_or("pkcs12");
                match f {
                    "pkcs12" => {}
                    "jks" => problems.push(
                        "format \"jks\" is not supported by this SDK; convert the keystore to PKCS#12"
                            .into(),
                    ),
                    other => problems.push(format!("format {other:?} must be \"pkcs12\"")),
                }
                Some(f.to_string())
            }
            _ => None,
        };
        for alg in a.key_algorithms {
            if !["RSA", "ECDSA", "Ed25519"].contains(alg) {
                problems.push(format!(
                    "key algorithm {alg:?} must be \"RSA\", \"ECDSA\" or \"Ed25519\""
                ));
            }
        }
        let min_remaining = match a.min_remaining {
            Some(s) => match parse_duration(s) {
                Ok(d) => Some(d),
                Err(e) => {
                    problems.push(format!("min_remaining: {e}"));
                    None
                }
            },
            None => None,
        };
        if let Some(n) = a.min_certificates {
            if n < 1 {
                problems.push("min_certificates must be at least 1".into());
            }
        }
        let pattern = match a.pattern {
            Some(p) => match compile_pattern(p) {
                Ok(re) => Some((p.to_string(), re)),
                Err(e) => {
                    problems.push(e);
                    None
                }
            },
            None => None,
        };
        if let (Some(lo), Some(hi)) = (a.min_length, a.max_length) {
            if lo > hi {
                problems.push(format!("min_length {lo} is above max_length {hi}"));
            }
        }

        let secret =
            a.secret || wrapped_secret || matches!(kind, FileKind::Tls | FileKind::Keystore);
        let decl = FileDecl {
            name: name.clone(),
            key: self
                .keys
                .iter()
                .cloned()
                .chain([a.key.to_string()])
                .collect(),
            field: a.field.to_string(),
            kind,
            description,
            required: a.required || !optional,
            secret,
            path,
            path_env: a.path_env.map(str::to_string),
            max_size,
            group: a.group.map(str::to_string),
            deprecated: a.deprecated.map(str::to_string),
            replaced_by: a.replaced_by.map(str::to_string),
            format,
            dns_names: a.dns_names.iter().map(|s| s.to_string()).collect(),
            key_algorithms: a.key_algorithms.iter().map(|s| s.to_string()).collect(),
            min_remaining,
            require_ca: a.require_ca,
            min_certificates: a.min_certificates.unwrap_or(1),
            password_var: a.password_var.map(str::to_string),
            pattern,
            min_length: a.min_length,
            max_length: a.max_length,
        };
        for p in problems {
            self.problems.push(format!("{label}: {p}"));
        }
        self.files.push(decl);
    }

    /// Cross-field checks, then sorting by name.
    pub(crate) fn finish(mut self) -> Result<Declaration, DeclarationError> {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for v in &self.vars {
            if let Some(other) = seen.insert(&v.name, &v.field) {
                self.problems.push(format!(
                    "{} ({}): declared twice, also by {other}",
                    v.name, v.field
                ));
            }
        }
        let mut names: BTreeMap<&str, &str> = BTreeMap::new();
        let mut dirs: BTreeMap<String, &str> = BTreeMap::new();
        let mut path_envs: BTreeMap<&str, &str> = BTreeMap::new();
        for f in &self.files {
            let label = format!("file input {} ({})", f.name, f.field);
            if let Some(other) = names.insert(&f.name, &f.field) {
                self.problems
                    .push(format!("{label}: name used twice, also by {other}"));
            }
            if !f.path.is_empty() {
                let dir = f.mount_dir();
                if RESERVED_DIRS.contains(&dir.as_str()) {
                    self.problems.push(format!(
                        "{label}: would be mounted at {dir}, which hides files the image or OS needs; use a dedicated directory"
                    ));
                }
                if let Some(other) = dirs.insert(dir.clone(), &f.name) {
                    self.problems.push(format!(
                        "{label}: shares mount directory {dir} with file input {other}"
                    ));
                }
            }
            if let Some(pe) = &f.path_env {
                if self.vars.iter().any(|v| &v.name == pe) {
                    self.problems.push(format!(
                        "{label}: path_env {pe} must not also be a declared variable"
                    ));
                }
                if let Some(other) = path_envs.insert(pe, &f.name) {
                    self.problems.push(format!(
                        "{label}: path_env {pe} is also used by file input {other}"
                    ));
                }
            }
            if let Some(pv) = &f.password_var {
                match self.vars.iter().find(|v| &v.name == pv) {
                    None => self.problems.push(format!(
                        "{label}: password_var {pv} is not a declared variable"
                    )),
                    Some(v) if !v.secret => self.problems.push(format!(
                        "{label}: password_var {pv} must be a secret variable"
                    )),
                    _ => {}
                }
            }
        }
        if !self.problems.is_empty() {
            return Err(DeclarationError {
                problems: self.problems,
            });
        }
        self.vars.sort_by(|a, b| a.name.cmp(&b.name));
        self.files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Declaration {
            vars: self.vars,
            files: self.files,
            warnings: self.warnings,
        })
    }
}

static FEATURE_FLAG: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new("^(FF|FEATURE|FEATURE_FLAG|ENABLE)_").unwrap());

/// Builds and checks the declaration of `C`.
pub(crate) fn declaration<C: crate::Docuconf>() -> Result<Declaration, DeclarationError> {
    let mut cx = DeclCx::new(C::PREFIX);
    C::declare_fields(&mut cx);
    cx.finish()
}

/// Canonical Go form of a duration, for messages.
pub(crate) fn go(d: Duration) -> String {
    format_go(d)
}
