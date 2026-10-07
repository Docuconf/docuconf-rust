# Example: orders

A tiny HTTP service, on nothing but `std::net`, whose configuration is
declared with docuconf. It shows the three things the Rust SDK gives an app:

- a normal serde struct, with `#[derive(Docuconf)]` and `#[docuconf(...)]`
  attributes for descriptions, secrets and constraints
  ([`src/main.rs`](src/main.rs));
- one check at boot, `docuconf::load()`, that reports every problem at once
  with stable codes;
- a CUE contract exported from the struct, for the platform to validate
  before it deploys ([`contract.cue`](contract.cue)).

| Variable | Type | Rules |
|---|---|---|
| `PORT` | int | 1–65535, default `8080` |
| `LOG_LEVEL` | enum | `debug`, `info`, `warn`, `error`; default `info` |
| `DATABASE_URL` | url | secret, required, scheme `postgres` |
| `ALLOWED_ORIGINS` | list of strings, a JSON array | at least 1 item; default `["http://localhost:3000"]` |
| `REQUEST_TIMEOUT` | duration, Go syntax | `1s`–`5m`, default `30s` |
| `WORKER_COUNT` | int | 1–64, default `4` |

The example is a member of this repository's Cargo workspace and depends on
the SDK by path, so it always builds against the code next to it.

## Run it

```console
$ cd examples/orders
$ DATABASE_URL=postgres://orders:pw@localhost:5432/orders cargo run -p orders
orders listening on :8080 with 4 workers, database localhost, log level Info
$ curl localhost:8080/healthz
ok
$ curl localhost:8080/config
{"ALLOWED_ORIGINS":["http://localhost:3000"],"DATABASE_URL":"***","LOG_LEVEL":"info","PORT":8080,"REQUEST_TIMEOUT":"30s","WORKER_COUNT":4}
```

`/config` shows the typed values; the secret is always `***`.

## When the configuration is wrong

With `PORT=0` and no `DATABASE_URL`, the service refuses to start, exits 1
and lists every problem, not just the first:

```console
$ PORT=0 cargo run -q -p orders
docuconf: 2 configuration problems:
  DATABASE_URL: is required but not set (missing_required)
  PORT: 0 is below min 1 (out_of_range)
$ echo $?
1
```

[`smoke.sh`](smoke.sh) checks both runs; CI runs it on every push.

## Export the contract

`contract.cue` is generated; never edit it by hand. The app exports its own
contract with `docuconf::export`, so re-export it after changing the struct
(CI fails if it is out of date):

```console
$ cd examples/orders
$ cargo run -p orders -- export contract.cue
```

## Deploy

The app ships `contract.cue`, and the platform checks its inputs against it
before anything reaches the cluster: `docuconf vet` reports every bad or
missing value, secret given as a literal or policy violation, and
`docuconf render` turns valid inputs into the pod's env. A Helm-based
platform can use the
[docuconf Helm chart](https://github.com/docuconf/docuconf-go/tree/main/helm),
which generates a `values.schema.json` from the contract. At boot,
`docuconf::load()` checks the same rules again.
