//! Config-file overlays (SPEC §4.7): export, precedence, validation,
//! declaration checks, and an end-to-end render with the CUE meta-schema.
// These tests use the TLS and keystore file inputs (default features).
#![cfg(all(feature = "tls", feature = "keystore"))]

mod common;

use std::process::Command;
use std::time::Duration;

use common::{cue_module, cue_vet};
use docuconf::figment::providers::{Format, Toml};
use docuconf::figment::Figment;
use docuconf::url::Url;
use docuconf::{Code, Docuconf, Error, Meta, Overlay, OverlayFormat, Reload, Secret, TextFile};
use serde::Deserialize;

#[derive(Debug, Deserialize, Docuconf)]
#[docuconf(prefix = "CATALOG_")]
struct Catalog {
    /// Deployment profile: production or staging.
    #[docuconf(default = "production")]
    profile: String,
    /// Results per page.
    #[docuconf(default = 20, min = 1, max = 200)]
    page_size: u32,
    /// How long cached entries live.
    #[docuconf(default = "5m", max = "1h")]
    #[serde(with = "docuconf::humantime_serde")]
    cache_ttl: Duration,
    /// Categories shown on the home page.
    featured_categories: Option<Vec<String>>,
    /// Whether search suggestions are shown.
    #[docuconf(default = false)]
    suggestions: bool,
    /// The search service.
    search: Search,
    /// Database password.
    db_password: Secret<String>,
}

#[derive(Debug, Deserialize, Docuconf)]
struct Search {
    /// Base URL of the search service.
    #[docuconf(schemes("https"))]
    url: Url,
}

const OVERLAY: &str = "/etc/catalog/platform/app.toml";

const APP_TOML: &str = r#"
[default]
page_size = 10

[global]
featured_categories = ["baked"]

[production]
cache_ttl = "2m"
search.url = "https://search.baked"
"#;

struct World {
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let env = vec![
            (
                "DOCUCONF_FILE_ROOT".to_string(),
                dir.path().to_str().unwrap().to_string(),
            ),
            ("CATALOG_DB_PASSWORD".to_string(), "pw".to_string()),
        ];
        World { dir, env }
    }

    fn set(&mut self, k: &str, v: &str) -> &mut Self {
        self.env.push((k.into(), v.into()));
        self
    }

    fn write(&self, p: &str, content: &str) {
        let path = self.dir.path().join(p.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn loader(&self) -> docuconf::Loader<Catalog> {
        loader().env(self.env.clone())
    }
}

/// How the app builds its loader, for both export and boot.
fn loader() -> docuconf::Loader<Catalog> {
    docuconf::Loader::new()
        .figment(Figment::from(Toml::string(APP_TOML).nested()))
        .profiles("CATALOG_PROFILE", "production")
        .overlay(Overlay::new("platform", OVERLAY).description("Settings the platform supplies"))
        .termination_log(false)
}

fn violations(r: Result<Catalog, Error>) -> Vec<(String, Code)> {
    match r {
        Err(Error::Validation(v)) => {
            let mut got: Vec<_> = v
                .violations
                .into_iter()
                .map(|x| (x.input, x.code))
                .collect();
            got.sort();
            got
        }
        Err(e) => panic!("expected violations, got {e}"),
        Ok(c) => panic!("expected violations, loaded {c:?}"),
    }
}

#[test]
fn export_declares_the_overlay_and_config_keys() {
    let out = loader().export(&Meta::new("catalog")).unwrap();
    assert!(
        out.contains(
            "\toverlays: {\n\t\tplatform: {\n\t\t\tdescription:  \"Settings the platform supplies\"\n\t\t\tformat:       \"toml\"\n\t\t\tpath:         \"/etc/catalog/platform/app.toml\"\n\t\t\tkeySeparator: \".\"\n\t\t\treload:       \"restart\"\n\t\t}\n\t}\n"
        ),
        "{out}"
    );
    assert!(out.contains("configKey:   \"search.url\""), "{out}");
    assert!(out.contains("configKey:   \"page_size\""), "{out}");
    assert!(out.contains("configKey:   \"cache_ttl\""), "{out}");
    // Not for a secret, nor for the profile selector, which is read from
    // the environment before any file.
    assert_eq!(out.matches("configKey").count(), 5, "{out}");
    let password = &out[out.find("CATALOG_DB_PASSWORD").unwrap()..];
    let password = &password[..password.find('}').unwrap()];
    assert!(!password.contains("configKey"), "{password}");
    let profile = &out[out.find("CATALOG_PROFILE: {").unwrap()..];
    let profile = &profile[..profile.find('}').unwrap()];
    assert!(!profile.contains("configKey"), "{profile}");

    if let Some(res) = cue_vet(&out) {
        res.unwrap_or_else(|e| panic!("cue vet failed:\n{e}\n{out}"));
    }
    // Without overlays, no configKey is invented.
    let plain = docuconf::export::<Catalog>(&Meta::new("catalog")).unwrap();
    assert!(!plain.contains("configKey"), "{plain}");
    assert!(!plain.contains("overlays"), "{plain}");
}

#[test]
fn overlay_sits_between_the_apps_files_and_the_environment() {
    let mut w = World::new();
    w.write(
        OVERLAY,
        r#"
page_size = 50
cache_ttl = "90s"
featured_categories = ["books", "games"]

[search]
url = "https://search.overlay"
"#,
    );
    w.set("CATALOG_SEARCH__URL", "https://search.env");
    let c = w.loader().load().unwrap();
    assert_eq!(c.page_size, 50, "overlay over the base [default] table");
    assert_eq!(
        c.cache_ttl,
        Duration::from_secs(90),
        "overlay over the selected profile"
    );
    assert_eq!(
        c.featured_categories.as_deref(),
        Some(&["books".to_string(), "games".to_string()][..]),
        "overlay over the [global] table"
    );
    assert_eq!(
        c.search.url.as_str(),
        "https://search.env/",
        "environment over overlay"
    );
    assert!(!c.suggestions, "declaration default where nothing sets it");

    // Where the overlay is silent, the app's files still apply.
    let w2 = World::new();
    w2.write(OVERLAY, "suggestions = true\n");
    let c = w2.loader().load().unwrap();
    assert!(c.suggestions);
    assert_eq!(c.page_size, 10);
    assert_eq!(c.cache_ttl, Duration::from_secs(120));
    assert_eq!(c.search.url.as_str(), "https://search.baked/");
}

#[test]
fn a_missing_overlay_is_fine() {
    let c = World::new().loader().load().unwrap();
    assert_eq!(c.page_size, 10);
    assert_eq!(c.cache_ttl, Duration::from_secs(120));
    assert_eq!(c.profile, "production");
}

#[test]
fn overlay_values_are_validated_like_any_other() {
    let w = World::new();
    w.write(
        OVERLAY,
        "page_size = 1000\ncache_ttl = \"forever\"\nsuggestions = \"yes\"\n[search]\nurl = \"http://insecure.example.com\"\n",
    );
    assert_eq!(
        violations(w.loader().load()),
        [
            ("CATALOG_CACHE_TTL".to_string(), Code::InvalidType),
            ("CATALOG_PAGE_SIZE".to_string(), Code::OutOfRange),
            ("CATALOG_SEARCH__URL".to_string(), Code::InvalidScheme),
            ("CATALOG_SUGGESTIONS".to_string(), Code::InvalidType),
        ]
    );
}

#[test]
fn a_malformed_overlay_is_reported_with_the_rest() {
    let mut w = World::new();
    w.write(OVERLAY, "page_size = [\n");
    w.set("CATALOG_PAGE_SIZE", "0");
    assert_eq!(
        violations(w.loader().load()),
        [
            ("CATALOG_PAGE_SIZE".to_string(), Code::OutOfRange),
            ("platform".to_string(), Code::FileMalformed),
        ]
    );
}

#[test]
fn json_and_yaml_overlays() {
    for (path, content) in [
        (
            "/etc/catalog/platform/app.json",
            r#"{"page_size": 60, "search": {"url": "https://search.json"}}"#,
        ),
        (
            "/etc/catalog/platform/app.yaml",
            "page_size: 70\nsearch:\n  url: https://search.yaml\n",
        ),
    ] {
        let w = World::new();
        w.write(path, content);
        let c = docuconf::Loader::<Catalog>::new()
            .overlay(Overlay::new("platform", path))
            .env(w.env.clone())
            .termination_log(false)
            .load()
            .unwrap();
        assert!(c.page_size == 60 || c.page_size == 70, "{path}");
        assert!(c.search.url.as_str().starts_with("https://search."));
    }
}

fn declaration_problems(l: docuconf::Loader<Catalog>) -> String {
    let export = l.export(&Meta::new("catalog")).unwrap_err().to_string();
    match l.env([("CATALOG_DB_PASSWORD", "pw")]).load() {
        Err(Error::Declaration(e)) => assert_eq!(e.to_string(), export),
        other => panic!("load must refuse the declaration too: {other:?}"),
    }
    export
}

#[test]
fn bad_overlay_declarations_are_refused() {
    let base = || docuconf::Loader::<Catalog>::new().termination_log(false);
    let cases = [
        (
            Overlay::new("platform", OVERLAY).reload(Reload::Watch),
            "overlay platform: reload watch is not implemented",
        ),
        (
            Overlay::new("Platform", OVERLAY),
            "overlay Platform: name must be a DNS label",
        ),
        (
            Overlay::new("platform", "etc/app.toml"),
            "must be absolute and normalised",
        ),
        (
            Overlay::new("platform", "/etc/catalog/platform/app.conf"),
            "cannot tell the format",
        ),
        (
            Overlay::new("platform", "/etc/app.toml"),
            "would be mounted at /etc",
        ),
        (
            Overlay::new("platform", OVERLAY).description("abc"),
            "shorter than 5 characters",
        ),
    ];
    for (o, want) in cases {
        let got = declaration_problems(base().overlay(o));
        assert!(got.contains(want), "want {want:?} in {got}");
    }
    // An explicit format makes any extension fine.
    base()
        .overlay(
            Overlay::new("platform", "/etc/catalog/platform/app.conf").format(OverlayFormat::Toml),
        )
        .export(&Meta::new("catalog"))
        .unwrap();

    let got = declaration_problems(
        base()
            .overlay(Overlay::new("platform", OVERLAY))
            .overlay(Overlay::new("platform", "/etc/catalog/other/app.toml")),
    );
    assert!(got.contains("overlay platform: declared twice"), "{got}");
}

#[test]
fn an_overlay_must_not_hide_the_apps_own_files() {
    // A directory holding one of the app's config files.
    let dir = tempfile::tempdir().unwrap();
    let app_toml = dir.path().join("App.toml");
    std::fs::write(&app_toml, "[default]\npage_size = 10\n").unwrap();
    let got = docuconf::Loader::<Catalog>::new()
        .figment(Figment::from(Toml::file(&app_toml).nested()))
        .overlay(Overlay::new(
            "platform",
            dir.path().join("overlay.toml").to_str().unwrap(),
        ))
        .export(&Meta::new("catalog"))
        .unwrap_err()
        .to_string();
    assert!(got.contains("holds files the app ships with"), "{got}");

    // The directory of the executable.
    let exe_dir = std::env::current_exe().unwrap();
    let exe_dir = exe_dir.parent().unwrap().to_str().unwrap();
    let err = docuconf::Loader::<Catalog>::new()
        .overlay(Overlay::new("platform", format!("{exe_dir}/overlay.toml")))
        .env([("CATALOG_DB_PASSWORD", "pw")])
        .termination_log(false)
        .load()
        .unwrap_err();
    assert!(
        matches!(&err, Error::Declaration(e) if e.to_string().contains("holds files the app ships with")),
        "{err}"
    );
}

#[test]
fn an_overlay_shares_no_mount_directory_with_a_file_input() {
    #[derive(Debug, Deserialize, Docuconf)]
    #[allow(dead_code)]
    struct WithFile {
        /// Licence key file.
        #[docuconf(path = "/etc/catalog/platform/license.key")]
        license: TextFile,
    }
    let err = docuconf::Loader::<WithFile>::new()
        .overlay(Overlay::new("platform", OVERLAY))
        .export(&Meta::new("catalog"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "overlay platform: shares mount directory /etc/catalog/platform with file input license"
        ),
        "{err}"
    );
}

#[test]
fn a_config_key_must_be_the_figment_key_when_overlays_are_declared() {
    #[derive(Debug, Deserialize, Docuconf)]
    #[allow(dead_code)]
    struct Renamed {
        /// Results per page.
        #[docuconf(default = 20, config_key = "Catalog:PageSize")]
        page_size: u32,
    }
    let out = docuconf::export::<Renamed>(&Meta::new("catalog")).unwrap();
    assert!(out.contains("configKey:   \"Catalog:PageSize\""), "{out}");
    let err = docuconf::Loader::<Renamed>::new()
        .overlay(Overlay::new("platform", OVERLAY))
        .export(&Meta::new("catalog"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("config_key \"Catalog:PageSize\" must be the figment key \"page_size\""),
        "{err}"
    );
}

// End to end: the platform renders the overlay from the exported contract
// with the meta-schema's #Render, and the app loads the rendered file.
#[test]
fn an_overlay_rendered_by_the_platform_binds_in_the_app() {
    for (format, ext) in [
        (OverlayFormat::Toml, "toml"),
        (OverlayFormat::Json, "json"),
        (OverlayFormat::Yaml, "yaml"),
    ] {
        let path = format!("/etc/catalog/platform/app.{ext}");
        let loader = || {
            docuconf::Loader::<Catalog>::new()
                .figment(Figment::from(Toml::string(APP_TOML).nested()))
                .profiles("CATALOG_PROFILE", "production")
                .overlay(Overlay::new("platform", &path).format(format))
                .termination_log(false)
        };
        let contract = loader().export(&Meta::new("catalog")).unwrap();
        let Some((cue, module)) = cue_module(&contract) else {
            return;
        };
        let render = format!(
            r#"package platform

import (
	"docuconf.dev/contract"
	app "docuconf.dev/svc:catalog"
)

rendered: contract.#Render & {{
	contract: app
	overlays: platform: {{
		CATALOG_PAGE_SIZE:   50
		CATALOG_CACHE_TTL:   "1m30s"
		CATALOG_SUGGESTIONS: true
		CATALOG_SEARCH__URL: "https://search.internal"
		CATALOG_FEATURED_CATEGORIES: ["books", "games"]
	}}
}}
file: rendered.configMaps[0].data["app.{ext}"]
"#
        );
        let pdir = module.path().join("platform");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("render.cue"), render).unwrap();
        let out = Command::new(&cue)
            .args(["export", "./platform", "-e", "file", "--out", "text"])
            .current_dir(module.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "cue export failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rendered = String::from_utf8(out.stdout).unwrap();

        let w = World::new();
        w.write(&path, &rendered);
        let c = loader()
            .env(w.env.clone())
            .load()
            .unwrap_or_else(|e| panic!("{ext}: {e}\n{rendered}"));
        assert_eq!(c.page_size, 50, "{ext}");
        assert_eq!(c.cache_ttl, Duration::from_secs(90), "{ext}");
        assert!(c.suggestions, "{ext}");
        assert_eq!(c.search.url.as_str(), "https://search.internal/", "{ext}");
        assert_eq!(
            c.featured_categories,
            Some(vec!["books".to_string(), "games".to_string()]),
            "{ext}"
        );
        assert_eq!(c.db_password.expose(), "pw");
    }
}
