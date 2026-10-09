//! orders: a tiny HTTP service whose configuration is declared with
//! docuconf. See README.md.
//!
//! ```sh
//! DATABASE_URL=postgres://orders:pw@localhost:5432/orders cargo run -p orders
//! cargo run -p orders -- export contract.cue
//! cargo run -p orders -- export --check contract.cue
//! ```

mod webhook;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use docuconf::{Docuconf, DocuconfEnum, Meta, Secret};
use serde::{Deserialize, Serialize};

/// The service's configuration. The first paragraph of each `///` comment
/// is the variable's description in the contract and the rest its details,
/// and the Rust type picks its contract type.
#[derive(Debug, Deserialize, Docuconf)]
struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    port: u16,

    /// Minimum log level emitted.
    #[docuconf(default = "info")]
    log_level: LogLevel,

    /// Postgres connection string for the orders database.
    // `Secret` marks it `secret: true`; `schemes` makes it a `url`, and
    // `max_length` caps it in characters.
    #[docuconf(schemes("postgres"), max_length = 2048)]
    database_url: Secret<String>,

    /// Browser origins allowed to call the API.
    #[docuconf(default = ["http://localhost:3000"], min_items = 1)]
    allowed_origins: Vec<String>,

    /// Time allowed to read and answer one request.
    #[docuconf(default = "30s", min = "1s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    request_timeout: Duration,

    /// Threads serving requests.
    ///
    /// Each worker answers one connection at a time, so this is also the
    /// number of requests served at once. Raise it when requests queue up;
    /// each worker holds a [`std::thread`] stack.
    ///
    /// Keep it at or below the database pool size:
    ///
    /// - one connection per worker;
    /// - plus one for migrations.
    #[docuconf(default = 4, min = 1, max = 64)]
    worker_count: u8,

    /// Keys that verify the signature on incoming payment webhooks.
    ///
    /// A webhook is accepted when it is signed with any key in the list, so
    /// the key can be rotated without turning webhooks away. To rotate:
    ///
    ///  1. add the new key as the second item, and roll out;
    ///  2. switch the sender to the new key;
    ///  3. remove the old key, and roll out.
    ///
    /// Each key is 32 to 256 characters, so an empty or truncated key fails
    /// at boot. Without this variable, the service rejects every webhook.
    // `Secret` marks the list `secret: true` and keeps every key out of
    // `Debug` and serde output; `encoding = "csv"` reads `old,new`.
    #[docuconf(
        encoding = "csv",
        min_items = 1,
        max_items = 2,
        item_min_length = 32,
        item_max_length = 256
    )]
    webhook_keys: Option<Secret<Vec<String>>>,
}

#[derive(Debug, Serialize, Deserialize, DocuconfEnum)]
#[serde(rename_all = "lowercase")]
enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

fn main() {
    // `orders export [--check] [PATH]` writes (or checks) the contract and
    // exits; any other invocation carries on.
    docuconf::export_command::<Config>(&Meta::new("orders-api").package("orders"));

    // Reads the environment and runs every check in one pass. On failure it
    // prints every problem at once, each with a stable code such as
    // missing_required, never a secret's value, and exits 1.
    let config: Config = docuconf::load_or_exit();

    let listener = TcpListener::bind(("0.0.0.0", config.port)).unwrap_or_else(|e| {
        eprintln!("orders: cannot listen on port {}: {e}", config.port);
        std::process::exit(1)
    });
    // docuconf has checked the URL and its scheme; only its host is printed.
    let db = docuconf::url::Url::parse(config.database_url.expose()).expect("a checked URL");
    println!(
        "orders listening on :{} with {} workers, database {}, log level {:?}",
        config.port,
        config.worker_count,
        db.host_str().unwrap_or_default(),
        config.log_level
    );
    let config = Arc::new(config);
    let workers: Vec<_> = (0..config.worker_count)
        .map(|_| {
            let listener = listener.try_clone().expect("clone the listener");
            let config = Arc::clone(&config);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    // A client that disconnects early is not an error.
                    let _ = serve(stream, &config);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
}

/// Answers one HTTP/1.1 request and closes the connection.
fn serve(mut stream: TcpStream, config: &Config) -> std::io::Result<()> {
    stream.set_read_timeout(Some(config.request_timeout))?;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // The two headers the webhook endpoint reads; the rest are skipped.
    let (mut content_length, mut signature) = (0usize, String::new());
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(usize::MAX);
            } else if name.eq_ignore_ascii_case("x-signature") {
                signature = value.to_string();
            }
        }
        header.clear();
    }

    let (status, content_type, body) =
        match request_line.split_whitespace().take(2).collect::<Vec<_>>()[..] {
            ["GET", "/healthz"] => ("200 OK", "text/plain", "ok".to_string()),
            ["GET", "/config"] => ("200 OK", "application/json", config_json(config)),
            // Payment webhooks, signed with any key in WEBHOOK_KEYS (see
            // `Config::webhook_keys` for how to rotate it).
            ["POST", "/webhooks/payments"] if content_length > 1 << 20 => (
                "413 Content Too Large",
                "text/plain",
                "body too large".into(),
            ),
            ["POST", "/webhooks/payments"] => {
                let mut payload = vec![0; content_length];
                reader.read_exact(&mut payload)?;
                let keys = config.webhook_keys.as_ref().map_or(&[][..], |k| k.expose());
                if webhook::verify(keys, &payload, &signature) {
                    ("204 No Content", "text/plain", String::new())
                } else {
                    ("401 Unauthorized", "text/plain", "bad signature".into())
                }
            }
            _ => ("404 Not Found", "text/plain", "not found".to_string()),
        };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The loaded configuration, with typed values. `Secret` serializes as
/// `"***"`, so the database URL and the webhook keys are redacted.
fn config_json(c: &Config) -> String {
    serde_json::json!({
        "PORT": c.port,
        "LOG_LEVEL": c.log_level,
        "DATABASE_URL": c.database_url,
        "ALLOWED_ORIGINS": c.allowed_origins,
        "REQUEST_TIMEOUT": docuconf::format_go(c.request_timeout),
        "WORKER_COUNT": c.worker_count,
        "WEBHOOK_KEYS": c.webhook_keys,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "oooooooooooooooooooooooooooooooo";
    const NEW: &str = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn";
    const BODY: &[u8] = br#"{"order":"42","status":"paid"}"#;

    fn sign(key: &str) -> String {
        webhook::sign(key, BODY)
    }

    /// Loads the configuration as the service does at boot.
    fn load(keys: &str) -> Result<Config, docuconf::Error> {
        docuconf::Loader::<Config>::new()
            .env([
                ("DATABASE_URL", "postgres://u:p@db/orders"),
                ("WEBHOOK_KEYS", keys),
            ])
            .termination_log(false)
            .load()
    }

    fn keys(value: &str) -> Vec<String> {
        load(value).unwrap().webhook_keys.unwrap().expose().clone()
    }

    /// Walks through a key rotation: each step is a rollout with a new
    /// WEBHOOK_KEYS, and a webhook signed with the key in use always
    /// verifies.
    #[test]
    fn rotation() {
        let both = format!("{OLD},{NEW}");
        for (step, value, accepts) in [
            ("before", OLD, [(OLD, true), (NEW, false)]),
            ("overlap", both.as_str(), [(OLD, true), (NEW, true)]),
            ("after", NEW, [(OLD, false), (NEW, true)]),
        ] {
            let ks = keys(value);
            for (key, want) in accepts {
                assert_eq!(
                    webhook::verify(&ks, BODY, &sign(key)),
                    want,
                    "{step}: key {}...",
                    &key[..1]
                );
            }
        }
        assert!(!webhook::verify(&keys(OLD), BODY, "not hex"));
        assert!(!webhook::verify(&keys(OLD), BODY, ""));
        assert!(!webhook::verify(&[], BODY, &sign(OLD)));
        assert!(load("").unwrap().webhook_keys.is_none());
    }

    /// The key set's constraints catch an empty or truncated key, and a
    /// third key, at boot, without printing any key.
    #[test]
    fn bad_key_sets() {
        for (value, code) in [
            (format!("{OLD},"), docuconf::Code::OutOfRange),
            (format!("{OLD},{}", &NEW[..10]), docuconf::Code::OutOfRange),
            (
                format!("{OLD},{NEW},{}", "x".repeat(32)),
                docuconf::Code::TooManyItems,
            ),
        ] {
            match load(&value) {
                Err(docuconf::Error::Validation(v)) => {
                    assert_eq!(v.codes_for("WEBHOOK_KEYS"), [code], "{value}");
                    let text = v.to_string();
                    assert!(
                        !text.contains(OLD) && !text.contains(&NEW[..10]),
                        "the error printed a key: {text}"
                    );
                }
                other => panic!("{value}: want {code:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn config_redacts_the_keys() {
        let json = config_json(&load(&format!("{OLD},{NEW}")).unwrap());
        assert!(json.contains(r#""WEBHOOK_KEYS":"***""#), "{json}");
        assert!(!json.contains(OLD) && !json.contains(NEW), "{json}");
    }
}
