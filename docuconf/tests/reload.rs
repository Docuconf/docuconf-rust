//! `reload: watch` (SPEC §11.2 item 8): `Watched<T>` file inputs reread
//! their files when they change, including the symlink swap Kubernetes
//! makes when it updates a projected volume.
#![cfg(all(unix, feature = "tls", feature = "keystore"))]

mod common;

use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{keystore_bytes, leaf, now, World};
use docuconf::{
    Code, ConfigFile, Contract, Docuconf, Keystore, Loader, Meta, Secret, TextFile, TlsKeyPair,
    Watched,
};
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

#[test]
fn hooks_run_on_an_accepted_change_only() {
    let mut f = Fixture::new();
    let app = f.load();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = seen.clone();
    let _sub = app
        .settings
        .on_change(move |s| sink.lock().unwrap().push(s.name.clone()));
    let st = app.settings.status();
    assert_eq!(st.generation, 1);
    assert_eq!(st.last_reload, None);
    assert_eq!(st.last_rejected, None);

    // Rejected: no hook, the rejection is in the status.
    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":0}"#)]);
    assert!(!app.settings.refresh());
    assert!(seen.lock().unwrap().is_empty());
    let st = app.settings.status();
    assert_eq!(st.generation, 1);
    let rej = st.last_rejected.expect("rejected");
    assert_eq!(rej.input, "settings");
    assert_eq!(rej.codes, [Code::SchemaMismatch]);
    assert!(f.warnings().len() == 1);

    // Accepted: the hook gets the new value, the rejection is cleared.
    let before = std::time::SystemTime::now();
    f.settings
        .update(&[("settings.json", r#"{"name":"ledger","replicas":3}"#)]);
    assert!(app.settings.refresh());
    assert_eq!(*seen.lock().unwrap(), ["ledger"]);
    let st = app.settings.status();
    assert_eq!(st.generation, 2);
    assert!(st.last_reload.unwrap() >= before);
    assert_eq!(st.last_rejected, None);
    assert_eq!(app.settings.generation(), 2);
    // Clones share hooks and status.
    assert_eq!(app.settings.clone().status(), st);
}

#[test]
fn a_panicking_hook_does_not_stop_the_reload() {
    let mut f = Fixture::new();
    let app = f.load();
    let calls: Arc<Mutex<Vec<&str>>> = Arc::default();
    let (a, b) = (calls.clone(), calls.clone());
    let watched = app.creds.clone();
    app.creds.on_change(move |_| {
        a.lock().unwrap().push("first");
        panic!("hook failed");
    });
    app.creds.on_change(move |c| {
        // Inside a hook, current() is the new content.
        assert_eq!(watched.current().token, c.token);
        b.lock().unwrap().push("second");
    });
    f.creds
        .update(&[("creds.json", r#"{"token":"tok-0001-second"}"#)]);
    assert!(app.creds.refresh());
    assert_eq!(*calls.lock().unwrap(), ["first", "second"]);
    assert_eq!(app.creds.current().token, "tok-0001-second");
    assert_eq!(app.creds.generation(), 2);
    let w = f.warnings();
    assert_eq!(
        w,
        ["creds: an on_change hook panicked; the new content is current"]
    );
}

#[test]
fn unsubscribed_hooks_are_not_called() {
    let mut f = Fixture::new();
    let app = f.load();
    let n = Arc::new(Mutex::new(0));
    let sink = n.clone();
    let sub = app.settings.on_change(move |_| *sink.lock().unwrap() += 1);
    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":3}"#)]);
    assert!(app.settings.refresh());
    sub.unsubscribe();
    f.settings
        .update(&[("settings.json", r#"{"name":"ledger","replicas":3}"#)]);
    assert!(app.settings.refresh());
    assert_eq!(*n.lock().unwrap(), 1);
}

#[test]
fn hooks_run_without_a_read() {
    let mut f = Fixture::new();
    let app = f
        .loader::<App>()
        .watch_interval(Duration::from_millis(20))
        .load()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    app.settings.on_change(move |s| {
        let _ = tx.lock().unwrap().send(s.name.clone());
    });
    f.settings
        .update(&[("settings.json", r#"{"name":"billing","replicas":3}"#)]);
    // Nothing reads the input: the background check notices the change.
    let got = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("no hook call");
    assert_eq!(got, "billing");
}

/// Replaces a file with a new one (a new inode), as an agent would.
fn replace(path: &Path, content: &[u8]) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

#[test]
fn a_keystore_reload_uses_the_boot_password() {
    #[derive(Debug, Deserialize, Docuconf)]
    #[allow(dead_code)]
    struct Client {
        /// Password of the partner keystore.
        partner_keystore_password: Secret<String>,

        /// Client certificate for mTLS to the partner API.
        #[docuconf(
            path = "/etc/gateway/partner/keystore.p12",
            password_var = "PARTNER_KEYSTORE_PASSWORD"
        )]
        partner_keystore: Watched<Keystore>,
    }
    let w = World::new();
    let warnings: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = warnings.clone();
    let client = w
        .loader::<Client>()
        .watch_interval(Duration::ZERO)
        .on_warning(move |m| sink.lock().unwrap().push(m.to_string()))
        .load()
        .unwrap();
    let first = client.partner_keystore.current().cert_chain()[0].clone();
    let path = w.path("/etc/gateway/partner/keystore.p12");

    // A new keystore written with another password: the password read at
    // boot does not open it, so it is rejected and the old one stays.
    let renewed = leaf(&["partner-client"], Some(&w.ca));
    replace(&path, &keystore_bytes(&renewed, "rotated-pass"));
    assert!(!client.partner_keystore.refresh());
    assert_eq!(client.partner_keystore.current().cert_chain()[0], first);
    let st = client.partner_keystore.status();
    assert_eq!(st.generation, 1);
    assert_eq!(
        st.last_rejected.map(|r| r.codes),
        Some(vec![Code::KeystoreUnreadable])
    );
    let got = warnings.lock().unwrap().clone();
    assert!(
        got.len() == 1 && got[0].ends_with("(keystore_unreadable)"),
        "{got:?}"
    );
    assert!(got
        .iter()
        .all(|m| !m.contains("rotated-pass") && !m.contains("s3cret-pass")));

    // The same certificate renewed under the boot password is picked up.
    replace(&path, &keystore_bytes(&renewed, "s3cret-pass"));
    assert!(client.partner_keystore.refresh());
    assert_ne!(client.partner_keystore.current().cert_chain()[0], first);
    let st = client.partner_keystore.status();
    assert_eq!((st.generation, st.last_rejected), (2, None));
}

const WATCH_CONTRACT: &str = r#"{
    "apiVersion": "docuconf.dev/v1alpha1",
    "kind": "ConfigContract",
    "metadata": {"name": "app"},
    "vars": {
        "PARTNER_KEYSTORE_PASSWORD": {"type": "string", "description": "Keystore password", "secret": true}
    },
    "files": {
        "settings": {"type": "config", "format": "json", "description": "Application settings",
                     "path": "/etc/app/settings/settings.json", "reload": "watch",
                     "schema": {"type": "object", "properties": {"replicas": {"type": "integer", "minimum": 1}}}},
        "banner": {"type": "text", "description": "Banner", "path": "/etc/app/banner/banner.txt", "reload": "watch"},
        "keystore": {"type": "keystore", "description": "Client certificate",
                     "path": "/etc/gateway/partner/keystore.p12", "passwordVar": "PARTNER_KEYSTORE_PASSWORD",
                     "reload": "watch"},
        "license": {"type": "text", "description": "Licence key", "path": "/etc/gateway/license/license.key"}
    }
}"#;

#[test]
fn contract_first_reloads_watched_inputs() {
    let w = World::new();
    let mut settings = Mount::new(
        w.path("/etc/app/settings"),
        &[("settings.json", r#"{"replicas":2}"#)],
    );
    let contract = Contract::from_json(WATCH_CONTRACT)
        .unwrap()
        .watch_interval(Duration::ZERO);
    let values = contract
        .load_env([
            ("DOCUCONF_FILE_ROOT", w.path("/").to_str().unwrap()),
            ("PARTNER_KEYSTORE_PASSWORD", "s3cret-pass"),
        ])
        .unwrap();
    // Only inputs declared watch have a handle.
    assert!(values.watched("license").is_none());
    let s = values.watched("settings").unwrap();
    let replicas = |s: &Watched<Option<docuconf::contract::FileValue>>| match s.current().as_ref() {
        Some(docuconf::contract::FileValue::Config { data, .. }) => data["replicas"].as_i64(),
        _ => None,
    };
    assert_eq!(replicas(&s), Some(2));
    let seen: Arc<Mutex<Vec<Option<i64>>>> = Arc::default();
    let sink = seen.clone();
    s.on_change(move |v| {
        if let Some(docuconf::contract::FileValue::Config { data, .. }) = v.as_ref() {
            sink.lock().unwrap().push(data["replicas"].as_i64());
        }
    });

    settings.update(&[("settings.json", r#"{"replicas":0}"#)]);
    assert!(!s.refresh());
    assert_eq!(replicas(&s), Some(2));
    assert_eq!(
        s.status().last_rejected.map(|r| (r.input, r.codes)),
        Some(("settings".to_string(), vec![Code::SchemaMismatch]))
    );
    settings.update(&[("settings.json", r#"{"replicas":5}"#)]);
    assert!(s.refresh());
    assert_eq!(replicas(&s), Some(5));
    assert_eq!(*seen.lock().unwrap(), [Some(5)]);
    assert_eq!((s.generation(), s.status().last_rejected), (2, None));
    // file() is the content as loaded.
    assert_eq!(
        values
            .file("settings")
            .map(|f| f.to_json()["replicas"].clone()),
        Some(serde_json::json!(2))
    );

    // An optional watched input that was absent appears.
    let b = values.watched("banner").unwrap();
    assert!(b.current().is_none());
    w.write("/etc/app/banner/banner.txt", b"Hello");
    assert!(b.refresh());
    assert!(
        matches!(b.current().as_ref(), Some(docuconf::contract::FileValue::Text(t)) if t.text() == "Hello")
    );

    // The keystore is reopened with the password read at load.
    let k = values.watched("keystore").unwrap();
    let path = w.path("/etc/gateway/partner/keystore.p12");
    let renewed = leaf(&["partner-client"], Some(&w.ca));
    replace(&path, &keystore_bytes(&renewed, "rotated-pass"));
    assert!(!k.refresh());
    assert_eq!(
        k.status().last_rejected.map(|r| r.codes),
        Some(vec![Code::KeystoreUnreadable])
    );
    replace(&path, &keystore_bytes(&renewed, "s3cret-pass"));
    assert!(k.refresh());
    assert_eq!(k.generation(), 2);
}

#[test]
fn contract_first_rejects_a_watched_overlay() {
    let e = Contract::from_json(
        r#"{"apiVersion": "docuconf.dev/v1alpha1", "kind": "ConfigContract",
            "metadata": {"name": "app"},
            "overlays": {"platform": {"format": "json", "path": "/etc/app/platform.json", "reload": "watch"}}}"#,
    )
    .unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("overlay platform") && text.contains("reload \"watch\""),
        "{text}"
    );
}
