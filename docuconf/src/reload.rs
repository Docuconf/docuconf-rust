//! `reload: watch` for file inputs (SPEC §4.6.2, §11.2 item 8).
//!
//! A [`Watched<T>`] field rereads its file when it changes. The check is a
//! poll, at most once per interval (a second by default): on access, and on
//! a background thread while an [`on_change`](Watched::on_change) hook is
//! registered. It looks at the metadata of each file the input reads,
//! following symlinks, so the symlink swap Kubernetes uses to update a
//! projected volume shows up as a different file. No inotify, no
//! dependency.

use std::any::Any;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use crate::decl::{FileDecl, FileKind};
use crate::error::{Code, Violation};
use crate::files::{self, FileCx, Outcome};
use crate::value::Typed;

/// Receives each warning, without the `docuconf: ` prefix.
pub(crate) type WarnFn = Arc<dyn Fn(&str) + Send + Sync>;

/// How often a [`Watched`] input looks at its files by default.
pub(crate) const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

/// The shortest pause of the background thread, whatever the interval.
const MIN_BACKGROUND_PAUSE: Duration = Duration::from_millis(10);

thread_local! {
    /// The input whose on-change hooks this thread is running (its
    /// address, 0 for none): a read of that input from inside one of its
    /// hooks returns the current value without looking at the files.
    static IN_HOOK: Cell<usize> = const { Cell::new(0) };
}

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
/// too; `Option<Watched<T>>` is a declaration error. In contract-first
/// mode, [`Values::watched`](crate::contract::Values::watched) gives the
/// same handle.
///
/// [`current`](Watched::current) looks at the files' metadata at most once
/// a second ([`Loader::watch_interval`](crate::Loader::watch_interval)
/// changes that), following symlinks, so the symlink swap Kubernetes makes
/// when it updates a projected volume is seen. When something changed, the
/// input is read again and passes the same checks as at boot. A changed
/// file that fails them is not used: the previous content stays current,
/// the rejection is kept in [`status`](Watched::status), and each problem
/// goes to the loader's [`on_warning`](crate::Loader::on_warning) (stderr
/// by default), with its code and never the file's content. It is reported
/// once per change. A file mounted with `subPath` is never updated by
/// Kubernetes, so mount the directory.
///
/// A keystore is reopened with the password read at boot: environment
/// variables do not change in a running process, so rotating a keystore's
/// password takes a rollout. A new keystore written with another password
/// is rejected (`keystore_unreadable`) and the previous one stays current.
///
/// A value copied out once (a TLS server context, an HTTP client) never
/// sees a reload: call `current()` on every use, or rebuild it in an
/// [`on_change`](Watched::on_change) hook.
///
/// Cloning a `Watched` is cheap and the clones share the current content,
/// hooks and status, so it can be handed to every request handler.
pub struct Watched<T> {
    inner: Arc<Inner<T>>,
}

struct Inner<T> {
    current: RwLock<Arc<T>>,
    /// `None` for [`Watched::new`], which never reloads.
    watcher: Option<(Mutex<Watcher>, Convert<T>)>,
    hooks: Mutex<Hooks<T>>,
    /// Held while hooks run, so the hooks of two reloads never overlap and
    /// run in the order of the reloads.
    notify: Mutex<()>,
    status: Mutex<ReloadStatus>,
    /// The input's name and watch interval, for the background thread.
    name: String,
    interval: Duration,
}

/// Builds the value from what the file checks produced.
type Convert<T> = fn(Option<Box<dyn Any + Send>>) -> Result<T, String>;

type Hook<T> = Arc<dyn Fn(Arc<T>) + Send + Sync>;

struct Hooks<T> {
    list: Vec<(u64, Hook<T>)>,
    next_id: u64,
    /// Whether the background thread is running.
    background: bool,
}

/// The reload state of one watched file input (SPEC §4.6.2), for a health
/// check or a metric. It never holds file content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReloadStatus {
    /// 1 after boot, plus one per accepted reload.
    pub generation: u64,
    /// When the last accepted reload happened; `None` until the input is
    /// reloaded after boot.
    pub last_reload: Option<SystemTime>,
    /// The last changed content that failed its checks and was not used.
    /// An accepted reload clears it.
    pub last_rejected: Option<RejectedReload>,
}

/// A changed file input that failed its checks, so the previous content
/// stayed current. It names the violation codes, never the content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RejectedReload {
    /// When the change was rejected.
    pub time: SystemTime,
    /// The file input's name.
    pub input: String,
    /// The violation codes, in the order found.
    pub codes: Vec<Code>,
}

/// An [`on_change`](Watched::on_change) hook's registration. Dropping it
/// keeps the hook registered; [`unsubscribe`](Subscription::unsubscribe)
/// removes it.
pub struct Subscription {
    remove: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Subscription {
    /// Removes the hook: it is not called for later reloads. The
    /// background thread stops when no hook is left.
    pub fn unsubscribe(mut self) {
        if let Some(f) = self.remove.take() {
            f();
        }
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Subscription")
    }
}

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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl<T> Watched<T> {
    fn with(value: Arc<T>, watcher: Option<(Mutex<Watcher>, Convert<T>)>) -> Self {
        let (name, interval) = match &watcher {
            Some((w, _)) => {
                let w = lock(w);
                (w.source.decl.name.clone(), w.source.interval)
            }
            None => (String::new(), DEFAULT_INTERVAL),
        };
        Watched {
            inner: Arc::new(Inner {
                current: RwLock::new(value),
                watcher,
                hooks: Mutex::new(Hooks {
                    list: Vec::new(),
                    next_id: 0,
                    background: false,
                }),
                notify: Mutex::new(()),
                status: Mutex::new(ReloadStatus {
                    generation: 1,
                    last_reload: None,
                    last_rejected: None,
                }),
                name,
                interval,
            }),
        }
    }

    /// A watched input that always holds `value` and never reloads. Meant
    /// for tests: an app gets its watched inputs from
    /// [`load`](crate::load).
    pub fn new(value: T) -> Self {
        Watched::with(Arc::new(value), None)
    }

    /// The current content. When the files were last looked at more than
    /// the watch interval ago, looks again first, and reloads the input if
    /// one changed. Cheap otherwise: a lock and a clock read.
    pub fn current(&self) -> Arc<T> {
        self.poll(false);
        self.peek()
    }

    /// Looks at the files now, whatever the interval, and reloads the input
    /// if one changed. Returns whether a new value became current. From
    /// inside an [`on_change`](Watched::on_change) hook it does nothing
    /// and returns `false`.
    pub fn refresh(&self) -> bool {
        self.poll(true)
    }

    /// The reload state: the generation, the last accepted reload and the
    /// last rejected change.
    pub fn status(&self) -> ReloadStatus {
        lock(&self.inner.status).clone()
    }

    /// The generation of the current content: 1 after boot, plus one per
    /// accepted reload. The same as `status().generation`.
    pub fn generation(&self) -> u64 {
        lock(&self.inner.status).generation
    }

    fn key(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    fn peek(&self) -> Arc<T> {
        self.inner
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn poll(&self, force: bool) -> bool {
        if IN_HOOK.with(Cell::get) == self.key() {
            return false;
        }
        let Some((watcher, convert)) = &self.inner.watcher else {
            return false;
        };
        // Another thread is already looking: use the current value.
        let mut w = match (force, watcher.try_lock()) {
            (_, Ok(w)) => w,
            (true, Err(_)) => lock(watcher),
            (false, Err(std::sync::TryLockError::Poisoned(e))) => e.into_inner(),
            (false, Err(std::sync::TryLockError::WouldBlock)) => return false,
        };
        let viols = match w.poll(force) {
            Polled::Unchanged => return false,
            Polled::Rejected(v) => v,
            Polled::Changed(b) => match convert(b) {
                Ok(v) => {
                    let v = Arc::new(v);
                    *self
                        .inner
                        .current
                        .write()
                        .unwrap_or_else(|e| e.into_inner()) = v.clone();
                    {
                        let mut s = lock(&self.inner.status);
                        s.generation += 1;
                        s.last_reload = Some(SystemTime::now());
                        s.last_rejected = None;
                    }
                    w.adopted();
                    // Taken before the watcher is released, so hooks run in
                    // the order of the reloads.
                    let notify = lock(&self.inner.notify);
                    let name = w.source.decl.name.clone();
                    let warn = w.source.warn.clone();
                    drop(w);
                    self.run_hooks(&v, &name, warn.as_ref());
                    drop(notify);
                    return true;
                }
                Err(e) => {
                    let msg = if w.source.decl.secret {
                        "does not deserialize into the field's type".to_string()
                    } else {
                        e
                    };
                    vec![Violation {
                        input: w.source.decl.name.clone(),
                        code: Code::SchemaMismatch,
                        message: msg,
                    }]
                }
            },
        };
        w.rejected(&viols);
        lock(&self.inner.status).last_rejected = Some(RejectedReload {
            time: SystemTime::now(),
            input: w.source.decl.name.clone(),
            codes: viols.iter().map(|v| v.code).collect(),
        });
        false
    }

    /// Calls every hook with the new value. A hook that panics is logged by
    /// input name only, and the others still run.
    fn run_hooks(&self, v: &Arc<T>, name: &str, warn: Option<&WarnFn>) {
        let hooks: Vec<Hook<T>> = lock(&self.inner.hooks)
            .list
            .iter()
            .map(|(_, h)| h.clone())
            .collect();
        let outer = IN_HOOK.with(|c| c.replace(self.key()));
        for h in hooks {
            if catch_unwind(AssertUnwindSafe(|| h(v.clone()))).is_err() {
                warn_to(
                    warn,
                    &format!("{name}: an on_change hook panicked; the new content is current"),
                );
            }
        }
        IN_HOOK.with(|c| c.set(outer));
    }
}

impl<T: Send + Sync + 'static> Watched<T> {
    /// Registers a hook, called with the new content after a change passes
    /// every check and becomes current. It is never called for a rejected
    /// change, nor at registration. Several hooks may be registered; they
    /// run one after the other, in the order registered, on the thread that
    /// noticed the change. A hook that panics is logged by input name
    /// (never the panic's message or the content) and the other hooks
    /// still run. Use it to rebuild what is built once from the value: a
    /// TLS server context, an HTTP client, a pool.
    ///
    /// While a hook is registered, a background thread looks at the files
    /// once per watch interval, so hooks run without a read; it stops when
    /// every `Watched` clone is dropped or every hook is unsubscribed. A
    /// [`current`](Watched::current) or [`refresh`](Watched::refresh) that
    /// notices the change first runs the hooks itself, before it returns.
    /// Inside one of its own hooks, an input's `current()` returns the new
    /// content without looking at the files again.
    ///
    /// ```
    /// # use docuconf::Watched;
    /// let banner = Watched::new(String::from("Hello"));
    /// let sub = banner.on_change(|text| println!("new banner: {} bytes", text.len()));
    /// sub.unsubscribe();
    /// ```
    pub fn on_change<F>(&self, hook: F) -> Subscription
    where
        F: Fn(Arc<T>) + Send + Sync + 'static,
    {
        let mut hooks = lock(&self.inner.hooks);
        let id = hooks.next_id;
        hooks.next_id += 1;
        hooks.list.push((id, Arc::new(hook)));
        if !hooks.background && self.inner.watcher.is_some() {
            let interval = self.inner.interval;
            let weak = Arc::downgrade(&self.inner);
            let spawned = std::thread::Builder::new()
                .name(format!("docuconf-watch-{}", self.inner.name))
                .spawn(move || background(weak, interval));
            hooks.background = spawned.is_ok();
        }
        drop(hooks);
        let weak = Arc::downgrade(&self.inner);
        Subscription {
            remove: Some(Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    lock(&inner.hooks).list.retain(|(i, _)| *i != id);
                }
            })),
        }
    }
}

/// The background check of a watched input with hooks: polls once per
/// interval until the input is dropped or has no hook left.
fn background<T>(weak: Weak<Inner<T>>, interval: Duration) {
    let pause = interval.max(MIN_BACKGROUND_PAUSE);
    loop {
        std::thread::sleep(pause);
        let Some(inner) = weak.upgrade() else { return };
        {
            let mut hooks = lock(&inner.hooks);
            if hooks.list.is_empty() {
                hooks.background = false;
                return;
            }
        }
        Watched { inner }.poll(false);
    }
}

impl<T> Watched<T> {
    /// A watched input from what the loader parked.
    pub(crate) fn from_parts(parts: Parts, convert: Convert<T>) -> Result<Self, String> {
        let value = convert(parts.initial)?;
        Ok(Watched::from_source(Arc::new(value), parts.source, convert))
    }

    /// A watched input whose content read at load is `value`.
    pub(crate) fn from_source(value: Arc<T>, source: Source, convert: Convert<T>) -> Self {
        let seen = source.stamps();
        Watched::with(
            value,
            Some((
                Mutex::new(Watcher {
                    next: Instant::now() + source.interval,
                    current: seen.clone(),
                    seen,
                    pending: None,
                    source,
                }),
                convert,
            )),
        )
    }
}

impl<'de, T: WatchedInput> Deserialize<'de> for Watched<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let parts: Parts = crate::types::unpark(d, "watched file input")?;
        Watched::from_parts(parts, T::from_loaded).map_err(D::Error::custom)
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
    /// The keystore password variable's value, if any, as read at boot: a
    /// reload never reads the environment again.
    pub values: HashMap<String, Typed>,
    /// The loader's fixed clock (`Loader::now`), else the real one.
    pub now: Option<SystemTime>,
    pub interval: Duration,
    pub warn: Option<WarnFn>,
}

fn warn_to(f: Option<&WarnFn>, w: &str) {
    match f {
        Some(f) => f(w),
        None => eprintln!("docuconf: {w}"),
    }
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
        // No environment: the path was resolved at boot, and the keystore
        // password is the one read at boot.
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
        warn_to(self.warn.as_ref(), w);
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

enum Polled {
    Unchanged,
    /// A file changed and every check passed: the input read again.
    Changed(Option<Box<dyn Any + Send>>),
    /// A file changed and a check failed.
    Rejected(Vec<Violation>),
}

impl Watcher {
    /// Looks at the files when due, and reads the input again when one
    /// changed since it was last looked at.
    fn poll(&mut self, force: bool) -> Polled {
        let now = Instant::now();
        if !force && now < self.next {
            return Polled::Unchanged;
        }
        self.next = now + self.source.interval;
        let stamps = self.source.stamps();
        if stamps == self.seen {
            return Polled::Unchanged;
        }
        self.seen = stamps.clone();
        if stamps == self.current {
            // Changed and changed back.
            return Polled::Unchanged;
        }
        match self.source.load() {
            Outcome::Loaded(b) => {
                self.pending = Some(stamps);
                Polled::Changed(Some(b))
            }
            Outcome::Absent => {
                self.pending = Some(stamps);
                Polled::Changed(None)
            }
            Outcome::Failed(v) => Polled::Rejected(v),
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
