//! `reload: watch` for file inputs (SPEC §4.6.2, §11.2 item 8).
//!
//! A [`Watched<T>`] field rereads its file when it changes. The check is a
//! poll on access, at most once per interval (a second by default): the
//! metadata of each file the input reads, following symlinks, so the
//! symlink swap Kubernetes uses to update a projected volume shows up as a
//! different file. No thread, no inotify, no dependency.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use crate::decl::{FileDecl, FileKind};
use crate::error::Violation;
use crate::files::{self, FileCx, Outcome};
use crate::value::Typed;

/// Receives each warning, without the `docuconf: ` prefix.
pub(crate) type WarnFn = Arc<dyn Fn(&str) + Send + Sync>;

/// How often a [`Watched`] input looks at its files by default.
pub(crate) const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

/// A file input declared `reload: watch`: the app rereads the file when it
/// changes, so a renewed certificate or an updated ConfigMap reaches it
/// without a restart.
///
/// ```
/// use docuconf::{ConfigFile, TextFile, Watched};
///
/// #[derive(serde::Deserialize, docuconf::Docuconf)]
/// struct Config {
///     /// Feature flags, updated without a rollout.
///     #[docuconf(path = "/etc/app/flags/flags.json")]
///     flags: Watched<ConfigFile<Flags>>,
///
///     /// Banner shown on the home page, when there is one.
///     #[docuconf(path = "/etc/app/banner/banner.txt")]
///     banner: Watched<Option<TextFile>>,
/// }
///
/// #[derive(serde::Deserialize, docuconf::JsonSchema)]
/// #[schemars(crate = "docuconf::schemars")]
/// struct Flags {
///     new_checkout: bool,
/// }
///
/// fn handle(config: &Config) {
///     // The current content: the file as it was at boot, or after its
///     // latest change that passed every check.
///     let flags = config.flags.current();
///     if flags.new_checkout { /* ... */ }
/// }
/// ```
///
/// The field type declares `reload: "watch"` in the contract
/// (`#[docuconf(reload = "watch")]` may say so too); any file input type
/// can be wrapped: [`ConfigFile<T>`](crate::ConfigFile),
/// [`TlsKeyPair`](crate::TlsKeyPair), [`CaBundle`](crate::CaBundle),
/// [`Keystore`](crate::Keystore), [`TextFile`](crate::TextFile) and
/// [`BinaryFile`](crate::BinaryFile). An optional input is
/// `Watched<Option<T>>`, so a file that appears after boot is picked up
/// too; `Option<Watched<T>>` is a declaration error.
///
/// [`current`](Watched::current) looks at the files' metadata at most once
/// a second ([`Loader::watch_interval`](crate::Loader::watch_interval)
/// changes that), following symlinks, so the symlink swap Kubernetes makes
/// when it updates a projected volume is seen. When something changed, the
/// input is read again and passes the same checks as at boot. A changed
/// file that fails them is not used: the previous content stays current and
/// each problem goes to the loader's
/// [`on_warning`](crate::Loader::on_warning) (stderr by default), with its
/// code and never the file's content. It is reported once per change. A
/// file mounted with `subPath` is never updated by Kubernetes, so mount
/// the directory.
///
/// Cloning a `Watched` is cheap and the clones share the current content,
/// so it can be handed to every request handler.
pub struct Watched<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    current: RwLock<Arc<T>>,
    /// `None` for [`Watched::new`], which never reloads.
    watcher: Option<(Mutex<Watcher>, Convert<T>)>,
}

/// Builds the value from what the file checks produced.
type Convert<T> = fn(Option<Box<dyn Any + Send>>) -> Result<T, String>;

impl<T> Clone for Watched<T> {
    fn clone(&self) -> Self {
        Watched {
            inner: self.inner.clone(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Watched<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Watched").field(&*self.peek()).finish()
    }
}

impl<T> Watched<T> {
    /// A watched input that always holds `value` and never reloads. Meant
    /// for tests: an app gets its watched inputs from
    /// [`load`](crate::load).
    pub fn new(value: T) -> Self {
        Watched {
            inner: Arc::new(Inner {
                current: RwLock::new(Arc::new(value)),
                watcher: None,
            }),
        }
    }

    /// The current content. When the files were last looked at more than
    /// the watch interval ago, looks again first, and reloads the input if
    /// one changed. Cheap otherwise: a lock and a clock read.
    pub fn current(&self) -> Arc<T> {
        self.poll(false);
        self.peek()
    }

    /// Looks at the files now, whatever the interval, and reloads the input
    /// if one changed. Returns whether a new value became current.
    pub fn refresh(&self) -> bool {
        self.poll(true)
    }

    fn peek(&self) -> Arc<T> {
        self.inner
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn poll(&self, force: bool) -> bool {
        let Some((watcher, convert)) = &self.inner.watcher else {
            return false;
        };
        // Another thread is already looking: use the current value.
        let mut w = match (force, watcher.try_lock()) {
            (_, Ok(w)) => w,
            (true, Err(_)) => watcher.lock().unwrap_or_else(|e| e.into_inner()),
            (false, Err(std::sync::TryLockError::Poisoned(e))) => e.into_inner(),
            (false, Err(std::sync::TryLockError::WouldBlock)) => return false,
        };
        let Some(b) = w.poll(force) else {
            return false;
        };
        match convert(b) {
            Ok(v) => {
                *self
                    .inner
                    .current
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = Arc::new(v);
                w.adopted();
                true
            }
            Err(e) => {
                let msg = if w.source.decl.secret {
                    "does not deserialize into the field's type".to_string()
                } else {
                    e
                };
                let v = Violation {
                    input: w.source.decl.name.clone(),
                    code: crate::Code::SchemaMismatch,
                    message: msg,
                };
                w.rejected(&[v]);
                false
            }
        }
    }
}

impl<'de, T: WatchedInput> Deserialize<'de> for Watched<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let parts: Parts = crate::types::unpark(d, "watched file input")?;
        let value = T::from_loaded(parts.initial).map_err(D::Error::custom)?;
        let seen = parts.source.stamps();
        Ok(Watched {
            inner: Arc::new(Inner {
                current: RwLock::new(Arc::new(value)),
                watcher: Some((
                    Mutex::new(Watcher {
                        next: Instant::now() + parts.source.interval,
                        current: seen.clone(),
                        seen,
                        pending: None,
                        source: parts.source,
                    }),
                    T::from_loaded,
                )),
            }),
        })
    }
}

/// What the loader parks for a `Watched` field: the content read at boot
/// and how to read it again.
pub(crate) struct Parts {
    pub initial: Option<Box<dyn Any + Send>>,
    pub source: Source,
}

/// Everything needed to check a file input again after boot.
pub(crate) struct Source {
    pub decl: FileDecl,
    /// The resolved path (`pathEnv` and `DOCUCONF_FILE_ROOT` applied).
    pub path: PathBuf,
    /// The keystore password variable's value, if any.
    pub values: HashMap<String, Typed>,
    /// The loader's fixed clock (`Loader::now`), else the real one.
    pub now: Option<SystemTime>,
    pub interval: Duration,
    pub warn: Option<WarnFn>,
}

impl Source {
    /// The files the input reads: a TLS key pair's three, else its path.
    fn paths(&self) -> Vec<PathBuf> {
        match self.decl.kind {
            FileKind::Tls => ["tls.crt", "tls.key", "ca.crt"]
                .iter()
                .map(|n| self.path.join(n))
                .collect(),
            _ => vec![self.path.clone()],
        }
    }

    fn stamps(&self) -> Vec<Option<Stamp>> {
        self.paths().iter().map(|p| Stamp::of(p)).collect()
    }

    fn load(&self) -> Outcome {
        let env = HashMap::new();
        let cx = FileCx {
            root: None,
            env: &env,
            now: self.now.unwrap_or_else(SystemTime::now),
            values: &self.values,
        };
        files::check_at(&self.decl, self.path.clone(), &cx)
    }

    fn warn(&self, w: &str) {
        match &self.warn {
            Some(f) => f(w),
            None => eprintln!("docuconf: {w}"),
        }
    }
}

struct Watcher {
    source: Source,
    next: Instant,
    /// The stamps of the content that is current.
    current: Vec<Option<Stamp>>,
    /// The stamps last looked at, so a rejected change is reported once.
    seen: Vec<Option<Stamp>>,
    /// The stamps of the content being adopted.
    pending: Option<Vec<Option<Stamp>>>,
}

impl Watcher {
    /// Looks at the files when due; returns the input read again when one
    /// changed since it was last looked at and every check passed.
    fn poll(&mut self, force: bool) -> Option<Option<Box<dyn Any + Send>>> {
        let now = Instant::now();
        if !force && now < self.next {
            return None;
        }
        self.next = now + self.source.interval;
        let stamps = self.source.stamps();
        if stamps == self.seen {
            return None;
        }
        self.seen = stamps.clone();
        if stamps == self.current {
            // Changed and changed back.
            return None;
        }
        match self.source.load() {
            Outcome::Loaded(b) => {
                self.pending = Some(stamps);
                Some(Some(b))
            }
            Outcome::Absent => {
                self.pending = Some(stamps);
                Some(None)
            }
            Outcome::Failed(v) => {
                self.rejected(&v);
                None
            }
        }
    }

    fn adopted(&mut self) {
        if let Some(s) = self.pending.take() {
            self.current = s;
        }
        log::info!("docuconf: reloaded file input {}", self.source.decl.name);
    }

    fn rejected(&mut self, viols: &[Violation]) {
        self.pending = None;
        for v in viols {
            self.source.warn(&format!(
                "{}: changed file rejected, keeping the previous content: {} ({})",
                v.input,
                v.message,
                v.code.as_str()
            ));
        }
    }
}

/// What identifies a file's content without reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    /// Follows symlinks, so a swapped `..data` link is a different file.
    fn of(path: &std::path::Path) -> Option<Stamp> {
        let m = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        let (dev, ino) = {
            use std::os::unix::fs::MetadataExt;
            (m.dev(), m.ino())
        };
        #[cfg(not(unix))]
        let (dev, ino) = (0, 0);
        Some(Stamp {
            dev,
            ino,
            len: m.len(),
            modified: m.modified().ok(),
        })
    }
}

/// A file input type that [`Watched`] can wrap: every file input type, and
/// `Option` of one. Implemented by docuconf only.
pub trait WatchedInput: Sized + Send + Sync + 'static {
    /// Builds the value from what the file checks produced (`None` when the
    /// file is absent).
    #[doc(hidden)]
    fn from_loaded(b: Option<Box<dyn Any + Send>>) -> Result<Self, String>;
}

impl<T: WatchedInput> WatchedInput for Option<T> {
    fn from_loaded(b: Option<Box<dyn Any + Send>>) -> Result<Self, String> {
        match b {
            None => Ok(None),
            some => T::from_loaded(some).map(Some),
        }
    }
}

fn take<T: 'static>(b: Option<Box<dyn Any + Send>>, what: &str) -> Result<T, String> {
    b.ok_or_else(|| format!("{what} is absent"))?
        .downcast::<T>()
        .map(|b| *b)
        .map_err(|_| format!("docuconf: field type does not match the {what}"))
}

macro_rules! watched_input {
    ($($(#[$m:meta])* $t:ty => $what:literal),*) => {$(
        $(#[$m])*
        impl WatchedInput for $t {
            fn from_loaded(b: Option<Box<dyn Any + Send>>) -> Result<Self, String> {
                take(b, $what)
            }
        }
    )*};
}
watched_input!(
    #[cfg(feature = "tls")]
    crate::TlsKeyPair => "TLS key pair",
    #[cfg(feature = "tls")]
    crate::CaBundle => "CA bundle",
    #[cfg(feature = "keystore")]
    crate::Keystore => "keystore",
    crate::TextFile => "text file",
    crate::BinaryFile => "binary file"
);

impl<T> WatchedInput for crate::ConfigFile<T>
where
    T: serde::de::DeserializeOwned + Send + Sync + 'static,
{
    fn from_loaded(b: Option<Box<dyn Any + Send>>) -> Result<Self, String> {
        let doc: crate::types::ConfigDoc = take(b, "config file")?;
        crate::types::config_file(doc).map_err(|e| e.to_string())
    }
}
