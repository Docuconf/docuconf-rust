# Example: shop

One image that runs as two processes, a `web` server and a queue `worker`,
with its configuration loaded **contract-first**: the contract is written by
hand, and the app loads it with `docuconf::Contract` instead of declaring a
struct. It shows three parts of the spec that the derive has no attributes
for yet:

- **roles** ([SPEC §4.9](https://github.com/docuconf/docuconf-go/blob/main/spec/SPEC.md)):
  `DOCUCONF_ROLE` picks the workload's role, and an input marked with roles
  is used only in those. The web server's `ALLOWED_ORIGINS` is required, but
  only for `web`; the worker's `QUEUES` only for `worker`.
- **requiredIf** (SPEC §4.2.1): `LOG_SCHEMA_URL` is required when
  `LOG_FORMAT` is `json`, and the `webhook-signing-key` file when
  `WEBHOOKS_ENABLED` is `true`.
- an **included fragment** (SPEC §4.8): `includes: ["otel@1"]`, the
  OpenTelemetry variables, inline in the contract and checked at boot like
  the app's own.

There is no real server, database or queue: the app loads its
configuration, prints one line and exits 0, or lists every problem and
exits 1.

| Input | Type | Rules |
|---|---|---|
| `DOCUCONF_ROLE` | enum | `web` or `worker`; required |
| `DATABASE_URL` | url | secret, required, scheme `postgres` or `postgresql` |
| `PORT` | int | 1–65535, default `8080`; `web` only |
| `ALLOWED_ORIGINS` | list of strings, comma-separated | required; `web` only |
| `QUEUES` | list of strings, comma-separated | required; `worker` only |
| `WEBHOOKS_ENABLED` | bool | default `false`; `worker` only |
| `LOG_FORMAT` | enum | `text` or `json`; default `text` |
| `LOG_SCHEMA_URL` | url | scheme `https`; required when `LOG_FORMAT` is `json` |
| `OTEL_*` | from the `otel@1` fragment | all optional |
| file `webhook-signing-key` | text, at `/etc/shop/webhooks/signing.key` | secret, at least 32 characters; required when `WEBHOOKS_ENABLED` is `true`; `worker` only |

The contract is [`contract.json`](contract.json), which the binary embeds
and loads at startup ([`src/main.rs`](src/main.rs)), and
[`contract.cue`](contract.cue), the same contract in CUE for the platform
and the docs. Both are written by hand; CI vets both against the
meta-schema and checks that they export the same contract. The example is a
member of this repository's Cargo workspace and depends on the SDK by path.

## Run it

```console
$ cd examples/shop
$ DOCUCONF_ROLE=web DATABASE_URL=postgres://shop@db:5432/shop \
    ALLOWED_ORIGINS=https://shop.example cargo run -q -p shop
shop web: port 8080, origins https://shop.example
$ DOCUCONF_ROLE=worker DATABASE_URL=postgres://shop@db:5432/shop \
    QUEUES=default,mail cargo run -q -p shop
shop worker: queues default,mail, webhooks off
```

The worker needs no `ALLOWED_ORIGINS`, and the web server no `QUEUES`. The
printed lists come from the typed values docuconf returns, not from the raw
environment.

## When the configuration is wrong

With JSON logs and no schema URL, the app exits 1 and says why the variable
is required:

```console
$ DOCUCONF_ROLE=worker DATABASE_URL=postgres://shop@db:5432/shop \
    QUEUES=default LOG_FORMAT=json cargo run -q -p shop
docuconf: 1 configuration problem:
  LOG_SCHEMA_URL: is required when LOG_FORMAT is "json" (missing_required)
```

Without a valid role, every role-specific input is optional, so the role is
the one problem reported. Secrets, such as `DATABASE_URL`, are never
printed.

To run the worker with webhooks on, put a key of at least 32 characters at
`/etc/shop/webhooks/signing.key`, or under `DOCUCONF_FILE_ROOT` (here,
`$root/etc/shop/webhooks/signing.key`) in a test.

## Tests

The tests in [`src/main.rs`](src/main.rs) run this matrix through
`Contract::load_env`. Each case starts from
`DATABASE_URL=postgres://shop@db:5432/shop` and a fresh, empty
`DOCUCONF_FILE_ROOT`:

| # | env (plus the base) | expected |
|---|---|---|
| 1 | `DOCUCONF_ROLE=web ALLOWED_ORIGINS=https://shop.example` | ok |
| 2 | `DOCUCONF_ROLE=worker QUEUES=default,mail` | ok: the worker needs no `ALLOWED_ORIGINS` |
| 3 | `DOCUCONF_ROLE=web` | `ALLOWED_ORIGINS` `missing_required` |
| 4 | `QUEUES=default` (no role) | `DOCUCONF_ROLE` `missing_required`, and nothing else |
| 5 | `DOCUCONF_ROLE=cron` | `DOCUCONF_ROLE` `not_in_enum`, and nothing else |
| 6 | `DOCUCONF_ROLE=worker QUEUES=default LOG_FORMAT=json` | `LOG_SCHEMA_URL` `missing_required`: `is required when LOG_FORMAT is "json"` |
| 7 | as 6, plus `LOG_SCHEMA_URL=https://logs.example/v1.json` | ok |
| 8 | `DOCUCONF_ROLE=worker QUEUES=default WEBHOOKS_ENABLED=true`, no key file | `webhook-signing-key` `file_missing`: `..., and is required when WEBHOOKS_ENABLED is true` |
| 9 | as 8, with a 40-character key file under the root | ok, `webhooks on` |
| 10 | `DOCUCONF_ROLE=web ALLOWED_ORIGINS=https://shop.example WEBHOOKS_ENABLED=true` | ok: the key file belongs to `worker` |
| 11 | `DOCUCONF_ROLE=web ALLOWED_ORIGINS=https://shop.example OTEL_EXPORTER_OTLP_PROTOCOL=carrier-pigeon` | `OTEL_EXPORTER_OTLP_PROTOCOL` `not_in_enum`: fragment variables are checked at boot |

```console
$ cargo test -p shop
```

[`smoke.sh`](smoke.sh) runs the real binary for cases 1, 2 and 6 and checks
its output and exit code; CI runs it on every push.

## Generated docs

[`CONFIG.md`](CONFIG.md) and [`CONFIG.agents.md`](CONFIG.agents.md) are
generated from `contract.cue` by the `docuconf` CLI, through the docs model
in [`docs.json`](docs.json). They list each input's roles and conditions.
Never edit them by hand; regenerate them after changing the contract (CI
fails if they are out of date):

```console
$ docuconf docs contract.cue -o CONFIG.md
$ docuconf docs contract.cue --format agents -o CONFIG.agents.md
$ docuconf docs contract.cue --format model -o docs.json
```
