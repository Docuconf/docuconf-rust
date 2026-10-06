//! Config-file overlays (SPEC §4.7): one more config file, mounted by the
//! platform, layered between the app's own files and the environment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use figment::providers::{Format, Json, Toml, Yaml};
use figment::{Figment, Profile, Provider, Source};

use crate::decl::{self, Declaration, VarDecl};
use crate::error::{Code, DeclarationError, Violation};

/// How deep a `configKey` may nest (overlays.cue `#MaxKeyDepth`).
const MAX_KEY_DEPTH: usize = 8;

/// The separator between the parts of a `configKey`: figment's key path.
pub(crate) const KEY_SEPARATOR: &str = ".";

/// The format of an overlay file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayFormat {
    /// JSON, read with figment's `Json` provider.
    Json,
    /// YAML, read with figment's `Yaml` provider.
    Yaml,
    /// TOML, read with figment's `Toml` provider.
    Toml,
}

impl OverlayFormat {
    fn as_str(self) -> &'static str {
        match self {
            OverlayFormat::Json => "json",
            OverlayFormat::Yaml => "yaml",
            OverlayFormat::Toml => "toml",
        }
    }

    fn label(self) -> &'static str {
        match self {
            OverlayFormat::Json => "JSON",
            OverlayFormat::Yaml => "YAML",
            OverlayFormat::Toml => "TOML",
        }
    }

    fn from_path(path: &str) -> Option<Self> {
        match path.rsplit_once('.')?.1 {
            "json" => Some(OverlayFormat::Json),
            "yaml" | "yml" => Some(OverlayFormat::Yaml),
            "toml" => Some(OverlayFormat::Toml),
            _ => None,
        }
    }
}

/// When the app picks up a change to an overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Reload {
    /// The overlay is read once at boot; the platform rolls the pods when it
    /// changes.
    #[default]
    Restart,
    /// The app reloads the overlay in place. Not implemented by this SDK
    /// version, so declaring it is a declaration error.
    Watch,
}

/// A config-file overlay: a file the platform mounts and docuconf layers
/// between the app's own config files and the environment (SPEC §4.7).
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
/// use docuconf::Overlay;
///
/// let loader = docuconf::Loader::<Config>::new()
///     .overlay(Overlay::new("platform", "/etc/app/platform/app.toml"));
/// let config = loader.load()?;
/// # Ok::<(), docuconf::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Overlay {
    name: String,
    path: String,
    format: Option<OverlayFormat>,
    description: Option<String>,
    reload: Reload,
}

impl Overlay {
    /// An overlay named `name` (a DNS label such as `platform`), read from
    /// the absolute `path`. Its format comes from the extension (`.toml`,
    /// `.json`, `.yaml`/`.yml`) unless set with [`format`](Self::format).
    ///
    /// The platform mounts the overlay's directory, hiding whatever the
    /// image has there, so give it a directory of its own.
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Overlay {
            name: name.into(),
            path: path.into(),
            format: None,
            description: None,
            reload: Reload::Restart,
        }
    }

    /// Sets the format, when the extension does not say it.
    pub fn format(mut self, format: OverlayFormat) -> Self {
        self.format = Some(format);
        self
    }

    /// What the overlay is for, at least 5 characters.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    /// When the app picks up a change. [`Reload::Watch`] is not implemented
    /// by this SDK version and is rejected.
    pub fn reload(mut self, reload: Reload) -> Self {
        self.reload = reload;
        self
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn description_text(&self) -> Option<&str> {
        self.description.as_deref()
    }

    fn resolved_format(&self) -> Option<OverlayFormat> {
        self.format.or_else(|| OverlayFormat::from_path(&self.path))
    }

    pub(crate) fn format_name(&self) -> &'static str {
        self.resolved_format()
            .map(OverlayFormat::as_str)
            .unwrap_or("")
    }

    pub(crate) fn mount_dir(&self) -> &str {
        match self.path.rfind('/') {
            Some(0) | None => "/",
            Some(i) => &self.path[..i],
        }
    }

    /// The file on disk, under `DOCUCONF_FILE_ROOT` when that is set.
    fn real_path(&self, root: Option<&Path>) -> PathBuf {
        match root {
            Some(r) => r.join(self.path.trim_start_matches('/')),
            None => PathBuf::from(&self.path),
        }
    }

    /// Reads the overlay as a figment layer in the global profile, so it
    /// overrides every profile of the app's files. `Ok(None)` when the file
    /// does not exist: an overlay is optional.
    pub(crate) fn read(&self, root: Option<&Path>) -> Result<Option<Figment>, Violation> {
        let path = self.real_path(root);
        let violation = |code, message| Violation {
            input: self.name.clone(),
            code,
            message,
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(violation(
                    Code::FileUnreadable,
                    format!("overlay {} cannot be read: {e}", path.display()),
                ))
            }
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let format = self.resolved_format().unwrap_or(OverlayFormat::Toml);
        let fig = match format {
            OverlayFormat::Json => Figment::from(Json::string(text).profile(Profile::Global)),
            OverlayFormat::Yaml => Figment::from(Yaml::string(text).profile(Profile::Global)),
            OverlayFormat::Toml => Figment::from(Toml::string(text).profile(Profile::Global)),
        };
        if let Err(e) = fig.data() {
            return Err(violation(
                Code::FileMalformed,
                format!(
                    "overlay {} is not valid {}: {e}",
                    path.display(),
                    format.label()
                ),
            ));
        }
        Ok(Some(fig))
    }
}

/// The directories of the config files in the app's figment.
pub(crate) fn shipped_dirs(figment: Option<&Figment>) -> Vec<PathBuf> {
    let Some(fig) = figment else {
        return Vec::new();
    };
    fig.metadata()
        .filter_map(|m| match &m.source {
            Some(Source::File(p)) if p.is_absolute() => p.parent().map(Path::to_path_buf),
            _ => None,
        })
        .collect()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    a == b || canon(a) == canon(b)
}

/// The `configKey` of each variable an overlay may carry, keyed by variable
/// name: figment's key path, so the platform writes values where the app's
/// struct reads them.
pub(crate) fn config_keys(decl: &Declaration, selector: Option<&str>) -> BTreeMap<String, String> {
    decl.vars
        .iter()
        .filter(|v| eligible(v, selector))
        .map(|v| (v.name.clone(), v.key.join(KEY_SEPARATOR)))
        .collect()
}

fn eligible(v: &VarDecl, selector: Option<&str>) -> bool {
    // Secrets never go in a ConfigMap; the profile selector is read from
    // the environment before any file.
    !v.secret
        && Some(v.name.as_str()) != selector
        && v.key.len() <= MAX_KEY_DEPTH
        && v.key
            .iter()
            .all(|k| !k.is_empty() && !k.contains(KEY_SEPARATOR))
}

/// Checks the overlays against the declaration and the app's own files.
/// `app_dirs` are directories holding files the app ships with: an overlay
/// mounted there would hide them. Returns warnings.
pub(crate) fn check(
    decl: &Declaration,
    overlays: &[Overlay],
    selector: Option<&str>,
    app_dirs: &[PathBuf],
    root: Option<&Path>,
) -> Result<Vec<String>, DeclarationError> {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    let mut names: BTreeSet<&str> = BTreeSet::new();
    let mut dirs: BTreeMap<String, String> = decl
        .files
        .iter()
        .map(|f| (f.mount_dir(), format!("file input {}", f.name)))
        .collect();
    for o in overlays {
        let label = format!("overlay {}", o.name);
        if !decl::is_input_name(&o.name) {
            problems.push(format!(
                "{label}: name must be a DNS label (^[a-z]([-a-z0-9]{{0,40}}[a-z0-9])?$)"
            ));
        }
        if !names.insert(&o.name) {
            problems.push(format!("{label}: declared twice"));
        }
        if let Some(d) = &o.description {
            if d.trim().chars().count() < 5 {
                problems.push(format!(
                    "{label}: description {d:?} is shorter than 5 characters"
                ));
            }
        }
        if !decl::is_abs_path(&o.path) {
            problems.push(format!(
                "{label}: path {:?} must be absolute and normalised (no \"..\", \".\", \"//\" or trailing slash)",
                o.path
            ));
            continue;
        }
        if o.resolved_format().is_none() {
            problems.push(format!(
                "{label}: cannot tell the format from {:?}; call .format(OverlayFormat::Toml) or name the file .toml, .json or .yaml",
                o.path
            ));
        }
        if o.reload == Reload::Watch {
            problems.push(format!(
                "{label}: reload watch is not implemented by this SDK version; use Reload::Restart"
            ));
        }
        let dir = o.mount_dir();
        if decl::RESERVED_DIRS.contains(&dir) {
            problems.push(format!(
                "{label}: would be mounted at {dir}, which hides files the image or OS needs; use a dedicated directory"
            ));
        }
        if let Some(other) = dirs.insert(dir.to_string(), label.clone()) {
            problems.push(format!(
                "{label}: shares mount directory {dir} with {other}"
            ));
        }
        let real_dir = o.real_path(root).parent().map(Path::to_path_buf);
        if let Some(rd) = real_dir {
            if app_dirs.iter().any(|a| same_dir(a, &rd)) {
                problems.push(format!(
                    "{label}: {dir} holds files the app ships with; mounting the overlay there would hide them. Use a directory of its own, such as /etc/app/platform"
                ));
            }
        }
    }
    if !overlays.is_empty() {
        for v in &decl.vars {
            let carried = eligible(v, selector);
            let key = v.key.join(KEY_SEPARATOR);
            if let Some(ck) = &v.config_key {
                if carried && *ck != key {
                    problems.push(format!(
                        "{} ({}): config_key {ck:?} must be the figment key {key:?} when overlays are declared, or be left out",
                        v.name, v.field
                    ));
                }
            }
            if !v.secret && !carried && Some(v.name.as_str()) != selector {
                warnings.push(format!(
                    "{}: key {key} has a \".\" in a part or is deeper than {MAX_KEY_DEPTH} levels, so an overlay cannot carry it",
                    v.name
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(warnings)
    } else {
        Err(DeclarationError { problems })
    }
}
