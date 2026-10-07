# Releasing docuconf

The workspace publishes two crates to crates.io, in this order: `docuconf-derive` (the proc macros), then
`docuconf`, which depends on the exact same version of `docuconf-derive`. Both share the workspace version in the
root `Cargo.toml`.

Releases are published by `.github/workflows/release.yml` when a version tag is pushed. It uses
[crates.io trusted publishing](https://crates.io/docs/trusted-publishing): GitHub Actions proves its identity with
OIDC through `rust-lang/crates-io-auth-action`, which exchanges it for a short-lived publish token, so no
crates.io token is stored in the repository.

Both crates are licensed MIT (`license = "MIT"` in the workspace `Cargo.toml`), and each crate directory carries
a copy of `LICENSE` so it is packaged with the crate.

## One-time setup

1. **First publish.** crates.io only lets you add a trusted publisher to a crate that already exists, so a
   maintainer publishes 0.1.0 once by hand, with a token scoped to `publish-new`:
   ```sh
   cargo publish -p docuconf-derive
   cargo publish -p docuconf
   ```
2. **Trusted publisher.** On crates.io, for **each** crate, open Settings → Trusted Publishing and add a GitHub
   publisher: owner `docuconf`, repository `docuconf-rust`, workflow `release.yml`, environment `crates-io`.
   Then revoke the token used for the first publish.
3. **GitHub environment.** In the repository settings create an environment named `crates-io`, limited to tags
   matching `v*`, with required reviewers if a human should approve each release.
4. **Owners.** Add a second owner (a GitHub team such as `github:docuconf:maintainers`) to both crates with
   `cargo owner --add`.

## Each release

1. Update `version` in the root `Cargo.toml` (`[workspace.package]`) and the `docuconf-derive` requirement
   (`version = "=X.Y.Z"`) in `docuconf/Cargo.toml`. Regenerate the golden export if the generator version is in
   it: `UPDATE_GOLDEN=1 cargo test --test export`. Commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.
3. The workflow checks that the tag matches the workspace version, runs fmt, clippy and the full test suite
   (including `cue vet` against the meta-schema), packages both crates, then publishes `docuconf-derive` and
   `docuconf`.

If the second publish fails after the first succeeded, re-run the job: `cargo publish` of an already published
version fails, so the workflow skips a crate whose version is already on crates.io.

## GitHub Packages and Releases

GitHub Packages has no Cargo registry, so the GitHub copy of each release is the GitHub Release. The `github` job in
`.github/workflows/release.yml` runs on the same `v*` tags, repeats the tag check and the checks (`fmt`, `clippy`,
tests, `cargo package`), creates the GitHub Release for the tag if it does not exist, and attaches
`docuconf-derive-<version>.crate` and `docuconf-<version>.crate`, the exact files crates.io would get.

It does not depend on the crates.io `publish` job, so it works before the crates exist on crates.io and before
trusted publishing is set up. It uses only the workflow's own `GITHUB_TOKEN` (`contents: write`); there are no secrets
or accounts to set up, and nothing to configure beyond the `Docuconf` organization allowing `GITHUB_TOKEN` write
access (it does unless restricted under Organization settings > Actions).

### Installing from GitHub

No token is needed for a public repository. Cargo installs from git, so the simplest way to use a release without
crates.io is the tag:

```toml
[dependencies]
docuconf = { git = "https://github.com/Docuconf/docuconf-rust", tag = "v0.1.0" }
```

A `.crate` file is a gzipped tarball of the crate's sources. To use the released files, unpack both and depend on
`docuconf` by path (its `docuconf-derive` dependency then needs a `[patch.crates-io]` entry pointing at the unpacked
`docuconf-derive`):

```sh
mkdir -p vendor
curl -sSL https://github.com/Docuconf/docuconf-rust/releases/download/v0.1.0/docuconf-0.1.0.crate | tar -xz -C vendor
curl -sSL https://github.com/Docuconf/docuconf-rust/releases/download/v0.1.0/docuconf-derive-0.1.0.crate | tar -xz -C vendor
```

```toml
[dependencies]
docuconf = { path = "vendor/docuconf-0.1.0" }

[patch.crates-io]
docuconf-derive = { path = "vendor/docuconf-derive-0.1.0" }
```

## docuconf-go version

docuconf-go owns the spec, the CUE meta-schema (`spec/cue`), the conformance suite (`conformance/cases.json`) and the
`docuconf` CLI. This SDK is tested against one docuconf-go commit, pinned in `.github/docuconf-go.ref` (a full SHA).

- **CI** checks out that commit on pushes and pull requests. The nightly scheduled run uses docuconf-go `main` instead,
  so a spec change that breaks this SDK shows up within a day. To try another docuconf-go commit or branch, run the CI
  workflow by hand (Actions, CI, Run workflow) with `docuconf_go_ref` set. Releases always build against the pinned commit.
- **Bump PRs.** `.github/workflows/docuconf-go-bump.yml` opens (or updates) a `build(deps): bump docuconf-go to <sha>`
  pull request from the `docuconf-go-bump` branch whenever docuconf-go `main` moves: immediately when docuconf-go sends
  a `docuconf-go-updated` dispatch (this needs the release GitHub App), otherwise on its daily schedule. CI on that PR
  is the compatibility check; merge it when it is green, or fix the SDK on the same branch. It can also be run by hand
  with a specific `sha`.
- **`scripts/conformance.sh`** runs only the docuconf-go-facing checks (the conformance suite and the `cue vet` of
  exported contracts) against any checkout: `DOCUCONF_GO_DIR=../docuconf-go scripts/conformance.sh`. CI runs it, and
  so does docuconf-go's downstream workflow, which runs it against every docuconf-go pull request that touches the spec,
  the conformance suite or the CLI. It needs Rust 1.89+ and `cue` on `PATH`.

Without the release App (secrets `RELEASE_APP_ID` and `RELEASE_APP_PRIVATE_KEY`) the bump workflow uses
`GITHUB_TOKEN`: the repository setting "Allow GitHub Actions to create and approve pull requests" must be on, and
because a PR opened that way triggers no workflows, the bump workflow starts CI on the branch itself
(`workflow_dispatch`, whose checks show on the PR).
