//! Boot-time checks of file inputs (SPEC §11.2 item 7).

use std::any::Any;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use figment::providers::{Format, Toml};
use figment::Figment;

use crate::decl::{FileDecl, FileKind};
use crate::error::{Code, Violation};
use crate::types::{BinaryFile, ConfigDoc, TextFile};
use crate::value::Typed;

// `now` and `values` are read by the PKI checks (features `tls` and
// `keystore`).
#[cfg_attr(not(feature = "keystore"), allow(dead_code))]
pub(crate) struct FileCx<'a> {
    pub root: Option<PathBuf>,
    pub env: &'a HashMap<String, String>,
    pub now: SystemTime,
    /// Resolved variable values by name, for keystore passwords.
    pub values: &'a HashMap<String, Typed>,
}

pub(crate) enum Outcome {
    Absent,
    Loaded(Box<dyn Any + Send>),
    Failed(Vec<Violation>),
}

#[cfg_attr(not(feature = "tls"), allow(dead_code))]
pub(crate) struct Ctx<'a> {
    pub(crate) decl: &'a FileDecl,
    pub(crate) path: PathBuf,
    pub(crate) cx: &'a FileCx<'a>,
}

impl Ctx<'_> {
    pub(crate) fn v(&self, code: Code, message: impl Into<String>) -> Violation {
        Violation {
            input: self.decl.name.clone(),
            code,
            message: message.into(),
        }
    }

    /// Reads a file. `Ok(None)` when it is absent and not required.
    pub(crate) fn read(&self, path: &Path, required: bool) -> Result<Option<Vec<u8>>, Violation> {
        let shown = path.display();
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return if required {
                    Err(self.v(Code::FileMissing, format!("{shown} does not exist")))
                } else {
                    Ok(None)
                };
            }
            Err(e) => return Err(self.unreadable(path, &e)),
        };
        if meta.is_dir() {
            return Err(self.v(
                Code::FileUnreadable,
                format!("{shown} is a directory, not a file"),
            ));
        }
        if let Some(max) = self.decl.max_size {
            if meta.len() > max {
                return Err(self.v(
                    Code::FileTooLarge,
                    format!("{shown} is {} bytes, above maxSize {max}", meta.len()),
                ));
            }
        }
        std::fs::read(path)
            .map(Some)
            .map_err(|e| self.unreadable(path, &e))
    }

    fn unreadable(&self, path: &Path, e: &std::io::Error) -> Violation {
        let hint = if e.kind() == ErrorKind::PermissionDenied {
            " (secret volumes are owned by root: a non-root container needs the pod's fsGroup)"
        } else {
            ""
        };
        self.v(
            Code::FileUnreadable,
            format!("cannot read {}: {e}{hint}", path.display()),
        )
    }
}

/// Where a file input is read from: the `pathEnv` variable when set, else
/// the declared path, under `DOCUCONF_FILE_ROOT` when that is set.
pub(crate) fn resolve_path(decl: &FileDecl, cx: &FileCx) -> PathBuf {
    let p = decl
        .path_env
        .as_ref()
        .and_then(|e| cx.env.get(e))
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| decl.path.clone());
    match &cx.root {
        Some(root) if p.starts_with('/') => root.join(p.trim_start_matches('/')),
        _ => PathBuf::from(p),
    }
}

pub(crate) fn check(decl: &FileDecl, cx: &FileCx) -> Outcome {
    check_at(decl, resolve_path(decl, cx), cx)
}

/// Checks a file input at an already resolved path.
pub(crate) fn check_at(decl: &FileDecl, path: PathBuf, cx: &FileCx) -> Outcome {
    let c = Ctx { decl, path, cx };
    let r = match &decl.kind {
        #[cfg(feature = "tls")]
        FileKind::Tls => crate::pki::tls(&c),
        #[cfg(feature = "tls")]
        FileKind::CaBundle => crate::pki::ca_bundle(&c),
        #[cfg(feature = "keystore")]
        FileKind::Keystore => crate::pki::keystore(&c),
        // Only the types of enabled features can declare these kinds.
        #[allow(unreachable_patterns)]
        FileKind::Tls | FileKind::CaBundle | FileKind::Keystore => {
            unreachable!("file kind {} needs a cargo feature", decl.kind.type_name())
        }
        FileKind::Text => text(&c),
        FileKind::Binary => binary(&c),
        FileKind::Config { schema, bind } => config(&c, schema, *bind),
    };
    match r {
        Ok(Some(b)) => Outcome::Loaded(b),
        Ok(None) => Outcome::Absent,
        Err(v) => Outcome::Failed(v),
    }
}

pub(crate) type R = Result<Option<Box<dyn Any + Send>>, Vec<Violation>>;

fn binary(c: &Ctx) -> R {
    let Some(bytes) = c.read(&c.path, c.decl.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    Ok(Some(Box::new(BinaryFile {
        path: c.path.clone(),
        bytes,
    })))
}

fn text(c: &Ctx) -> R {
    let d = c.decl;
    let Some(bytes) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Err(vec![c.v(Code::FileMalformed, "is not valid UTF-8 text")]);
    };
    let mut viols = Vec::new();
    let n = text.chars().count() as u64;
    if let Some(lo) = d.min_length {
        if n < lo {
            viols.push(c.v(
                Code::OutOfRange,
                format!("is {n} characters, below minLength {lo}"),
            ));
        }
    }
    if let Some(hi) = d.max_length {
        if n > hi {
            viols.push(c.v(
                Code::OutOfRange,
                format!("is {n} characters, above maxLength {hi}"),
            ));
        }
    }
    if let Some((p, re)) = &d.pattern {
        if !re.is_match(&text) {
            viols.push(c.v(Code::PatternMismatch, format!("does not match pattern {p}")));
        }
    }
    if !viols.is_empty() {
        return Err(viols);
    }
    Ok(Some(Box::new(TextFile {
        path: c.path.clone(),
        text,
    })))
}

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Parses a `json`, `yaml` or `toml` document as JSON, so config files and
/// overlays in every format are checked the same way. Any JSON value is a
/// document, not only an object.
pub(crate) fn parse_structured(format: &str, src: &str) -> Result<serde_json::Value, String> {
    match format {
        "yaml" => serde_yaml::from_str(src).map_err(|e| e.to_string()),
        "toml" => Figment::from(Toml::string(src))
            .extract()
            .map_err(|e| e.to_string()),
        _ => serde_json::from_str(src).map_err(|e| e.to_string()),
    }
}

fn config(c: &Ctx, schema: &serde_json::Value, bind: crate::decl::BindFn) -> R {
    let d = c.decl;
    let Some(bytes) = c.read(&c.path, d.required).map_err(|v| vec![v])? else {
        return Ok(None);
    };
    let bytes = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    let format = d.format.as_deref().unwrap_or("json");
    let reason = |e: &dyn std::fmt::Display| {
        if d.secret {
            String::new()
        } else {
            format!(": {e}")
        }
    };
    let Ok(src) = std::str::from_utf8(bytes) else {
        return Err(vec![c.v(Code::FileMalformed, "is not valid UTF-8")]);
    };
    let doc = match parse_structured(format, src) {
        Ok(v) => v,
        Err(e) => {
            let what = format.to_uppercase();
            return Err(vec![c.v(
                Code::FileMalformed,
                format!("is not a valid {what} document{}", reason(&e)),
            )]);
        }
    };
    let viols: Vec<Violation> = crate::schema::validate(schema, &doc, d.secret)
        .into_iter()
        .map(|m| c.v(Code::SchemaMismatch, m))
        .collect();
    if !viols.is_empty() {
        return Err(viols);
    }
    if let Err(e) = bind(&doc) {
        return Err(vec![c.v(
            Code::SchemaMismatch,
            format!("does not bind to the app's type{}", reason(&e)),
        )]);
    }
    Ok(Some(Box::new(ConfigDoc {
        path: c.path.clone(),
        doc,
    })))
}
