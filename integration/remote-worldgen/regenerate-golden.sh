#!/usr/bin/env bash
set -euo pipefail

: "${UPDATE_GOLDEN:?set UPDATE_GOLDEN=true to replace the checked fixture}"
[[ "$UPDATE_GOLDEN" == true ]] || { echo 'UPDATE_GOLDEN must equal true' >&2; exit 2; }
: "${STEEL_WORLDGEN_BUILD_ID:?set the immutable build identity recorded in the fixture}"
: "${STEEL_WORLDGEN_SOURCE_URL:?set the exact public corresponding-source URL}"
export STEEL_WORLDGEN_BUILD_ID STEEL_WORLDGEN_SOURCE_URL
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PORT=${STEEL_GOLDEN_PORT:-50082}
ASSET="$ROOT/steel-worldgen-service/test_assets/noise-v1-overworld-seed-13579-x-6-z2.pb"
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
  cargo build --release --locked -p steel-worldgen-service \
    --bin steel-worldgen-service --bin steel-worldgen-probe
STEEL_WORLDGEN_SEED=13579 \
STEEL_WORLDGEN_BIND="127.0.0.1:$PORT" \
STEEL_WORLDGEN_THREADS=1 \
"$ROOT/target/release/steel-worldgen-service" > "$TMP/worker.log" 2>&1 &
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
  STEEL_WORLDGEN_ARTIFACT_OUT="$TMP/artifact.pb" \
  "$ROOT/target/release/steel-worldgen-probe" > "$TMP/probe.json"
kill "$worker_pid"
wait "$worker_pid"
worker_pid=
sha256sum "$TMP/artifact.pb" | awk '{print $1}' > "$TMP/artifact.pb.sha256"
python3 - "$TMP/probe.json" "$TMP/artifact.pb.sha256" "$STEEL_WORLDGEN_BUILD_ID" \
  "$TMP/golden-manifest.json" <<'PY'
import json, pathlib, sys
probe = json.loads(pathlib.Path(sys.argv[1]).read_text())
manifest = {
    "schema": "steel-worldgen-golden-v1",
    "requested_build_id": sys.argv[3],
    "artifact_sha256": pathlib.Path(sys.argv[2]).read_text().strip(),
    "artifact_bytes": probe["artifact_bytes"],
    "chunk": probe["chunk"],
    "seed": 13579,
    "profile_sha256": probe["profile_sha256"],
    "generator_sha256": probe["generator_sha256"],
    "registry_sha256": probe["registry_sha256"],
    "build": probe["build"],
}
pathlib.Path(sys.argv[4]).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
PY
(
  cd "$ROOT/integration/folia-plugin"
  STEEL_WORLDGEN_ARTIFACT_TEST_OVERRIDE="$TMP/artifact.pb" \
  STEEL_WORLDGEN_ARTIFACT_TEST_SHA256="$(cat "$TMP/artifact.pb.sha256")" \
    ./gradlew --no-daemon --dependency-verification strict test \
      --tests dev.steelmc.worldgen.GoldenArtifactTest
)
# Replace checked data only after Rust response validation and Java decode/apply both pass.
install -m 0644 "$TMP/artifact.pb" "$ASSET"
install -m 0644 "$TMP/artifact.pb.sha256" "$ASSET.sha256"
install -m 0644 "$TMP/golden-manifest.json" \
  "$ROOT/steel-worldgen-service/test_assets/golden-manifest.json"
printf 'updated %s (%s)\n' "$ASSET" "$(cat "$ASSET.sha256")"
