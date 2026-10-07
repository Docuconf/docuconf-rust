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

Releases are automated with [release-please](https://github.com/googleapis/release-please); see
[CONTRIBUTING.md](CONTRIBUTING.md#how-releases-happen) for the commit conventions it reads.

1. Merge the open release PR (`chore(main): release X.Y.Z`). It already updates `version` in the root
   `Cargo.toml` (`[workspace.package]`, inherited by both crates), the `docuconf-derive` requirement
   (`version = "=X.Y.Z"`) in `docuconf/Cargo.toml`, both crates in `Cargo.lock`, and `CHANGELOG.md`. The golden
   export and the example contract do not need regenerating: their comparisons ignore
   `metadata.generator.version`.
2. release-please tags the merge commit `vX.Y.Z` and creates the GitHub release with the changelog entries.
3. The workflow checks that the tag matches the workspace version, runs fmt, clippy and the full test suite
   (including `cue vet` against the meta-schema), packages both crates, then publishes `docuconf-derive` and
   `docuconf`.

If the release PR was created with `GITHUB_TOKEN` (no release GitHub App configured), the tag does not trigger
`release.yml` by itself, so `.github/workflows/release-please.yml` starts it with `gh workflow run`. To redo a
release by hand: `gh workflow run release.yml --ref vX.Y.Z`.

If the second publish fails after the first succeeded, re-run the job: `cargo publish` of an already published
version fails, so the workflow skips a crate whose version is already on crates.io.
