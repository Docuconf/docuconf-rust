//! Boot: load the declared variables and files through figment, checking
//! everything and reporting every violation together.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use figment::providers::Serialized;
use figment::value::{Dict, Value};
use figment::{Figment, Profile};
use serde::de::DeserializeOwned;

use crate::decl::{self, Declaration};
use crate::env::{self, Env};
use crate::error::{Code, DeclarationError, Error, ValidationError, Violation};
use crate::export::{self, Meta, Profiles};
use crate::files::{self, FileCx, Outcome};
use crate::overlay::{self, Overlay};
use crate::types;
use crate::value::{self, Typed};
use crate::Docuconf;

/// Receives each warning `load` reports, without the `docuconf: ` prefix.
type WarnFn = Arc<dyn Fn(&str) + Send + Sync>;

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
    overlays: Vec<Overlay>,
    env: Option<HashMap<String, String>>,
    dotenv: Option<PathBuf>,
    now: Option<SystemTime>,
    termination_log: Option<bool>,
    warn: Option<WarnFn>,
    _c: PhantomData<fn() -> C>,
}

impl<C> Default for Loader<C> {
    fn default() -> Self {
        Loader {
            figment: None,
            profiles: None,
            overlays: Vec::new(),
            env: None,
            dotenv: None,
            now: None,
            termination_log: None,
            warn: None,
            _c: PhantomData,
        }
    }
}

impl<C> std::fmt::Debug for Loader<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader")
            .field("profiles", &self.profiles)
            .field("overlays", &self.overlays)
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

    /// Adds a config-file overlay the platform mounts (SPEC §4.7). It is
    /// layered above the app's figment, profiles included, and below the
    /// environment, and is optional: a missing file is fine. Exported as
    /// the contract's `overlays`, with a `configKey` (figment's dotted key
    /// path, `keySeparator: "."`) on every variable it may carry.
    pub fn overlay(mut self, overlay: Overlay) -> Self {
        self.overlays.push(overlay);
        self
    }

    /// Reads variables from this map instead of the process environment.
    /// Meant for tests: the process environment is neither read nor
    /// changed, and nothing is written to the termination log unless
    /// [`termination_log(true)`](Loader::termination_log) asks for it. Put
    /// `DOCUCONF_FILE_ROOT` in the map to read file inputs from a test
    /// directory.
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

    /// Whether to write the problems to `/dev/termination-log` (or
    /// `DOCUCONF_TERMINATION_LOG`) when loading fails, so `kubectl describe
    /// pod` shows them. On by default, off when [`env`](Loader::env)
    /// replaces the process environment.
    pub fn termination_log(mut self, on: bool) -> Self {
        self.termination_log = Some(on);
        self
    }

    /// Where `load` sends warnings: a set variable that looks like a typo of
    /// a declared one, a deprecated variable that is set, a feature-flag
    /// name. By default each is printed to stderr as `docuconf: <warning>`.
    /// Pass `|w| tracing::warn!("{w}")` to route them to your logger, or
    /// `|_| {}` to drop them.
    pub fn on_warning(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.warn = Some(Arc::new(f));
        self
    }

    fn warn(&self, w: &str) {
        match &self.warn {
            Some(f) => f(w),
            None => eprintln!("docuconf: {w}"),
        }
    }

    /// Exports the contract, including the values in the app's config files
    /// and its profiles. Warnings go to the `log` crate; use
    /// [`export_with_warnings`](Loader::export_with_warnings) to get them.
    pub fn export(&self, meta: &Meta) -> Result<String, DeclarationError> {
        let out = self.export_with_warnings(meta)?;
        for w in &out.warnings {
            log::warn!("docuconf: {w}");
        }
        Ok(out.cue)
    }

    /// Exports the contract, returning the warnings with it: feature-flag
    /// names, config-file keys that are not declared variables.
    pub fn export_with_warnings(&self, meta: &Meta) -> Result<Export, DeclarationError> {
        let decl = decl::declaration::<C>()?;
        let mut warnings = decl.warnings.clone();
        let values = match &self.figment {
            None => None,
            Some(fig) => {
                let fv = export::file_values(&decl, fig)?;
                warnings.extend(fv.warnings.iter().cloned());
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
        warnings.extend(overlay::check(
            &decl,
            &self.overlays,
            self.selector(),
            &overlay::shipped_dirs(self.figment.as_ref()),
            None,
        )?);
        let cue = export::render(
            &decl,
            meta,
            values.as_ref(),
            self.profiles.as_ref(),
            &self.overlays,
        )?;
        Ok(Export { cue, warnings })
    }

    /// Runs the export command when the program was started as
    /// `<program> export [--check] [PATH]`, then exits; otherwise returns
    /// and `main` carries on. Call it first thing in `main`:
    ///
    /// - `<program> export contract.cue` writes the contract (the default
    ///   path is `contract.cue`);
    /// - `<program> export --check contract.cue` exits 1, showing the first
    ///   difference, when the committed contract is stale. Run it in CI.
    ///
    /// Warnings and errors go to stderr. See also
    /// [`assert_contract`](Loader::assert_contract), the same check as a
    /// unit test.
    pub fn export_command(&self, meta: &Meta) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("export") {
            std::process::exit(self.run_export(meta, &args[1..]));
        }
    }

    /// The export command on `args` (after `export`); returns the exit code.
    pub(crate) fn run_export(&self, meta: &Meta, args: &[String]) -> i32 {
        let mut check = false;
        let mut path = None;
        for a in args {
            match a.as_str() {
                "--check" => check = true,
                s if !s.starts_with('-') && path.is_none() => path = Some(s.to_string()),
                other => {
                    eprintln!(
                        "docuconf: unexpected argument {other:?}; usage: export [--check] [PATH]"
                    );
                    return 2;
                }
            }
        }
        let path = path.unwrap_or_else(|| "contract.cue".to_string());
        let out = match self.export_with_warnings(meta) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        for w in &out.warnings {
            eprintln!("docuconf: warning: {w}");
        }
        if check {
            match check_contract(&out.cue, Path::new(&path)) {
                Ok(()) => {
                    eprintln!("docuconf: {path} is up to date");
                    0
                }
                Err(e) => {
                    eprintln!("{e}\nre-export it with: <program> export {path}");
                    1
                }
            }
        } else {
            match std::fs::write(&path, &out.cue) {
                Ok(()) => {
                    eprintln!("docuconf: wrote {path}");
                    0
                }
                Err(e) => {
                    eprintln!("docuconf: cannot write {path}: {e}");
                    1
                }
            }
        }
    }

    /// Panics when the contract at `path` is not what this declaration
    /// exports, showing the first difference; with `UPDATE_CONTRACT=1` in
    /// the environment it rewrites the file instead. A unit test that keeps
    /// a committed `contract.cue` current:
    ///
    /// ```no_run
    /// # use serde::Deserialize;
    /// # #[derive(Deserialize, docuconf::Docuconf)]
    /// # struct Config {
    /// #     /// HTTP listen port.
    /// #     #[docuconf(default = 8080)]
    /// #     port: u16,
    /// # }
    /// #[test]
    /// fn contract_is_current() {
    ///     docuconf::Loader::<Config>::new()
    ///         .assert_contract(&docuconf::Meta::new("billing-api"), "contract.cue");
    /// }
    /// ```
    #[track_caller]
    pub fn assert_contract(&self, meta: &Meta, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let cue = self.export(meta).unwrap_or_else(|e| panic!("{e}"));
        if std::env::var_os("UPDATE_CONTRACT").is_some_and(|v| v == "1") {
            std::fs::write(path, cue)
                .unwrap_or_else(|e| panic!("docuconf: cannot write {}: {e}", path.display()));
            return;
        }
        if let Err(e) = check_contract(&cue, path) {
            panic!("{e}\nrun the test again with UPDATE_CONTRACT=1 to rewrite it");
        }
    }

    fn selector(&self) -> Option<&str> {
        self.profiles.as_ref().map(|p| p.selector.as_str())
    }

    fn environment(&self) -> Env {
        let mut env = match &self.env {
            Some(e) => Env::from_map(e.clone()),
            None => Env::process(),
        };
        if let Some(path) = &self.dotenv {
            if let Ok(iter) = dotenvy::from_path_iter(path) {
                for (k, v) in iter.flatten() {
                    env.add_default(k, v);
                }
            }
        }
        env
    }

    /// Loads and checks every declared variable and file input. On failure,
    /// every problem is reported together, and written to the termination
    /// log. Warnings go to [`on_warning`](Loader::on_warning) (stderr by
    /// default).
    pub fn load(&self) -> Result<C, Error> {
        let env = self.environment();
        let out = self.load_from(&env);
        if let Err(e) = &out {
            // With `env(map)`, only a log path the map names explicitly.
            let default = self.env.is_none() || env.vars.contains_key("DOCUCONF_TERMINATION_LOG");
            if self.termination_log.unwrap_or(default) {
                write_termination_log(&env.vars, e);
            }
        }
        out
    }

    /// Loads like [`load`](Loader::load); on failure prints the report to
    /// stderr and exits with status 1, with no panic and no backtrace. The
    /// usual first line of `main`.
    pub fn load_or_exit(&self) -> C {
        self.load().unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        })
    }

    fn load_from(&self, env: &Env) -> Result<C, Error> {
        let decl = decl::declaration::<C>()?;
        if let Some(p) = &self.profiles {
            export::check_selector(&decl, &p.selector)?;
        }
        for w in &decl.warnings {
            self.warn(w);
        }
        for w in env::typo_hints(&decl.vars, C::PREFIX, &extra_names(&decl), env) {
            self.warn(&w);
        }
        for var in &decl.vars {
            if let Some(w) = env::deprecation(var, env) {
                self.warn(&w);
            }
        }
        if let (Some(p), Some(app)) = (&self.profiles, &self.figment) {
            if let Some(selected) = env.get(&p.selector).filter(|s| !s.is_empty()) {
                if let Some(w) = unknown_profile(p, app, selected) {
                    self.warn(&w);
                }
            }
        }
        if !self.overlays.is_empty() {
            let mut app_dirs = overlay::shipped_dirs(self.figment.as_ref());
            if let Some(dir) = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
            {
                app_dirs.push(dir);
            }
            overlay::check(
                &decl,
                &self.overlays,
                self.selector(),
                &app_dirs,
                file_root(&env.vars).as_deref(),
            )?;
        }
        self.load_with(&decl, env).map_err(|mut e| {
            sort_violations(&decl, &mut e.violations);
            Error::Validation(e)
        })
    }

    fn load_with(&self, decl: &Declaration, env: &Env) -> Result<C, ValidationError> {
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
            match env::read(var, env) {
                Ok(None) => {}
                Ok(Some(t)) => insert_path(&mut env_layer, &var.key, t.to_figment()),
                Err(v) => {
                    viols.push(v);
                    failed.insert(var.name.clone());
                }
            }
        }

        // 2. Layers: declaration defaults < app files < overlays <
        //    environment (SPEC §4.7).
        let root = file_root(&env.vars);
        let profile = match &self.profiles {
            Some(p) => env
                .get(&p.selector)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| p.default.clone()),
            None => Profile::Default.as_str().to_string(),
        };
        let mut fig = Figment::new().merge(Serialized::defaults(defaults));
        if let Some(app) = &self.figment {
            fig = fig.merge(app.clone());
        }
        for o in &self.overlays {
            match o.read(root.as_deref()) {
                Ok(Some(layer)) => fig = fig.merge(layer),
                Ok(None) => {}
                Err(v) => viols.push(v),
            }
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
                        viols.push(env::missing(var));
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
                    Ok(t) => match env::finish(var, t) {
                        Ok(t) => {
                            values.insert(var.name.clone(), t);
                        }
                        Err(v) => {
                            failed.insert(var.name.clone());
                            viols.extend(v);
                        }
                    },
                },
            }
        }

        // 4. Files.
        let fcx = FileCx {
            root,
            env: &env.vars,
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

pub(crate) fn file_root(env: &HashMap<String, String>) -> Option<PathBuf> {
    env.get("DOCUCONF_FILE_ROOT")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// What [`Loader::export_with_warnings`] returns.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Export {
    /// The contract, as CUE.
    pub cue: String,
    /// Warnings about the declaration, one line each.
    pub warnings: Vec<String>,
}

/// Compares an exported contract with the file at `path`.
pub(crate) fn check_contract(cue: &str, path: &Path) -> Result<(), String> {
    let shown = path.display();
    let old = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("docuconf: {shown} does not exist"));
        }
        Err(e) => return Err(format!("docuconf: cannot read {shown}: {e}")),
    };
    if old == cue {
        return Ok(());
    }
    let (old_lines, new_lines): (Vec<&str>, Vec<&str>) =
        (old.lines().collect(), cue.lines().collect());
    let i = old_lines
        .iter()
        .zip(&new_lines)
        .position(|(a, b)| a != b)
        .unwrap_or(old_lines.len().min(new_lines.len()));
    let line = |l: &[&str]| {
        l.get(i)
            .map_or("(end of file)".to_string(), |s| s.to_string())
    };
    Err(format!(
        "docuconf: {shown} is out of date; first difference at line {}:\n  - {}\n  + {}",
        i + 1,
        line(&old_lines),
        line(&new_lines)
    ))
}

/// Names besides the declared variables that a set variable may be
/// mistaken for, for typo hints: `path_env` overrides.
fn extra_names(decl: &Declaration) -> Vec<String> {
    decl.files
        .iter()
        .filter_map(|f| f.path_env.clone())
        .collect()
}

/// A warning when the profile selector names a profile that none of the
/// app's config files define (and that is not the default): only base
/// values and the environment apply. The spec allows such a profile (the
/// platform supplies its values), so this is not a violation, but it is
/// usually a typo.
fn unknown_profile(p: &Profiles, app: &Figment, selected: &str) -> Option<String> {
    if selected.eq_ignore_ascii_case(&p.default) {
        return None;
    }
    let mut known: Vec<String> = app
        .profiles()
        .filter(|pr| **pr != Profile::Default && **pr != Profile::Global)
        .map(|pr| pr.as_str().as_str().to_string())
        .collect();
    if known.iter().any(|k| k.eq_ignore_ascii_case(selected)) {
        return None;
    }
    if !known.iter().any(|k| k.eq_ignore_ascii_case(&p.default)) {
        known.push(p.default.clone());
    }
    known.sort();
    Some(format!(
        "{} selects profile {}, which no config file defines, so only base values and the environment apply; the config files define {}",
        p.selector,
        Typed::Str(selected.to_string()).show(),
        known.join(", ")
    ))
}

/// Orders violations by input: variables by name, then file inputs by
/// name, then anything else (an overlay), so the report reads the same
/// whatever stage found each problem.
fn sort_violations(decl: &Declaration, v: &mut [Violation]) {
    v.sort_by_key(|x| {
        let group = if decl.var(&x.input).is_some() {
            0
        } else if decl.files.iter().any(|f| f.name == x.input) {
            1
        } else {
            2
        };
        (group, x.input.clone())
    });
}

pub(crate) fn write_termination_log(env: &HashMap<String, String>, e: &dyn Display) {
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

#[cfg(test)]
mod tests {
    use crate::{Docuconf, Loader, Meta};
    use serde::Deserialize;

    #[derive(Deserialize, Docuconf)]
    #[allow(dead_code)]
    struct Svc {
        /// HTTP listen port.
        #[docuconf(default = 8080)]
        port: u16,
    }

    fn run(args: &[&str]) -> i32 {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        Loader::<Svc>::new().run_export(&Meta::new("svc"), &args)
    }

    #[test]
    fn export_command_writes_then_checks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contract.cue");
        let p = path.to_str().unwrap();
        // Missing file: --check fails.
        assert_eq!(run(&["--check", p]), 1);
        assert_eq!(run(&[p]), 0);
        assert!(std::fs::read_to_string(&path).unwrap().contains("PORT: {"));
        assert_eq!(run(&["--check", p]), 0);
        std::fs::write(&path, "stale").unwrap();
        assert_eq!(run(&[p, "--check"]), 1);
        assert_eq!(run(&["--bogus"]), 2);
        assert_eq!(run(&[p, "extra"]), 2);
    }

    #[test]
    fn check_contract_shows_the_first_difference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.cue");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        assert!(super::check_contract("a\nb\nc\n", &path).is_ok());
        let e = super::check_contract("a\nB\nc\n", &path).unwrap_err();
        assert!(
            e.ends_with("first difference at line 2:\n  - b\n  + B"),
            "{e}"
        );
    }
}
