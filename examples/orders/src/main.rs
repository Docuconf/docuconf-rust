//! orders: a tiny HTTP service whose configuration is declared with
//! docuconf. See README.md.
//!
//! ```sh
//! DATABASE_URL=postgres://orders:pw@localhost:5432/orders cargo run -p orders
//! cargo run -p orders -- export contract.cue
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use docuconf::{Docuconf, DocuconfEnum, Meta, Secret};
use serde::{Deserialize, Serialize};

/// The service's configuration. Each `///` comment is the variable's
/// description in the contract, and the Rust type picks its contract type.
#[derive(Debug, Deserialize, Docuconf)]
struct Config {
    /// HTTP listen port.
    #[docuconf(default = 8080, min = 1)]
    port: u16,

    /// Minimum log level emitted.
    #[docuconf(default = "info")]
    log_level: LogLevel,

    /// Postgres connection string for the orders database.
    // `Secret` marks it `secret: true`; `schemes` makes it a `url`.
    #[docuconf(schemes("postgres"))]
    database_url: Secret<String>,

    /// Browser origins allowed to call the API.
    #[docuconf(default = ["http://localhost:3000"], min_items = 1)]
    allowed_origins: Vec<String>,

    /// Time allowed to read and answer one request.
    #[docuconf(default = "30s", min = "1s", max = "5m")]
    #[serde(with = "docuconf::humantime_serde")]
    request_timeout: Duration,

    /// Threads serving requests.
    #[docuconf(default = 4, min = 1, max = 64)]
    worker_count: u8,
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("export") {
        let out = args.get(1).map_or("contract.cue", String::as_str);
        let meta = Meta {
            package: Some("orders".into()),
            ..Meta::new("orders-api")
        };
        match docuconf::export::<Config>(&meta) {
            Ok(cue) => std::fs::write(out, cue).expect("write the contract"),
            Err(e) => exit(e),
        }
        return;
    }

    // Reads the environment and runs every check in one pass. On failure it
    // reports every violation at once, each with a stable code such as
    // missing_required, and never prints a secret's value.
    let config: Config = docuconf::load().unwrap_or_else(|e| exit(e));

    let listener = TcpListener::bind(("0.0.0.0", config.port)).unwrap_or_else(|e| exit(e));
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

fn exit(e: impl std::fmt::Display) -> ! {
    eprintln!("{e}");
    std::process::exit(1)
}

/// Answers one HTTP/1.1 request and closes the connection.
fn serve(mut stream: TcpStream, config: &Config) -> std::io::Result<()> {
    stream.set_read_timeout(Some(config.request_timeout))?;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // Skip the headers: neither endpoint reads them.
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }

    let (status, content_type, body) =
        match request_line.split_whitespace().take(2).collect::<Vec<_>>()[..] {
            ["GET", "/healthz"] => ("200 OK", "text/plain", "ok".to_string()),
            ["GET", "/config"] => ("200 OK", "application/json", config_json(config)),
            _ => ("404 Not Found", "text/plain", "not found".to_string()),
        };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The loaded configuration, with typed values and the secret redacted.
fn config_json(c: &Config) -> String {
    serde_json::json!({
        "PORT": c.port,
        "LOG_LEVEL": c.log_level,
        "DATABASE_URL": "***",
        "ALLOWED_ORIGINS": c.allowed_origins,
        "REQUEST_TIMEOUT": docuconf::format_go(c.request_timeout),
        "WORKER_COUNT": c.worker_count,
    })
    .to_string()
}
