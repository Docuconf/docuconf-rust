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
