# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately, through GitHub's private vulnerability reporting: open the repository's
**Security** tab and choose **Report a vulnerability**
([direct link](https://github.com/docuconf/docuconf-rust/security/advisories/new)). Do not open a public issue,
pull request or discussion for a suspected vulnerability.

Include what you can of:

- the affected crate and version (`docuconf`, `docuconf-derive`) and the Rust version;
- what an attacker can do, and what they need first;
- steps or a minimal contract, environment or program that reproduces it.

We work on the fix in a private security advisory, credit you in it unless you prefer otherwise, and publish the
advisory when a fixed release is out.

## Response targets

| | |
|---|---|
| Acknowledge the report | within 3 business days |
| First assessment (confirmed or not, severity) | as soon as we can reproduce it, and we keep you updated in the advisory |
| Fix | released as a patch to the supported version, then the advisory is published |

## Supported versions

Both crates are released together, with one workspace version (see [RELEASING.md](RELEASING.md)). Security fixes
go to the latest minor release, as a new patch release:

| Crate | Tag | Supported |
|---|---|---|
| `docuconf` | `v*` | latest minor |
| `docuconf-derive` | `v*` | latest minor |

**During the beta, only the latest release is supported.** Upgrade to it to get a fix.

## Scope

In scope:

- the `docuconf` crate, for example a value that loads but should not, or a secret value (a `Secret`, a
  `KeySet` key, a secret file) that reaches an error message, a log line, `Debug` or `Serialize` output;
- the `docuconf-derive` proc macros.

Out of scope: the example application under [`examples`](examples), vulnerabilities in dependencies that docuconf
does not make reachable (report those upstream), and issues in a platform or cluster that only arise from its own
misconfiguration. The CLI, the CUE meta-schema, the Helm chart and the other language SDKs live in their own
repositories and follow their own policies; the CLI and the meta-schema are in
[docuconf-go](https://github.com/docuconf/docuconf-go/security).
