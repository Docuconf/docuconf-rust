//! shop: one image that runs as two roles, `web` and `worker`, with its
//! configuration loaded contract-first by docuconf. See README.md.
//!
//! ```sh
//! DOCUCONF_ROLE=web DATABASE_URL=postgres://shop@db:5432/shop \
//!   ALLOWED_ORIGINS=https://shop.example cargo run -p shop
//! DOCUCONF_ROLE=worker DATABASE_URL=postgres://shop@db:5432/shop \
//!   QUEUES=default,mail cargo run -p shop
//! ```

use docuconf::contract::Values;
use docuconf::Contract;

/// The contract, as JSON (`contract.cue` is the same contract in CUE). It
/// is compiled in, so the binary needs no file next to it.
const CONTRACT: &str = include_str!("../contract.json");

fn main() {
    // A contract that docuconf cannot read is a bug in the image, not in
    // the deployment: say so and exit 1.
    let contract = Contract::from_json(CONTRACT).unwrap_or_else(|e| {
        eprintln!("shop: bad contract.json: {e}");
        std::process::exit(1)
    });

    // Reads the environment and runs every check in one pass, roles and
    // requiredIf included. On failure it prints every problem at once, each
    // with a stable code such as missing_required, never a secret's value,
    // and exits 1.
    let values = contract.load().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });

    println!("{}", summary(&values));
}

/// The line the app prints once its configuration is valid, from the typed
/// values (never the raw environment).
fn summary(values: &Values) -> String {
    let list = |name: &str| {
        values
            .get(name)
            .and_then(|v| v.as_list())
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    // A valid configuration always has a role: DOCUCONF_ROLE is a required
    // enum whose values are exactly the contract's roles.
    match values.get("DOCUCONF_ROLE").and_then(|v| v.as_str()) {
        Some("web") => {
            let port = values
                .get("PORT")
                .and_then(|v| v.as_int())
                .unwrap_or_default();
            format!("shop web: port {port}, origins {}", list("ALLOWED_ORIGINS"))
        }
        Some("worker") => {
            let webhooks = values.get("WEBHOOKS_ENABLED").and_then(|v| v.as_bool());
            let webhooks = if webhooks == Some(true) { "on" } else { "off" };
            format!(
                "shop worker: queues {}, webhooks {webhooks}",
                list("QUEUES")
            )
        }
        role => unreachable!("docuconf accepted the role {role:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docuconf::Code;

    const BASE: (&str, &str) = ("DATABASE_URL", "postgres://shop@db:5432/shop");
    const KEY_PATH: &str = "etc/shop/webhooks/signing.key";

    /// Loads the contract with the base env plus `env`, and a fresh, empty
    /// file root, through `files` to put files in it first.
    fn load_with(
        env: &[(&str, &str)],
        files: impl FnOnce(&std::path::Path),
    ) -> Result<Values, docuconf::ValidationError> {
        let root = tempfile::tempdir().unwrap();
        files(root.path());
        let root_path = root.path().to_str().unwrap().to_string();
        let mut vars = vec![BASE, ("DOCUCONF_FILE_ROOT", root_path.as_str())];
        vars.extend_from_slice(env);
        Contract::from_json(CONTRACT).unwrap().load_env(vars)
    }

    fn load(env: &[(&str, &str)]) -> Result<Values, docuconf::ValidationError> {
        load_with(env, |_| {})
    }

    /// Asserts the configuration is valid and returns the printed line.
    #[track_caller]
    fn ok(r: Result<Values, docuconf::ValidationError>) -> String {
        summary(&r.unwrap_or_else(|e| panic!("want ok, got {e}")))
    }

    /// Asserts the only violation is `code` on `input`, and returns its
    /// message.
    #[track_caller]
    fn only(r: Result<Values, docuconf::ValidationError>, input: &str, code: Code) -> String {
        let e = r.expect_err("want a violation, got ok");
        assert_eq!(e.violations.len(), 1, "want only {input} {code:?}, got {e}");
        let v = &e.violations[0];
        assert_eq!((v.input.as_str(), v.code), (input, code), "{e}");
        assert!(!e.to_string().contains("shop@db"), "a secret leaked: {e}");
        v.message.clone()
    }

    #[test]
    fn contract_reads() {
        let c = Contract::from_json(CONTRACT).unwrap();
        assert_eq!(c.name(), "shop");
        assert_eq!(
            c.roles(),
            Some(&["web".to_string(), "worker".to_string()][..])
        );
        assert_eq!(c.includes(), ["otel@1"]);
    }

    #[test]
    fn case_01_web() {
        let out = ok(load(&[
            ("DOCUCONF_ROLE", "web"),
            ("ALLOWED_ORIGINS", "https://shop.example"),
        ]));
        assert_eq!(out, "shop web: port 8080, origins https://shop.example");
    }

    #[test]
    fn case_02_worker_needs_no_allowed_origins() {
        let out = ok(load(&[
            ("DOCUCONF_ROLE", "worker"),
            ("QUEUES", "default,mail"),
        ]));
        assert_eq!(out, "shop worker: queues default,mail, webhooks off");
    }

    #[test]
    fn case_03_web_needs_allowed_origins() {
        only(
            load(&[("DOCUCONF_ROLE", "web")]),
            "ALLOWED_ORIGINS",
            Code::MissingRequired,
        );
    }

    #[test]
    fn case_04_no_role() {
        only(
            load(&[("QUEUES", "default")]),
            "DOCUCONF_ROLE",
            Code::MissingRequired,
        );
    }

    #[test]
    fn case_05_unknown_role() {
        only(
            load(&[("DOCUCONF_ROLE", "cron")]),
            "DOCUCONF_ROLE",
            Code::NotInEnum,
        );
    }

    const JSON_LOGS: [(&str, &str); 3] = [
        ("DOCUCONF_ROLE", "worker"),
        ("QUEUES", "default"),
        ("LOG_FORMAT", "json"),
    ];

    #[test]
    fn case_06_json_logs_need_a_schema_url() {
        let msg = only(load(&JSON_LOGS), "LOG_SCHEMA_URL", Code::MissingRequired);
        assert!(
            msg.contains(r#"is required when LOG_FORMAT is "json""#),
            "{msg}"
        );
    }

    #[test]
    fn case_07_json_logs_with_a_schema_url() {
        let mut env = JSON_LOGS.to_vec();
        env.push(("LOG_SCHEMA_URL", "https://logs.example/v1.json"));
        assert_eq!(ok(load(&env)), "shop worker: queues default, webhooks off");
    }

    const WEBHOOKS: [(&str, &str); 3] = [
        ("DOCUCONF_ROLE", "worker"),
        ("QUEUES", "default"),
        ("WEBHOOKS_ENABLED", "true"),
    ];

    #[test]
    fn case_08_webhooks_need_the_key_file() {
        let msg = only(load(&WEBHOOKS), "webhook-signing-key", Code::FileMissing);
        assert!(
            msg.contains(", and is required when WEBHOOKS_ENABLED is true"),
            "{msg}"
        );
    }

    #[test]
    fn case_09_webhooks_with_the_key_file() {
        let key = "k".repeat(40);
        let r = load_with(&WEBHOOKS, |root| {
            let path = root.join(KEY_PATH);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &key).unwrap();
        });
        assert_eq!(ok(r), "shop worker: queues default, webhooks on");
    }

    #[test]
    fn case_10_key_file_belongs_to_the_worker() {
        let out = ok(load(&[
            ("DOCUCONF_ROLE", "web"),
            ("ALLOWED_ORIGINS", "https://shop.example"),
            ("WEBHOOKS_ENABLED", "true"),
        ]));
        assert_eq!(out, "shop web: port 8080, origins https://shop.example");
    }

    #[test]
    fn case_11_fragment_vars_are_checked() {
        only(
            load(&[
                ("DOCUCONF_ROLE", "web"),
                ("ALLOWED_ORIGINS", "https://shop.example"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "carrier-pigeon"),
            ]),
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            Code::NotInEnum,
        );
    }
}
