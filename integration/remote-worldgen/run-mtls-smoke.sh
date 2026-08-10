#!/usr/bin/env bash
set -euo pipefail

: "${STEEL_WORLDGEN_BUILD_ID:?set an immutable source/image identifier}"
: "${STEEL_WORLDGEN_SOURCE_URL:?set the exact public corresponding-source URL}"
export STEEL_WORLDGEN_BUILD_ID STEEL_WORLDGEN_SOURCE_URL
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT=${STEEL_MTLS_OUTPUT:-"$ROOT/artifacts/remote-worldgen-mtls/$(date -u +%Y%m%dT%H%M%SZ)"}
PORT=${STEEL_MTLS_PORT:-50081}
[[ ! -e "$OUT" ]] || { echo "refusing to reuse output: $OUT" >&2; exit 2; }
mkdir -p "$OUT/certs"
command -v openssl >/dev/null

cd "$ROOT"
STEEL_WORLDGEN_BUILD_ID="$STEEL_WORLDGEN_BUILD_ID" \
  cargo build --release --locked -p steel-worldgen-service \
    --bin steel-worldgen-service --bin steel-worldgen-probe

openssl req -x509 -newkey rsa:2048 -nodes -days 1 -sha256 \
  -subj /CN=steel-worldgen-test-ca \
  -keyout "$OUT/certs/ca.key" -out "$OUT/certs/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -sha256 -subj /CN=localhost \
  -keyout "$OUT/certs/server.key" -out "$OUT/certs/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > "$OUT/certs/server.ext"
openssl x509 -req -days 1 -sha256 -in "$OUT/certs/server.csr" \
  -CA "$OUT/certs/ca.crt" -CAkey "$OUT/certs/ca.key" -CAcreateserial \
  -extfile "$OUT/certs/server.ext" -out "$OUT/certs/server.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -sha256 -subj /CN=steel-worldgen-probe \
  -keyout "$OUT/certs/client.key" -out "$OUT/certs/client.csr" >/dev/null 2>&1
printf 'extendedKeyUsage=clientAuth\n' > "$OUT/certs/client.ext"
openssl x509 -req -days 1 -sha256 -in "$OUT/certs/client.csr" \
  -CA "$OUT/certs/ca.crt" -CAkey "$OUT/certs/ca.key" -CAcreateserial \
  -extfile "$OUT/certs/client.ext" -out "$OUT/certs/client.crt" >/dev/null 2>&1

worker_pid=
cleanup() {
  [[ -z "$worker_pid" ]] || kill "$worker_pid" 2>/dev/null || true
  [[ -z "$worker_pid" ]] || wait "$worker_pid" 2>/dev/null || true
}
trap cleanup EXIT
STEEL_WORLDGEN_SEED=13579 \
STEEL_WORLDGEN_BIND="127.0.0.1:$PORT" \
STEEL_WORLDGEN_THREADS=1 \
STEEL_WORLDGEN_MAX_IN_FLIGHT=1 \
STEEL_WORLDGEN_TLS_CERT="$OUT/certs/server.crt" \
STEEL_WORLDGEN_TLS_KEY="$OUT/certs/server.key" \
STEEL_WORLDGEN_TLS_CLIENT_CA="$OUT/certs/ca.crt" \
"$ROOT/target/release/steel-worldgen-service" > "$OUT/worker.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 100); do
  grep -q 'worker ready' "$OUT/worker.log" && break
  kill -0 "$worker_pid"
  sleep 0.1
done
grep -q 'worker ready' "$OUT/worker.log" || { cat "$OUT/worker.log" >&2; exit 1; }

common=(
  STEEL_WORLDGEN_ENDPOINT="https://127.0.0.1:$PORT"
  STEEL_WORLDGEN_CLIENT_CA="$OUT/certs/ca.crt"
  STEEL_WORLDGEN_CLIENT_DOMAIN=localhost
)
if timeout 15s env "${common[@]}" STEEL_WORLDGEN_HEALTH_ONLY=true \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUT/unauthenticated.log" 2>&1; then
  echo 'worker accepted a client without a certificate' >&2
  exit 1
fi

authenticated=(
  "${common[@]}"
  STEEL_WORLDGEN_CLIENT_CERT="$OUT/certs/client.crt"
  STEEL_WORLDGEN_CLIENT_KEY="$OUT/certs/client.key"
)
timeout 15s env "${authenticated[@]}" STEEL_WORLDGEN_HEALTH_ONLY=true \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUT/health.json"
timeout 30s env "${authenticated[@]}" STEEL_WORLDGEN_SEED=13579 \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUT/generate.json"

kill "$worker_pid"
wait "$worker_pid"
worker_pid=
python3 - "$OUT" "$STEEL_WORLDGEN_BUILD_ID" <<'PY'
import datetime, hashlib, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()
generated = json.loads((out / "generate.json").read_text())
summary = {
    "schema": "steel-worldgen-mtls-v1",
    "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "requested_build_id": sys.argv[2],
    "build": generated["build"],
    "profile_sha256": generated["profile_sha256"],
    "authenticated_health": json.loads((out / "health.json").read_text()),
    "authenticated_generate": generated,
    "unauthenticated_client_rejected": True,
    "certificate_sha256": {
        name: sha(out / "certs" / name)
        for name in ("ca.crt", "server.crt", "client.crt")
    },
    "raw_sha256": {
        name: sha(out / name)
        for name in ("health.json", "generate.json", "unauthenticated.log", "worker.log")
    },
}
(out / "evidence-summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
PY
rm -f "$OUT/certs/"*.key "$OUT/certs/"*.csr "$OUT/certs/ca.srl"
printf 'mTLS evidence: %s\n' "$OUT"
