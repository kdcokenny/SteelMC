#!/usr/bin/env bash
set -euo pipefail

: "${ACCEPT_MINECRAFT_EULA:?set ACCEPT_MINECRAFT_EULA=true after reviewing the Minecraft EULA}"
[[ "$ACCEPT_MINECRAFT_EULA" == true ]] || { echo 'ACCEPT_MINECRAFT_EULA must equal true' >&2; exit 2; }
: "${FOLIA_SOURCE_DIR:?set the pinned patched Folia source checkout}"
: "${FOLIA_NATIVE_JAR:?set the Paperclip JAR built from that checkout}"
: "${STEEL_WORLDGEN_BUILD_ID:?set the immutable Steel build identity}"
: "${STEEL_WORLDGEN_SOURCE_URL:?set the exact public corresponding-source URL}"

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
FOLIA_SOURCE_REVISION=57f643f10e0a9d01024773232d38ae666067d593
OUTPUT=${STEEL_NATIVE_E2E_OUTPUT:-$ROOT/artifacts/native-folia-e2e}
WORKER_PORT=${STEEL_NATIVE_WORKER_PORT:-50091}
SERVER_PORT=${STEEL_NATIVE_SERVER_PORT:-25591}
SEED=${STEEL_NATIVE_SEED:-13579}
WAYPOINTS=${STEEL_CLIENT_WAYPOINTS:-'0,0;512,0;512,512;0,512'}
JAVA_BIN=${JAVA_BIN:-java}

[[ -f "$FOLIA_NATIVE_JAR" ]] || { echo "missing FOLIA_NATIVE_JAR: $FOLIA_NATIVE_JAR" >&2; exit 2; }
[[ -d "$FOLIA_SOURCE_DIR/.git" ]] || { echo "FOLIA_SOURCE_DIR is not a git checkout" >&2; exit 2; }
[[ "$(git -C "$FOLIA_SOURCE_DIR" rev-parse HEAD)" == "$FOLIA_SOURCE_REVISION" ]] || {
  echo "Folia source must be $FOLIA_SOURCE_REVISION" >&2
  exit 2
}
[[ -f "$FOLIA_SOURCE_DIR/folia-server/src/main/java/io/papermc/paper/worldgen/steel/SteelRemoteNoise.java" ]] || {
  echo 'the maintained Steel Folia overlay is not applied' >&2
  exit 2
}
[[ "$($JAVA_BIN -version 2>&1 | awk -F '[".]' '/version/ {print $2; exit}')" == 25 ]] || {
  echo 'Java 25 is required' >&2
  exit 2
}
[[ -z "$(git -C "$ROOT" status --porcelain)" ]] || { echo 'Steel source tree must be clean' >&2; exit 2; }
[[ "$(git -C "$ROOT" rev-parse HEAD)" == "$STEEL_WORLDGEN_BUILD_ID" ]] || {
  echo 'STEEL_WORLDGEN_BUILD_ID must equal Steel HEAD' >&2
  exit 2
}

rm -rf "$OUTPUT"
mkdir -p "$OUTPUT/server/config"
cp "$FOLIA_NATIVE_JAR" "$OUTPUT/server/folia.jar"
printf 'eula=true\n' > "$OUTPUT/server/eula.txt"
cat > "$OUTPUT/server/server.properties" <<EOF
online-mode=false
server-ip=127.0.0.1
level-seed=$SEED
view-distance=4
simulation-distance=4
spawn-protection=0
server-port=$SERVER_PORT
sync-chunk-writes=true
EOF
cat > "$OUTPUT/server/ops.json" <<'EOF'
[{"uuid":"76a51c1b-1113-35b8-979e-c2640f303760","name":"SteelProbe","level":4,"bypassesPlayerLimit":true}]
EOF

export STEEL_WORLDGEN_BUILD_ID STEEL_WORLDGEN_SOURCE_URL
cd "$ROOT"
timeout 900s cargo build --release --locked -p steel-worldgen-service --bins
timeout 900s cargo build --release --locked --manifest-path integration/client-bot/Cargo.toml

worker_pid=
server_pid=
console_fd=
cleanup() {
  [[ -z "$server_pid" ]] || kill "$server_pid" 2>/dev/null || true
  [[ -z "$worker_pid" ]] || kill "$worker_pid" 2>/dev/null || true
  [[ -z "$server_pid" ]] || wait "$server_pid" 2>/dev/null || true
  [[ -z "$worker_pid" ]] || wait "$worker_pid" 2>/dev/null || true
}
trap cleanup EXIT

STEEL_WORLDGEN_SEED="$SEED" \
STEEL_WORLDGEN_BIND="127.0.0.1:$WORKER_PORT" \
STEEL_WORLDGEN_THREADS=1 \
STEEL_WORLDGEN_MAX_IN_FLIGHT=1 \
STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER=1 \
  "$ROOT/target/release/steel-worldgen-service" > "$OUTPUT/worker.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 150); do
  grep -q 'worker ready' "$OUTPUT/worker.log" && break
  kill -0 "$worker_pid"
  sleep 0.1
done
grep -q 'worker ready' "$OUTPUT/worker.log" || { cat "$OUTPUT/worker.log" >&2; exit 1; }

STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" \
STEEL_WORLDGEN_CHUNK_X=1000000 STEEL_WORLDGEN_CHUNK_Z=1000000 \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUTPUT/identity.json"
profile_sha=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["profile_sha256"])' "$OUTPUT/identity.json")
cat > "$OUTPUT/server/config/paper-global.yml" <<EOF
_version: 31
steel-remote-worldgen:
  enabled: true
  target-world: world
  expected-noise-settings: minecraft:overworld
  endpoint: http://127.0.0.1:$WORKER_PORT
  deadline-millis: 120000
  max-in-flight: 1
  max-queued: 4096
  expected-minecraft-version: '26.2'
  expected-profile-sha256: $profile_sha
  plaintext: true
  tls:
    ca-certificate: ''
    client-certificate: ''
    client-key: ''
    domain: ''
EOF

start_server() {
  local log=$1 fifo=$2 fd=$3
  rm -f "$fifo"
  mkfifo "$fifo"
  if [[ "$fd" == 3 ]]; then
    exec 3<>"$fifo"
  else
    exec 4<>"$fifo"
  fi
  (cd "$OUTPUT/server" && "$JAVA_BIN" -Xms1G -Xmx2G -jar folia.jar --nogui <"$fifo") > "$log" 2>&1 &
  server_pid=$!
  for _ in $(seq 1 240); do
    grep -q 'Done (' "$log" && break
    kill -0 "$server_pid"
    sleep 1
  done
  grep -q 'Done (' "$log" || { tail -200 "$log" >&2; exit 1; }
}

stop_server() {
  local fd=$1
  if [[ "$fd" == 3 ]]; then
    echo stop >&3
  else
    echo stop >&4
  fi
  for _ in $(seq 1 120); do
    ! kill -0 "$server_pid" 2>/dev/null && break
    sleep 1
  done
  wait "$server_pid"
  server_pid=
  if [[ "$fd" == 3 ]]; then exec 3>&-; else exec 4>&-; fi
}

start_server "$OUTPUT/folia-native.log" "$OUTPUT/server/console" 3
STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" STEEL_WORLDGEN_METRICS_ONLY=true \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUTPUT/metrics-before.json"
STEEL_CLIENT_ADDRESS="127.0.0.1:$SERVER_PORT" STEEL_CLIENT_DWELL_TICKS=150 \
STEEL_CLIENT_WAYPOINTS="$WAYPOINTS" STEEL_CLIENT_MIN_CHUNKS_PER_PHASE=9 \
  timeout 240s "$ROOT/integration/client-bot/target/release/steel-worldgen-client-bot" > "$OUTPUT/client-native.json"
STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" STEEL_WORLDGEN_METRICS_ONLY=true \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUTPUT/metrics-after-native.json"
stop_server 3

python3 - "$OUTPUT/server/config/paper-global.yml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
marker = "steel-remote-worldgen:\n"
start = text.index(marker)
tail = text[start:].replace("  enabled: true\n", "  enabled: false\n", 1)
path.write_text(text[:start] + tail)
PY

start_server "$OUTPUT/folia-restart.log" "$OUTPUT/server/console-restart" 4
STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" STEEL_WORLDGEN_METRICS_ONLY=true \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUTPUT/metrics-before-restart.json"
STEEL_CLIENT_ADDRESS="127.0.0.1:$SERVER_PORT" STEEL_CLIENT_DWELL_TICKS=100 \
STEEL_CLIENT_WAYPOINTS="$WAYPOINTS" STEEL_CLIENT_MIN_CHUNKS_PER_PHASE=9 \
  timeout 180s "$ROOT/integration/client-bot/target/release/steel-worldgen-client-bot" > "$OUTPUT/client-restart.json"
STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" STEEL_WORLDGEN_METRICS_ONLY=true \
  "$ROOT/target/release/steel-worldgen-probe" > "$OUTPUT/metrics-after-restart.json"
stop_server 4

kill "$worker_pid"
wait "$worker_pid"
worker_pid=
trap - EXIT

python3 - "$OUTPUT" "$FOLIA_SOURCE_REVISION" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
load = lambda name: json.loads((root / name).read_text())
identity = load("identity.json")
client_native = load("client-native.json")
client_restart = load("client-restart.json")
before = load("metrics-before.json")
after = load("metrics-after-native.json")
restart_before = load("metrics-before-restart.json")
restart_after = load("metrics-after-restart.json")
for client in (client_native, client_restart):
    assert client["ok"] and client["protocol"] == 776
    assert all(count >= 9 for count in client["unique_chunks_by_phase"])
assert after["succeeded"] > before["succeeded"]
assert after["failed"] == 0 and after["cancelled"] == 0 and after["in_flight"] == 0
assert restart_after == restart_before, "disabled importer contacted the worker during restart"
for name in ("folia-native.log", "folia-restart.log"):
    log = (root / name).read_text(errors="replace").lower()
    assert "chunk system error" not in log and "unrecoverablechunksystemfailure" not in log
summary = {
    "schema": "steel-native-folia-importer-e2e-v1",
    "native_importer": True,
    "persisted_restart": True,
    "patched_folia_source_revision": sys.argv[2],
    "folia_jar_sha256": hashlib.sha256((root / "server/folia.jar").read_bytes()).hexdigest(),
    "client_native": client_native,
    "client_restart": client_restart,
    "worker_delta_native": {key: after[key] - before[key] for key in ("requests", "succeeded", "failed", "cancelled", "cache_hits")},
    "worker_unchanged_during_restart": True,
    "profile_sha256": identity["profile_sha256"],
    "generator_sha256": identity["generator_sha256"],
    "registry_sha256": identity["registry_sha256"],
    "build": identity["build"],
}
encoded = (json.dumps(summary, indent=2, sort_keys=True) + "\n").encode()
(root / "evidence-summary.json").write_bytes(encoded)
(root / "evidence-summary.json.sha256").write_text(hashlib.sha256(encoded).hexdigest() + "\n")
print(encoded.decode(), end="")
PY
