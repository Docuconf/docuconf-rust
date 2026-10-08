//! Contract export (SPEC §4): the declaration as a CUE `#Contract`.

use std::collections::BTreeMap;

use figment::value::{Dict, Value};
use figment::{Figment, Profile, Provider};

use crate::cue::{body, Node};
use crate::decl::{Declaration, FileDecl, FileKind, ItemKind, ListEncoding, VarDecl, VarKind};
use crate::error::DeclarationError;
use crate::overlay::{self, Overlay};
use crate::value::{self, Typed};

/// Contract metadata.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    /// Service name, a DNS label such as `billing-api`.
    pub name: String,
    /// The application version or git SHA the contract is exported from.
    pub app_version: Option<String>,
    /// The CUE package name. Defaults to `name` with `-` replaced by `_`.
    pub package: Option<String>,
}

impl Meta {
    /// Metadata with just a service name.
    pub fn new(name: impl Into<String>) -> Self {
        Meta {
            name: name.into(),
            ..Meta::default()
        }
    }

    /// Sets the application version.
    pub fn app_version(mut self, v: impl Into<String>) -> Self {
        self.app_version = Some(v.into());
        self
    }

    /// Sets the CUE package name (default: `name` with `-` replaced by
    /// `_`).
    pub fn package(mut self, p: impl Into<String>) -> Self {
        self.package = Some(p.into());
        self
    }
}

/// The profile selector must be a declared, non-secret variable, so export
/// and boot agree on it.
pub(crate) fn check_selector(decl: &Declaration, selector: &str) -> Result<(), DeclarationError> {
    let problem = match decl.var(selector) {
        None => format!(
            "profile selector {selector} must be a declared variable; add a field for it (for example `app_profile: Option<String>`) to the struct"
        ),
        Some(v) if v.secret => format!("profile selector {selector} must not be a secret"),
        _ => return Ok(()),
    };
    Err(DeclarationError {
        problems: vec![problem],
    })
}

/// The profile layout of the app's config files.
#[derive(Debug, Clone)]
pub(crate) struct Profiles {
    pub selector: String,
    pub default: String,
}

/// Values found in the app's config files, keyed by variable name.
#[derive(Debug, Default)]
pub(crate) struct FileValues {
    pub base: BTreeMap<String, Typed>,
    pub profiles: BTreeMap<String, BTreeMap<String, Typed>>,
    pub warnings: Vec<String>,
}

fn lookup<'a>(dict: &'a Dict, key: &[String]) -> Option<&'a Value> {
    let (first, rest) = key.split_first()?;
    let v = dict.get(first)?;
    if rest.is_empty() {
        return Some(v);
    }
    match v {
        Value::Dict(_, d) => lookup(d, rest),
        _ => None,
    }
}

fn leaf_paths(dict: &Dict, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    for (k, v) in dict {
        prefix.push(k.clone());
        match v {
            Value::Dict(_, d) if !d.is_empty() => leaf_paths(d, prefix, out),
            _ => out.push(prefix.clone()),
        }
        prefix.pop();
    }
}

/// Reads the values the app's config files (the figment the app passed to
/// the loader) give each declared variable: the default and global profiles
/// are always loaded, so their values are defaults; any other profile's
/// values go in `profiles.defaults`.
pub(crate) fn file_values(
    decl: &Declaration,
    figment: &Figment,
) -> Result<FileValues, DeclarationError> {
    let data = figment.data().map_err(|e| DeclarationError {
        problems: vec![format!("reading the app's config files: {e}")],
    })?;
    let mut out = FileValues::default();
    let mut problems = Vec::new();
    let global_has = |key: &[String]| {
        data.get(&Profile::Global)
            .is_some_and(|d| lookup(d, key).is_some())
    };
    for (profile, dict) in &data {
        let is_base = *profile == Profile::Default || *profile == Profile::Global;
        let mut leaves = Vec::new();
        leaf_paths(dict, &mut Vec::new(), &mut leaves);
        for leaf in leaves {
            let declared = decl.vars.iter().any(|v| v.key == leaf)
                || decl.files.iter().any(|f| leaf.starts_with(&f.key));
            if !declared {
                out.warnings.push(format!(
                    "config key {} (profile {profile}) is not a declared variable, so the platform cannot set it",
                    leaf.join(".")
                ));
            }
        }
        for var in &decl.vars {
            let Some(v) = lookup(dict, &var.key) else {
                continue;
            };
            if var.secret {
                problems.push(format!(
                    "{} ({}): a secret must not have a value in a config file (profile {profile}); it would ship inside the image",
                    var.name, var.field
                ));
                continue;
            }
            match value::from_figment(&var.kind, v) {
                Err(e) => problems.push(format!(
                    "{} ({}): config file value (profile {profile}) {e}",
                    var.name, var.field
                )),
                Ok(t) => {
                    for (_, msg) in value::check(var, &t) {
                        problems.push(format!(
                            "{} ({}): config file value (profile {profile}) {msg}",
                            var.name, var.field
                        ));
                    }
                    if is_base {
                        // Global overrides default, as in figment.
                        if *profile == Profile::Global || !out.base.contains_key(&var.name) {
                            out.base.insert(var.name.clone(), t);
                        }
                    } else if !global_has(&var.key) {
                        // figment's global profile overrides every other
                        // profile, so a profile value under a global one
                        // never applies.
                        out.profiles
                            .entry(profile.as_str().to_string())
                            .or_default()
                            .insert(var.name.clone(), t);
                    }
                }
            }
        }
    }
    if !problems.is_empty() {
        return Err(DeclarationError { problems });
    }
    Ok(out)
}

fn typed_node(t: &Typed) -> Node {
    match t {
        Typed::Str(s) => Node::Str(s.clone()),
        Typed::Int(i) => Node::Int(*i),
        Typed::Float(f) => Node::Float(*f),
        Typed::Bool(b) => Node::Bool(*b),
        Typed::Dur(d) => Node::Str(crate::duration::format_go(*d)),
        Typed::List(l) => Node::List(l.iter().map(typed_node).collect()),
        Typed::Json(j) => Node::from_json(j),
    }
}

fn strs(v: &[String]) -> Node {
    Node::List(v.iter().map(|s| Node::Str(s.clone())).collect())
}

fn deprecated(msg: &Option<String>, by: &Option<String>) -> Option<Node> {
    let msg = msg.as_ref()?;
    let mut f = vec![("message".to_string(), Node::Str(msg.clone()))];
    if let Some(b) = by {
        f.push(("replacedBy".into(), Node::Str(b.clone())));
    }
    Some(Node::Struct(f))
}

fn var_node(v: &VarDecl, file_default: Option<&Typed>, config_key: Option<&String>) -> Node {
    let mut f: Vec<(String, Node)> = Vec::new();
    let mut add = |k: &str, n: Node| f.push((k.to_string(), n));
    add("type", Node::Str(v.kind.type_name().into()));
    add("description", Node::Str(v.description.clone()));
    if let Some(d) = &v.details {
        add("details", Node::Str(d.clone()));
    }
    // A value in an always-loaded config file makes the variable optional
    // with that default (SPEC §4.4).
    let default = file_default.or(v.default.as_ref());
    if v.required && default.is_none() {
        add("required", Node::Bool(true));
    }
    if v.secret {
        add("secret", Node::Bool(true));
    }
    if let Some(d) = default {
        add("default", typed_node(d));
    }
    if let Some(g) = &v.group {
        add("group", Node::Str(g.clone()));
    }
    if !v.examples.is_empty() {
        add("examples", strs(&v.examples));
    }
    if let Some(d) = deprecated(&v.deprecated, &v.replaced_by) {
        add("deprecated", d);
    }
    if let Some(k) = config_key.or(v.config_key.as_ref()) {
        add("configKey", Node::Str(k.clone()));
    }
    match &v.kind {
        VarKind::String => {
            if let Some(n) = v.min_length {
                add("minLength", Node::Int(n as i64));
            }
            if let Some(n) = v.max_length {
                add("maxLength", Node::Int(n as i64));
            }
            if let Some((p, _)) = &v.pattern {
                add("pattern", Node::Str(p.clone()));
            }
        }
        VarKind::Int { .. } | VarKind::Float => {
            if let Some(m) = &v.min {
                add("min", typed_node(m));
            }
            if let Some(m) = &v.max {
                add("max", typed_node(m));
            }
        }
        VarKind::Duration => {
            add("encoding", Node::Str(v.duration_encoding.name().into()));
            if let Some(m) = &v.min {
                add("min", typed_node(m));
            }
            if let Some(m) = &v.max {
                add("max", typed_node(m));
            }
        }
        VarKind::Url => {
            if !v.schemes.is_empty() {
                add("schemes", strs(&v.schemes));
            }
            if let Some(n) = v.max_length {
                add("maxLength", Node::Int(n as i64));
            }
        }
        VarKind::Enum(values) => add("values", strs(values)),
        VarKind::List(item) => {
            add(
                "items",
                Node::Str(
                    match item {
                        ItemKind::String => "string",
                        ItemKind::Int { .. } => "int",
                    }
                    .into(),
                ),
            );
            add("encoding", Node::Str(v.list_encoding.name().into()));
            if let ListEncoding::Csv(sep) = &v.list_encoding {
                add("separator", Node::Str(sep.clone()));
            }
            if let Some(n) = v.min_items {
                add("minItems", Node::Int(n as i64));
            }
            if let Some(n) = v.max_items {
                add("maxItems", Node::Int(n as i64));
            }
            if let Some(n) = v.item_min {
                add("itemMin", Node::Int(n));
            }
            if let Some(n) = v.item_max {
                add("itemMax", Node::Int(n));
            }
            if let Some(n) = v.item_min_length {
                add("itemMinLength", Node::Int(n as i64));
            }
            if let Some(n) = v.item_max_length {
                add("itemMaxLength", Node::Int(n as i64));
            }
        }
        VarKind::Json { schema, .. } => {
            if let Some(n) = v.max_length {
                add("maxLength", Node::Int(n as i64));
            }
            add("schema", Node::from_json(schema));
        }
        VarKind::Bool => {}
    }
    Node::Struct(f)
}

fn file_node(d: &FileDecl) -> Node {
    let mut f: Vec<(String, Node)> = Vec::new();
    let mut add = |k: &str, n: Node| f.push((k.to_string(), n));
    add("type", Node::Str(d.kind.type_name().into()));
    if let Some(fmt) = &d.format {
        add("format", Node::Str(fmt.clone()));
    }
    add("description", Node::Str(d.description.clone()));
    if let Some(details) = &d.details {
        add("details", Node::Str(details.clone()));
    }
    if d.required {
        add("required", Node::Bool(true));
    }
    let forced = matches!(d.kind, FileKind::Tls | FileKind::Keystore);
    if d.secret && !forced {
        add("secret", Node::Bool(true));
    }
    add("path", Node::Str(d.path.clone()));
    if let Some(p) = &d.path_env {
        add("pathEnv", Node::Str(p.clone()));
    }
    if let Some(n) = d.max_size {
        add("maxSize", Node::Int(n as i64));
    }
    if let Some(g) = &d.group {
        add("group", Node::Str(g.clone()));
    }
    if let Some(x) = deprecated(&d.deprecated, &d.replaced_by) {
        add("deprecated", x);
    }
    match &d.kind {
        FileKind::Config { schema, .. } => add("schema", Node::from_json(schema)),
        FileKind::Tls => {
            if !d.dns_names.is_empty() {
                add("dnsNames", strs(&d.dns_names));
            }
            if !d.key_algorithms.is_empty() {
                add("keyAlgorithms", strs(&d.key_algorithms));
            }
            if let Some(m) = d.min_remaining {
                add("minRemaining", Node::Str(crate::duration::format_go(m)));
            }
            if d.require_ca {
                add("requireCA", Node::Bool(true));
            }
        }
        FileKind::CaBundle => {
            if d.min_certificates != 1 {
                add("minCertificates", Node::Int(d.min_certificates as i64));
            }
        }
        FileKind::Keystore => {
            if let Some(p) = &d.password_var {
                add("passwordVar", Node::Str(p.clone()));
            }
        }
        FileKind::Text => {
            if let Some((p, _)) = &d.pattern {
                add("pattern", Node::Str(p.clone()));
            }
            if let Some(n) = d.min_length {
                add("minLength", Node::Int(n as i64));
            }
            if let Some(n) = d.max_length {
                add("maxLength", Node::Int(n as i64));
            }
        }
        FileKind::Binary => {}
    }
    Node::Struct(f)
}

fn overlay_node(o: &Overlay) -> Node {
    let mut f: Vec<(String, Node)> = Vec::new();
    if let Some(d) = o.description_text() {
        f.push(("description".into(), Node::Str(d.trim().to_string())));
    }
    f.push(("format".into(), Node::Str(o.format_name().into())));
    f.push(("path".into(), Node::Str(o.path().into())));
    f.push((
        "keySeparator".into(),
        Node::Str(overlay::KEY_SEPARATOR.into()),
    ));
    f.push(("reload".into(), Node::Str("restart".into())));
    Node::Struct(f)
}

fn is_dns_label(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Renders the contract document.
pub(crate) fn render(
    decl: &Declaration,
    meta: &Meta,
    values: Option<&FileValues>,
    profiles: Option<&Profiles>,
    overlays: &[Overlay],
) -> Result<String, DeclarationError> {
    let mut problems = Vec::new();
    if !is_dns_label(&meta.name) {
        problems.push(format!(
            "contract name {:?} must be a DNS label ([a-z0-9-], at most 63 characters)",
            meta.name
        ));
    }
    let package = meta
        .package
        .clone()
        .unwrap_or_else(|| meta.name.replace('-', "_"));
    let package_ok = package
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !package_ok {
        problems.push(format!("CUE package name {package:?} is not an identifier"));
    }
    if let Some(p) = profiles {
        if let Err(e) = check_selector(decl, &p.selector) {
            problems.extend(e.problems);
        }
    }
    if !problems.is_empty() {
        return Err(DeclarationError { problems });
    }

    let generator = vec![
        ("language".to_string(), Node::Str("rust".into())),
        ("sdk".to_string(), Node::Str("docuconf".into())),
        (
            "version".to_string(),
            Node::Str(env!("CARGO_PKG_VERSION").into()),
        ),
    ];
    let mut metadata = vec![("name".to_string(), Node::Str(meta.name.clone()))];
    if let Some(v) = &meta.app_version {
        metadata.push(("appVersion".into(), Node::Str(v.clone())));
    }
    metadata.push(("generator".into(), Node::Struct(generator)));

    let config_keys = if overlays.is_empty() {
        BTreeMap::new()
    } else {
        overlay::config_keys(decl, profiles.map(|p| p.selector.as_str()))
    };
    let base = values.map(|f| &f.base);
    let vars = decl
        .vars
        .iter()
        .map(|v| {
            let fd = base.and_then(|b| b.get(&v.name));
            (v.name.clone(), var_node(v, fd, config_keys.get(&v.name)))
        })
        .collect();
    let mut top = vec![
        (
            "apiVersion".to_string(),
            Node::Str("docuconf.dev/v1alpha1".into()),
        ),
        ("kind".into(), Node::Str("ConfigContract".into())),
        ("metadata".into(), Node::Struct(metadata)),
        ("vars".into(), Node::Struct(vars)),
    ];
    if !decl.files.is_empty() {
        top.push((
            "files".into(),
            Node::Struct(
                decl.files
                    .iter()
                    .map(|f| (f.name.clone(), file_node(f)))
                    .collect(),
            ),
        ));
    }
    if !overlays.is_empty() {
        top.push((
            "overlays".into(),
            Node::Struct(
                overlays
                    .iter()
                    .map(|o| (o.name().to_string(), overlay_node(o)))
                    .collect(),
            ),
        ));
    }
    if let Some(p) = profiles {
        let empty = BTreeMap::new();
        let defaults = values
            .map(|v| &v.profiles)
            .unwrap_or(&empty)
            .iter()
            .map(|(name, vals)| {
                (
                    name.clone(),
                    Node::Struct(
                        vals.iter()
                            .map(|(k, t)| (k.clone(), typed_node(t)))
                            .collect(),
                    ),
                )
            })
            .collect();
        top.push((
            "profiles".into(),
            Node::Struct(vec![
                ("selector".into(), Node::Str(p.selector.clone())),
                ("default".into(), Node::Str(p.default.clone())),
                ("defaults".into(), Node::Struct(defaults)),
            ]),
        ));
    }

    let mut out = String::new();
    out.push_str("// Code generated by docuconf. DO NOT EDIT.\n");
    out.push_str(&format!("package {package}\n\n"));
    out.push_str("import \"docuconf.dev/contract\"\n\n");
    out.push_str("contract.#Contract & {\n");
    body(&mut out, &top, 1);
    out.push_str("}\n");
    Ok(out)
}
