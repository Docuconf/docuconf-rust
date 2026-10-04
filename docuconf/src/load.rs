//! Boot: load the declared variables and files through figment, checking
//! everything and reporting every violation together.

use std::collections::{BTreeSet, HashMap};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::time::SystemTime;

use figment::providers::Serialized;
use figment::value::{Dict, Value};
use figment::{Figment, Profile};
use serde::de::DeserializeOwned;

use crate::decl::{self, Declaration};
use crate::error::{Code, DeclarationError, Error, ValidationError, Violation};
use crate::export::{self, Meta, Profiles};
use crate::files::{self, FileCx, Outcome};
use crate::types;
use crate::value::{self, Typed};
use crate::Docuconf;

/// Loads a `#[derive(Docuconf)]` struct, with options.
///
/// ```no_run
/// # use serde::Deserialize;
/// # use docuconf::Docuconf;
/// # #[derive(Deserialize, Docuconf)]
/// # struct Config {
/// #     /// HTTP listen port.
/// #     #[docuconf(default = 8080)]
/// #     port: u16,
/// # }
/// use figment::{Figment, providers::{Format, Toml}};
///
/// let config: Config = docuconf::Loader::new()
///     // The app's own config files; values in them are exported as defaults.
///     .figment(Figment::from(Toml::file("App.toml").nested()))
///     .profiles("APP_PROFILE", "production")
///     .load()?;
/// # Ok::<(), docuconf::Error>(())
/// ```
pub struct Loader<C> {
    figment: Option<Figment>,
    profiles: Option<Profiles>,
    env: Option<HashMap<String, String>>,
    dotenv: Option<PathBuf>,
    now: Option<SystemTime>,
    termination_log: bool,
    _c: PhantomData<fn() -> C>,
}

impl<C> Default for Loader<C> {
    fn default() -> Self {
        Loader {
            figment: None,
            profiles: None,
            env: None,
            dotenv: None,
            now: None,
            termination_log: true,
            _c: PhantomData,
        }
    }
}

impl<C> std::fmt::Debug for Loader<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader")
            .field("profiles", &self.profiles)
            .field("dotenv", &self.dotenv)
            .finish_non_exhaustive()
    }
}

fn insert_path(dict: &mut Dict, key: &[String], v: Value) {
    let (first, rest) = key.split_first().expect("non-empty key");
    if rest.is_empty() {
        dict.insert(first.clone(), v);
        return;
    }
    let entry = dict
        .entry(first.clone())
        .or_insert_with(|| Value::from(Dict::new()));
    if !matches!(entry, Value::Dict(..)) {
        *entry = Value::from(Dict::new());
    }
    if let Value::Dict(_, d) = entry {
        insert_path(d, rest, v);
    }
}

impl<C: Docuconf + DeserializeOwned> Loader<C> {
    /// A loader reading the process environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// The app's own figment layers, typically config files such as
    /// `Toml::file("App.toml").nested()`. docuconf puts the declaration's
    /// defaults below them and the environment above them, so the
    /// platform's variables override file values (SPEC §4.4). Do not add
    /// figment's `Env` provider: docuconf reads the declared variables
    /// itself.
    pub fn figment(mut self, figment: Figment) -> Self {
        self.figment = Some(figment);
        self
    }

    /// Selects a figment profile by the declared variable `selector`, or
    /// `default` when it is unset. Exported as the contract's `profiles`.
    pub fn profiles(mut self, selector: &str, default: &str) -> Self {
        self.profiles = Some(Profiles {
            selector: selector.to_string(),
            default: default.to_string(),
        });
        self
    }

    /// Reads variables from this map instead of the process environment.
    /// Meant for tests.
    pub fn env<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env = Some(
            vars.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        );
        self
    }

    /// Also reads a `.env` file, for local development. Real environment
    /// variables override it, and a missing file is ignored.
    pub fn dotenv(mut self, path: impl Into<PathBuf>) -> Self {
        self.dotenv = Some(path.into());
        self
    }

    /// The time certificates are checked against. Meant for tests.
    pub fn now(mut self, now: SystemTime) -> Self {
        self.now = Some(now);
        self
    }

    /// Whether to write violations to `/dev/termination-log` (or
    /// `DOCUCONF_TERMINATION_LOG`). On by default.
    pub fn termination_log(mut self, on: bool) -> Self {
        self.termination_log = on;
        self
    }

    /// Exports the contract, including the values in the app's config files
    /// and its profiles.
    pub fn export(&self, meta: &Meta) -> Result<String, DeclarationError> {
        let decl = decl::declaration::<C>()?;
        for w in &decl.warnings {
            log::warn!("docuconf: {w}");
        }
        let values = match &self.figment {
            None => None,
            Some(fig) => {
                let fv = export::file_values(&decl, fig)?;
                for w in &fv.warnings {
                    log::warn!("docuconf: {w}");
                }
                if self.profiles.is_none() {
                    if let Some(name) = fv.profiles.keys().next() {
                        return Err(DeclarationError {
                            problems: vec![format!(
                                "the config files have a profile {name:?}; call Loader::profiles(selector, default) to say how it is selected"
                            )],
                        });
                    }
                }
                Some(fv)
            }
        };
        export::render(&decl, meta, values.as_ref(), self.profiles.as_ref())
    }

    fn environment(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = match &self.env {
            Some(e) => e.clone(),
            None => std::env::vars_os()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect(),
        };
        if let Some(path) = &self.dotenv {
            if let Ok(iter) = dotenvy::from_path_iter(path) {
                for (k, v) in iter.flatten() {
                    env.entry(k).or_insert(v);
                }
            }
        }
        env
    }

    /// Loads and checks every declared variable and file input. On failure,
    /// every violation is reported together, and written to the
    /// termination log.
    pub fn load(&self) -> Result<C, Error> {
        let decl = decl::declaration::<C>()?;
        for w in &decl.warnings {
            log::warn!("docuconf: {w}");
        }
        let env = self.environment();
        match self.load_with(&decl, &env) {
            Ok(c) => Ok(c),
            Err(e) => {
                if self.termination_log {
                    write_termination_log(&env, &e);
                }
                Err(Error::Validation(e))
            }
        }
    }

    fn load_with(
        &self,
        decl: &Declaration,
        env: &HashMap<String, String>,
    ) -> Result<C, ValidationError> {
        let mut viols: Vec<Violation> = Vec::new();
        let mut failed: BTreeSet<String> = BTreeSet::new();
        let violation = |input: &str, code: Code, message: String| Violation {
            input: input.to_string(),
            code,
            message,
        };

        // 1. The environment, parsed in its wire form.
        let mut defaults = Dict::new();
        let mut env_layer = Dict::new();
        for var in &decl.vars {
            if let Some(d) = &var.default {
                insert_path(&mut defaults, &var.key, d.to_figment());
            }
            let Some(raw) = env.get(&var.name) else {
                continue;
            };
            // An empty string is unset for every type but string (SPEC §5).
            if raw.is_empty() && !matches!(var.kind, crate::decl::VarKind::String) {
                continue;
            }
            if let Some(msg) = &var.deprecated {
                let by = var
                    .replaced_by
                    .as_ref()
                    .map(|r| format!("; use {r}"))
                    .unwrap_or_default();
                log::warn!("docuconf: {} is deprecated: {msg}{by}", var.name);
            }
            match value::parse_wire(&var.kind, raw) {
                Ok(t) => insert_path(&mut env_layer, &var.key, t.to_figment()),
                Err(e) => {
                    let shown = if var.secret {
                        "value".to_string()
                    } else {
                        Typed::Str(raw.clone()).show()
                    };
                    viols.push(violation(
                        &var.name,
                        Code::InvalidType,
                        format!("{shown} {e}"),
                    ));
                    failed.insert(var.name.clone());
                }
            }
        }

        // 2. Layers: declaration defaults < app files < environment.
        let profile = match &self.profiles {
            Some(p) => env
                .get(&p.selector)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| p.default.clone()),
            None => Profile::Default.as_str().to_string(),
        };
        let mut fig = Figment::new().merge(Serialized::defaults(defaults));
        if let Some(app) = &self.figment {
            fig = fig.merge(app.clone());
        }
        fig = fig
            .merge(Serialized::globals(env_layer))
            .select(profile.as_str());

        // 3. Every variable's final value against its constraints.
        let mut values: HashMap<String, Typed> = HashMap::new();
        for var in &decl.vars {
            if failed.contains(&var.name) {
                continue;
            }
            match fig.find_value(&var.key_path()) {
                Err(_) => {
                    if var.required {
                        viols.push(violation(
                            &var.name,
                            Code::MissingRequired,
                            "is required but not set".into(),
                        ));
                        failed.insert(var.name.clone());
                    }
                }
                Ok(v) => match value::from_figment(&var.kind, &v) {
                    Err(e) => {
                        let shown = if var.secret {
                            "value"
                        } else {
                            "config file value"
                        };
                        viols.push(violation(
                            &var.name,
                            Code::InvalidType,
                            format!("{shown} {e}"),
                        ));
                        failed.insert(var.name.clone());
                    }
                    Ok(t) => {
                        let problems = value::check(var, &t);
                        if problems.is_empty() {
                            values.insert(var.name.clone(), t);
                        } else {
                            failed.insert(var.name.clone());
                            let hint = match (&t, var.secret) {
                                (Typed::Str(s), true) if s.ends_with('\n') => {
                                    " (the value ends in a newline: was the secret created from a file?)"
                                }
                                _ => "",
                            };
                            for (code, msg) in problems {
                                viols.push(violation(&var.name, code, format!("{msg}{hint}")));
                            }
                        }
                    }
                },
            }
        }

        // 4. Files.
        let root = env
            .get("DOCUCONF_FILE_ROOT")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let fcx = FileCx {
            root,
            env,
            now: self.now.unwrap_or_else(SystemTime::now),
            values: &values,
        };
        let mut file_layer = Dict::new();
        let mut tokens = Vec::new();
        for f in &decl.files {
            match files::check(f, &fcx) {
                Outcome::Absent => {}
                Outcome::Failed(v) => viols.extend(v),
                Outcome::Loaded(b) => {
                    let token = types::park(b);
                    insert_path(&mut file_layer, &f.key, Value::from(token.clone()));
                    tokens.push(token);
                }
            }
        }
        let discard = |tokens: &[String]| tokens.iter().for_each(|t| types::discard(t));
        if !viols.is_empty() {
            discard(&tokens);
            return Err(ValidationError { violations: viols });
        }

        // 5. The host binds the struct.
        let fig = fig.merge(Serialized::globals(file_layer));
        let out = fig.extract::<C>();
        discard(&tokens);
        out.map_err(|errs| {
            let violations = errs
                .into_iter()
                .map(|e| {
                    let path = e.path.join(".");
                    let var = decl.vars.iter().find(|v| v.key_path() == path);
                    let file = decl.files.iter().find(|f| f.key_path() == path);
                    let input = var
                        .map(|v| v.name.clone())
                        .or_else(|| file.map(|f| f.name.clone()))
                        .unwrap_or_else(|| {
                            if path.is_empty() {
                                "config".into()
                            } else {
                                path.clone()
                            }
                        });
                    let secret = var.map(|v| v.secret).unwrap_or(false)
                        || file.map(|f| f.secret).unwrap_or(false);
                    let message = if secret {
                        "value does not deserialize into the field's type".to_string()
                    } else {
                        e.kind.to_string()
                    };
                    let code = if file.is_some() {
                        Code::SchemaMismatch
                    } else {
                        Code::InvalidType
                    };
                    Violation {
                        input,
                        code,
                        message,
                    }
                })
                .collect();
            ValidationError { violations }
        })
    }
}

fn write_termination_log(env: &HashMap<String, String>, e: &ValidationError) {
    let path = match env
        .get("DOCUCONF_TERMINATION_LOG")
        .filter(|s| !s.is_empty())
    {
        Some(p) => PathBuf::from(p),
        None => {
            let p = PathBuf::from("/dev/termination-log");
            if !p.exists() {
                return;
            }
            p
        }
    };
    let _ = std::fs::write(path, format!("{e}\n"));
}
