#!/usr/bin/env bash
# Smoke test: runs the real shop binary as each role with valid env, and
# with JSON logs but no schema URL, checking stdout, stderr and the exit
# code. Needs cargo.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$here"
cargo build --quiet --locked -p shop
target_dir="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
bin="$target_dir/debug/shop"

# Each run gets only the base env, its own variables and a fresh, empty
# file root, so nothing from the caller's environment leaks in.
secret='postgres://shop:s3cr3t-pw@db:5432/shop'
run() {
  mkdir -p "$tmp/root"
  env -i PATH="$PATH" DOCUCONF_FILE_ROOT="$tmp/root" DOCUCONF_TERMINATION_LOG="$tmp/termination-log" \
    DATABASE_URL="$secret" "$@" "$bin" >"$tmp/out.txt" 2>"$tmp/err.txt"
}

# check NAME WANT_CODE WANT_STDOUT WANT_STDERR ENV...
check() {
  local name=$1 want_code=$2 want_out=$3 want_err=$4 code=0
  shift 4
  run "$@" || code=$?
  if [ "$code" != "$want_code" ] || [ "$(cat "$tmp/out.txt")" != "$want_out" ] ||
    [ "$(cat "$tmp/err.txt")" != "$want_err" ]; then
    echo "$name: want exit $want_code, got $code; stdout and stderr:" >&2
    cat "$tmp/out.txt" "$tmp/err.txt" >&2
    exit 1
  fi
  if grep -q s3cr3t-pw "$tmp/out.txt" "$tmp/err.txt"; then
    echo "$name: the database password leaked" >&2; exit 1
  fi
  echo "$name: exit $code"
  sed 's/^/  /' "$tmp/out.txt" "$tmp/err.txt"
}

# 1. web, with its required ALLOWED_ORIGINS.
check "1 web" 0 "shop web: port 8080, origins https://shop.example" "" \
  DOCUCONF_ROLE=web ALLOWED_ORIGINS=https://shop.example

# 2. worker: ALLOWED_ORIGINS belongs to web, so the worker needs only QUEUES.
check "2 worker" 0 "shop worker: queues default,mail, webhooks off" "" \
  DOCUCONF_ROLE=worker QUEUES=default,mail

# 6. JSON logs make LOG_SCHEMA_URL required: exit 1 with that one problem.
check "6 json logs, no schema URL" 1 "" 'docuconf: 1 configuration problem:
  LOG_SCHEMA_URL: is required when LOG_FORMAT is "json" (missing_required)' \
  DOCUCONF_ROLE=worker QUEUES=default LOG_FORMAT=json

echo "smoke: ok"
