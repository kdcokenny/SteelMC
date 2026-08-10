#!/usr/bin/env bash
set -euo pipefail

: "${STEEL_WORLDGEN_BUILD_ID:?set the build identity embedded in the tested worker}"
: "${STEEL_WORLDGEN_SOURCE_URL:?set the exact public corresponding-source URL}"
export STEEL_WORLDGEN_BUILD_ID STEEL_WORLDGEN_SOURCE_URL
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PORT=${STEEL_CROSS_LANGUAGE_PORT:-50083}
TMP=$(mktemp -d)
worker_pid=
cleanup() {
  [[ -z "$worker_pid" ]] || kill "$worker_pid" 2>/dev/null || true
  [[ -z "$worker_pid" ]] || wait "$worker_pid" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT
cd "$ROOT"
STEEL_WORLDGEN_BUILD_ID="$STEEL_WORLDGEN_BUILD_ID" \
  cargo build --locked -p steel-worldgen-service \
    --bin steel-worldgen-service --bin steel-worldgen-probe
STEEL_WORLDGEN_SEED=13579 \
STEEL_WORLDGEN_BIND="127.0.0.1:$PORT" \
STEEL_WORLDGEN_THREADS=1 \
"$ROOT/target/debug/steel-worldgen-service" > "$TMP/worker.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 100); do
  grep -q 'worker ready' "$TMP/worker.log" && break
  kill -0 "$worker_pid"
  sleep 0.1
done
grep -q 'worker ready' "$TMP/worker.log" || { cat "$TMP/worker.log" >&2; exit 1; }
timeout 30s env \
  STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$PORT" \
  STEEL_WORLDGEN_SEED=13579 \
  STEEL_WORLDGEN_CHUNK_X=-6 \
  STEEL_WORLDGEN_CHUNK_Z=2 \
  STEEL_WORLDGEN_ARTIFACT_OUT="$TMP/current.pb" \
  "$ROOT/target/debug/steel-worldgen-probe" > "$TMP/probe.json"
artifact_sha256=$(sha256sum "$TMP/current.pb" | awk '{print $1}')
(
  cd "$ROOT/integration/folia-plugin"
  STEEL_WORLDGEN_ARTIFACT_TEST_OVERRIDE="$TMP/current.pb" \
  STEEL_WORLDGEN_ARTIFACT_TEST_SHA256="$artifact_sha256" \
    ./gradlew --no-daemon --dependency-verification strict test \
      --tests dev.steelmc.worldgen.GoldenArtifactTest
)
python3 - "$TMP/probe.json" "$artifact_sha256" <<'PY'
import json, sys
probe = json.load(open(sys.argv[1]))
assert probe["artifact_sha256"] == sys.argv[2]
print(json.dumps({
    "artifact_sha256": sys.argv[2],
    "profile_sha256": probe["profile_sha256"],
    "build": probe["build"],
    "java_decode_and_apply": True,
}, sort_keys=True))
PY
