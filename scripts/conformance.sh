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
# Needs a Rust toolchain (1.89+), cue, and the docuconf CLI built from that
# checkout (`go install ./cmd/docuconf`), on PATH or as $DOCUCONF_CLI. docuconf-go's downstream
# workflow and this repository's CI both call it.
set -euo pipefail

: "${DOCUCONF_GO_DIR:?set DOCUCONF_GO_DIR to a docuconf-go checkout}"
DOCUCONF_GO_DIR="$(cd "$DOCUCONF_GO_DIR" && pwd)"
export DOCUCONF_GO_DIR
export DOCUCONF_CONFORMANCE="${DOCUCONF_CONFORMANCE:-$DOCUCONF_GO_DIR/conformance/cases.json}"
export DOCUCONF_SPEC_CUE="${DOCUCONF_SPEC_CUE:-$DOCUCONF_GO_DIR/spec/cue}"
export DOCUCONF_REQUIRE_CONFORMANCE=1
export DOCUCONF_REQUIRE_VET=1
export DOCUCONF_REQUIRE_EXPORT=1

cd "$(dirname "$0")/.."
cargo test --locked -p docuconf --test conformance --test conformance_export --test export --test overlays
