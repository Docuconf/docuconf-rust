#!/usr/bin/env bash
# Smoke test: starts the orders service with valid env and checks its
# endpoints, then starts it with bad env and checks it refuses to boot,
# and posts webhooks signed with each key of a key set that is
# mid-rotation. Needs cargo, curl and openssl.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
pid=""
trap '[ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$tmp"' EXIT
cd "$here"
cargo build --quiet --locked -p orders
target_dir="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
bin="$target_dir/debug/orders"

secret='postgres://orders:s3cr3t-pw@localhost:5432/orders'
# Two webhook keys: the old one and, mid-rotation, the new one.
old_key='old-webhook-key-0123456789abcdef0123'
new_key='new-webhook-key-0123456789abcdef0123'
# A random port, so parallel runs and other services do not collide.
port=${ORDERS_SMOKE_PORT:-$((20000 + RANDOM % 20000))}

# 1. Valid env: the service serves /healthz and /config, without the
# secrets; a webhook without a valid signature gets 401.
PORT=$port DATABASE_URL="$secret" WEBHOOK_KEYS="$old_key,$new_key" "$bin" >"$tmp/out.txt" 2>&1 &
pid=$!
for _ in $(seq 50); do
  curl -fsS "http://127.0.0.1:$port/healthz" >"$tmp/healthz" 2>/dev/null && break
  sleep 0.1
done
kill -0 "$pid" 2>/dev/null || { echo "the service exited:" >&2; cat "$tmp/out.txt" >&2; exit 1; }
[ "$(cat "$tmp/healthz" 2>/dev/null)" = ok ] || { echo "GET /healthz did not return ok" >&2; cat "$tmp/out.txt" >&2; exit 1; }
curl -fsS "http://127.0.0.1:$port/config" >"$tmp/config.json"
if grep -q -e 's3cr3t-pw' -e 'webhook-key' "$tmp/config.json" "$tmp/out.txt"; then
  echo "a secret leaked into /config or the log" >&2; exit 1
fi
grep -q '"DATABASE_URL":"\*\*\*"' "$tmp/config.json" || { echo "GET /config did not redact DATABASE_URL" >&2; exit 1; }
grep -q '"WEBHOOK_KEYS":"\*\*\*"' "$tmp/config.json" || { echo "GET /config did not redact WEBHOOK_KEYS" >&2; exit 1; }
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'X-Signature: 00' -d '{}' "http://127.0.0.1:$port/webhooks/payments")
[ "$code" = 401 ] || { echo "an unsigned webhook got $code, want 401" >&2; exit 1; }
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -d '{}' "http://127.0.0.1:$port/webhooks/payments")
[ "$code" = 401 ] || { echo "a webhook without X-Signature got $code, want 401" >&2; exit 1; }
echo "valid env: /healthz ok, /config $(cat "$tmp/config.json")"

# 2. Mid-rotation, a webhook signed with either key is accepted, and one
# signed with any other key is not.
body='{"order":"42","status":"paid"}'
for key in "$old_key" "$new_key" "other-webhook-key-0123456789abcdef"; do
  sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$key" | sed 's/.*= //')
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "X-Signature: $sig" -d "$body" "http://127.0.0.1:$port/webhooks/payments")
  want=204; [ "${key#other}" != "$key" ] && want=401
  [ "$code" = "$want" ] || { echo "webhook signed with ${key%%-*} key: got $code, want $want" >&2; exit 1; }
done
echo "webhooks: old and new key accepted, any other rejected"
kill "$pid"; wait "$pid" 2>/dev/null || true; pid=""

# 3. PORT=0 and no DATABASE_URL: the service exits non-zero and names both.
if env -u DATABASE_URL PORT=0 "$bin" >"$tmp/bad.txt" 2>&1; then
  echo "service started with PORT=0 and no DATABASE_URL" >&2; exit 1
fi
for code in missing_required out_of_range; do
  grep -q "$code" "$tmp/bad.txt" || { echo "startup output lacks $code:" >&2; cat "$tmp/bad.txt" >&2; exit 1; }
done
echo "bad env: exited non-zero with:"
sed 's/^/  /' "$tmp/bad.txt"

# 4. A key set with an empty second key (a trailing comma): an empty key
# is never valid, so it fails at boot, without printing the key.
set +e
PORT=$port DATABASE_URL="$secret" WEBHOOK_KEYS="$old_key," "$bin" >"$tmp/bad.txt" 2>&1
code=$?
set -e
cat >"$tmp/want.txt" <<'WANT'
docuconf: 1 configuration problem:
  WEBHOOK_KEYS: key 2 is empty (out_of_range)
WANT
if [ "$code" != 1 ] || ! diff -u "$tmp/want.txt" "$tmp/bad.txt" || grep -q webhook-key "$tmp/bad.txt"; then
  echo "want exit 1 for an empty webhook key, got $code:" >&2; cat "$tmp/bad.txt" >&2; exit 1
fi
echo "empty webhook key: exited 1"
echo "smoke: ok"
