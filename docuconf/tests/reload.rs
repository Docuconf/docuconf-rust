//! `reload: watch` (SPEC §11.2 item 8): `Watched<T>` file inputs reread
//! their files when they change, including the symlink swap Kubernetes
//! makes when it updates a projected volume.
#![cfg(all(unix, feature = "tls", feature = "keystore"))]

mod common;

use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{leaf, now, World};
use docuconf::{ConfigFile, Docuconf, Loader, Meta, TextFile, TlsKeyPair, Watched};
use serde::Deserialize;

#[derive(Debug, Deserialize, docuconf::JsonSchema)]
#[schemars(crate = "docuconf::schemars")]
#[serde(deny_unknown_fields)]
struct Settings {
    name: String,
    #[schemars(range(min = 1))]
    replicas: i64,
}

#[derive(Debug, Deserialize, Docuconf)]
struct App {
    /// Application settings.
    #[docuconf(path = "/etc/app/settings/settings.json", reload = "watch")]
    settings: Watched<ConfigFile<Settings>>,

    /// Banner shown on the home page.
    #[docuconf(path = "/etc/app/banner/banner.txt", max_length = 20)]
    banner: Watched<Option<TextFile>>,

    /// Upstream credentials, rotated by an agent.
    #[docuconf(path = "/etc/app/creds/creds.json", secret)]
    creds: Watched<ConfigFile<Creds>>,
}

#[derive(Debug, Deserialize, docuconf::JsonSchema)]
#[schemars(crate = "docuconf::schemars")]
struct Creds {
    token: String,
}

/// A mount directory laid out like the kubelet's AtomicWriter: each version
/// in its own timestamped directory, `..data` a symlink to the current one,
/// and every file a symlink through `..data`.
struct Mount {
    dir: std::path::PathBuf,
    version: u32,
}

impl Mount {
    fn new(dir: std::path::PathBuf, files: &[(&str, &str)]) -> Mount {
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = Mount { dir, version: 0 };
        m.update(files);
        for (name, _) in files {
            symlink(Path::new("..data").join(name), m.dir.join(name)).unwrap();
        }
        m
    }

    /// Writes a new version and swaps `..data` to it with a rename, as the
    /// kubelet does.
    fn update(&mut self, files: &[(&str, &str)]) {
        self.version += 1;
        let ts = format!("..2026_10_09_12_00_{:02}.{}", self.version, self.version);
        let vdir = self.dir.join(&ts);
        std::fs::create_dir(&vdir).unwrap();
        for (name, content) in files {
            std::fs::write(vdir.join(name), content).unwrap();
        }
        let tmp = self.dir.join("..data_tmp");
        symlink(&ts, &tmp).unwrap();
        std::fs::rename(&tmp, self.dir.join("..data")).unwrap();
    }
}

struct Fixture {
    root: tempfile::TempDir,
    settings: Mount,
    creds: Mount,
    warnings: Arc<Mutex<Vec<String>>>,
}

const SETTINGS: &str = r#"{"name":"orders","replicas":2}"#;
const CREDS: &str = r#"{"token":"tok-0000-first"}"#;

impl Fixture {
    fn new() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let settings = Mount::new(
            root.path().join("etc/app/settings"),
            &[("settings.json", SETTINGS)],
        );
        let creds = Mount::new(root.path().join("etc/app/creds"), &[("creds.json", CREDS)]);
        Fixture {
            root,
            settings,
            creds,
            warnings: Arc::default(),
        }
    }

    fn loader<C: Docuconf + serde::de::DeserializeOwned>(&self) -> Loader<C> {
        let w = self.warnings.clone();
        Loader::<C>::new()
            .env([("DOCUCONF_FILE_ROOT", self.root.path().to_str().unwrap())])
            .termination_log(false)
            .now(now())
            .on_warning(move |m| w.lock().unwrap().push(m.to_string()))
    }

    fn load(&self) -> App {
        self.loader::<App>()
            .watch_interval(Duration::ZERO)
            .load()
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }
}

#[test]
fn exports_reload_watch() {
    let cue = docuconf::export::<App>(&Meta::new("app")).unwrap();
    for name in ["settings", "banner", "creds"] {
        let at = cue.find(&format!("\t\t{name}: {{")).unwrap();
        let block = &cue[at..at + cue[at..].find("\n\t\t}").unwrap()];
        assert!(block.contains("reload:"), "{block}");
        assert!(block.contains("\"watch\""), "{block}");
    }
}

#[test]
fn kubernetes_symlink_swap_is_picked_up() {
    let mut f = Fixture::new();
    let app = f.load();
    assert_eq!(app.settings.current().name, "orders");
    assert!(!app.settings.refresh(), "nothing changed");

    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":3}"#)]);
    let s = app.settings.current();
    assert_eq!((s.name.as_str(), s.replicas), ("billing", 3));
    // Clones share the content.
    let clone = app.settings.clone();
    assert_eq!(clone.current().name, "billing");

    f.settings
        .update(&[("settings.json", r#"{"name":"ledger","replicas":4}"#)]);
    assert_eq!(clone.current().name, "ledger");
    assert_eq!(app.settings.current().name, "ledger");
    assert!(f.warnings().is_empty());
}

#[test]
fn a_bad_change_keeps_the_previous_content() {
    let mut f = Fixture::new();
    let app = f.load();

    // Breaks the schema (replicas below 1): rejected, reported once.
    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":0}"#)]);
    assert_eq!(app.settings.current().name, "orders");
    assert_eq!(app.settings.current().name, "orders");
    let w = f.warnings();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].starts_with("settings: changed file rejected, keeping the previous content: "),
        "{w:?}"
    );
    assert!(w[0].ends_with("(schema_mismatch)"), "{w:?}");

    // Not JSON at all.
    f.settings.update(&[("settings.json", "{not json")]);
    assert_eq!(app.settings.current().replicas, 2);
    let w = f.warnings();
    assert!(w.len() == 1 && w[0].ends_with("(file_malformed)"), "{w:?}");

    // Fixed: picked up.
    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":5}"#)]);
    assert_eq!(app.settings.current().replicas, 5);
    assert!(f.warnings().is_empty());
}

#[test]
fn warnings_never_show_secret_content() {
    let mut f = Fixture::new();
    let app = f.load();
    assert_eq!(app.creds.current().token, "tok-0000-first");

    for bad in [
        r#"{"token":"tok-9999-leaked","#,
        r#"{"token":["tok-9999-leaked"]}"#,
        r#"["tok-9999-leaked"]"#,
    ] {
        f.creds.update(&[("creds.json", bad)]);
        assert_eq!(app.creds.current().token, "tok-0000-first");
        let w = f.warnings();
        assert!(!w.is_empty());
        for m in &w {
            assert!(m.starts_with("creds: changed file rejected"), "{m}");
            assert!(!m.contains("tok-"), "secret content in {m:?}");
        }
    }
    f.creds
        .update(&[("creds.json", r#"{"token":"tok-0001-second"}"#)]);
    assert_eq!(app.creds.current().token, "tok-0001-second");
}

#[test]
fn an_optional_file_can_appear_and_go() {
    let f = Fixture::new();
    let app = f.load();
    assert!(app.banner.current().is_none());

    let path = f.root.path().join("etc/app/banner/banner.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "Hello").unwrap();
    assert_eq!(
        app.banner.current().as_ref().as_ref().map(|t| t.text()),
        Some("Hello")
    );

    // Too long for max_length 20: the text checks run again.
    std::fs::write(&path, "x".repeat(30)).unwrap();
    assert_eq!(
        app.banner.current().as_ref().as_ref().map(|t| t.text()),
        Some("Hello")
    );
    let w = f.warnings();
    assert!(w.len() == 1 && w[0].contains("(out_of_range)"), "{w:?}");

    std::fs::remove_file(&path).unwrap();
    assert!(app.banner.current().is_none());
}

#[test]
fn a_required_file_that_disappears_keeps_its_content() {
    let f = Fixture::new();
    let app = f.load();
    std::fs::remove_file(f.root.path().join("etc/app/settings/..data")).unwrap();
    assert_eq!(app.settings.current().name, "orders");
    let w = f.warnings();
    assert!(w.len() == 1 && w[0].contains("(file_missing)"), "{w:?}");
}

#[test]
fn files_are_looked_at_once_per_interval() {
    let mut f = Fixture::new();
    // The default interval is a second.
    let app = f.loader::<App>().load().unwrap();
    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":3}"#)]);
    assert_eq!(app.settings.current().name, "orders", "not looked at yet");
    assert!(app.settings.refresh());
    assert_eq!(app.settings.current().name, "billing");

    let app = f
        .loader::<App>()
        .watch_interval(Duration::from_millis(50))
        .load()
        .unwrap();
    f.settings
        .update(&[("settings.json", r#"{"name":"ledger","replicas":3}"#)]);
    let start = std::time::Instant::now();
    while app.settings.current().name != "ledger" {
        assert!(start.elapsed() < Duration::from_secs(5), "never reloaded");
        std::thread::yield_now();
    }
}

#[test]
fn watched_tls_key_pair_rotates() {
    #[derive(Debug, Deserialize, Docuconf)]
    struct Server {
        /// Certificate the service serves HTTPS with.
        #[docuconf(path = "/etc/gateway/tls", dns_names("api.example.com"))]
        serving_tls: Watched<TlsKeyPair>,
    }
    let w = World::new();
    let warnings: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = warnings.clone();
    let server = w
        .loader::<Server>()
        .watch_interval(Duration::ZERO)
        .on_warning(move |m| sink.lock().unwrap().push(m.to_string()))
        .load()
        .unwrap();
    let first = server.serving_tls.current().cert_chain()[0].clone();

    // A renewed certificate.
    w.write_tls(&leaf(&["api.example.com"], Some(&w.ca)));
    let second = server.serving_tls.current().cert_chain()[0].clone();
    assert_ne!(first, second);

    // One that does not cover api.example.com is not used.
    w.write_tls(&leaf(&["other.example.com"], Some(&w.ca)));
    assert_eq!(server.serving_tls.current().cert_chain()[0], second);
    let got = warnings.lock().unwrap().clone();
    assert!(
        got.iter()
            .any(|m| m.contains("(certificate_name_mismatch)")),
        "{got:?}"
    );
    assert!(got.iter().all(|m| !m.contains("PRIVATE KEY")), "{got:?}");
}

#[test]
fn watched_new_never_reloads() {
    let t = Watched::new(Some(7));
    assert_eq!(*t.current(), Some(7));
    assert!(!t.refresh());
    assert_eq!(format!("{t:?}"), "Watched(Some(7))");
}
