#!/usr/bin/env bash
# Runs only the docuconf-go-facing tests against a given docuconf-go checkout:
# the shared conformance suite (docuconf/tests/conformance.rs, which fails
# if any case is skipped), the shared export fixture compared with its
# golden contract by `docuconf conformance export`
# (docuconf/tests/conformance_export.rs), and the tests that `cue vet`
# exported contracts against its meta-schema (docuconf/tests/export.rs,
# docuconf/tests/overlays.rs). Not the full suite.
#
#   DOCUCONF_GO_DIR=/path/to/docuconf-go scripts/conformance.sh
#
# Needs a Rust toolchain (1.89+), cue, and the docuconf CLI: $DOCUCONF_CLI,
# else `docuconf` on PATH, else one built here from that checkout (needs go).
# docuconf-go's downstream workflow and this repository's CI both call it.
set -euo pipefail

: "${DOCUCONF_GO_DIR:?set DOCUCONF_GO_DIR to a docuconf-go checkout}"
DOCUCONF_GO_DIR="$(cd "$DOCUCONF_GO_DIR" && pwd)"
export DOCUCONF_GO_DIR
export DOCUCONF_CONFORMANCE="${DOCUCONF_CONFORMANCE:-$DOCUCONF_GO_DIR/conformance/cases.json}"
export DOCUCONF_SPEC_CUE="${DOCUCONF_SPEC_CUE:-$DOCUCONF_GO_DIR/spec/cue}"
export DOCUCONF_REQUIRE_CONFORMANCE=1
export DOCUCONF_REQUIRE_VET=1
export DOCUCONF_REQUIRE_EXPORT=1

if [ -z "${DOCUCONF_CLI:-}" ]; then
  if command -v docuconf >/dev/null 2>&1; then
    DOCUCONF_CLI="$(command -v docuconf)"
  else
    cli_dir="$(mktemp -d)"
    trap 'rm -rf "$cli_dir"' EXIT
    (cd "$DOCUCONF_GO_DIR/cmd/docuconf" && go build -o "$cli_dir/docuconf" .)
    DOCUCONF_CLI="$cli_dir/docuconf"
  fi
fi
export DOCUCONF_CLI

cd "$(dirname "$0")/.."
cargo test --locked -p docuconf --test conformance --test conformance_export --test export --test overlays
