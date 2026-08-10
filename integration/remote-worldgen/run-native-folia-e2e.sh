#!/usr/bin/env bash
set -euo pipefail

: "${ACCEPT_MINECRAFT_EULA:?set ACCEPT_MINECRAFT_EULA=true after reviewing the Minecraft EULA}"
[[ "$ACCEPT_MINECRAFT_EULA" == true ]] || { echo 'ACCEPT_MINECRAFT_EULA must equal true' >&2; exit 2; }
: "${FOLIA_SOURCE_DIR:?set the pinned Folia upstream source checkout}"
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
OUTPUT=$(python3 - "$OUTPUT" "$ROOT" "$FOLIA_SOURCE_DIR" <<'PY'
from pathlib import Path
import sys
import tempfile
output = Path(sys.argv[1]).resolve()
repository = Path(sys.argv[2]).resolve()
folia = Path(sys.argv[3]).resolve()
repository_artifacts = (repository / "artifacts").resolve()
temporary_root = Path(tempfile.gettempdir()).resolve()
within = lambda child, parent: child != parent and parent in child.parents
overlaps = lambda left, right: left == right or left in right.parents or right in left.parents
allowed_repository_output = within(output, repository_artifacts)
allowed_temporary_output = within(output, temporary_root) and not overlaps(output, repository) and not overlaps(output, folia)
if not (allowed_repository_output or allowed_temporary_output):
    raise SystemExit(f"native evidence output must be below {repository_artifacts} or a separate child of {temporary_root}: {output}")
print(output)
PY
)

[[ "$(git -C "$FOLIA_SOURCE_DIR" rev-parse --is-inside-work-tree 2>/dev/null)" == true ]] || {
  echo "FOLIA_SOURCE_DIR is not a git checkout" >&2
  exit 2
}
[[ "$(git -C "$FOLIA_SOURCE_DIR" rev-parse HEAD)" == "$FOLIA_SOURCE_REVISION" ]] || {
  echo "Folia source must be $FOLIA_SOURCE_REVISION" >&2
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

OVERLAY_DIR="$ROOT/integration/folia-fork/folia-server"
FOLIA_BUILD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/steel-folia-build.XXXXXX")
rmdir "$FOLIA_BUILD_DIR"
cleanup_build() {
  if [[ -n "${FOLIA_BUILD_DIR:-}" ]]; then
    git -C "$FOLIA_SOURCE_DIR" worktree remove --force "$FOLIA_BUILD_DIR" >/dev/null 2>&1 || true
    rm -rf -- "$FOLIA_BUILD_DIR"
    git -C "$FOLIA_SOURCE_DIR" worktree prune >/dev/null 2>&1 || true
  fi
}
trap cleanup_build EXIT
git -C "$FOLIA_SOURCE_DIR" worktree add --detach "$FOLIA_BUILD_DIR" "$FOLIA_SOURCE_REVISION"
cp -a "$OVERLAY_DIR"/. "$FOLIA_BUILD_DIR/folia-server/"
overlay_sha=$(python3 - "$OVERLAY_DIR" "$FOLIA_BUILD_DIR/folia-server" <<'PY'
import hashlib
from pathlib import Path
import subprocess
import sys
source = Path(sys.argv[1])
target = Path(sys.argv[2])
source_files = sorted(path for path in source.rglob("*") if path.is_file())
allowed_changes = {"folia-server/" + path.relative_to(source).as_posix() for path in source_files}
status = subprocess.run(
    ["git", "-C", str(target.parent), "status", "--porcelain=v1", "-z", "--untracked-files=all"],
    check=True,
    stdout=subprocess.PIPE,
).stdout
for record in status.split(b"\0"):
    if not record:
        continue
    code = record[:2].decode("ascii")
    if "R" in code or "C" in code:
        raise SystemExit("renamed/copied files are not allowed in the Folia evidence checkout")
    path = record[3:].decode()
    if path not in allowed_changes:
        raise SystemExit(f"unexpected change in Folia evidence checkout: {code} {path}")
digest = hashlib.sha256()
for path in source_files:
    relative = path.relative_to(source).as_posix().encode()
    data = path.read_bytes()
    installed = target / path.relative_to(source)
    if not installed.is_file() or installed.read_bytes() != data:
        raise SystemExit(f"Folia overlay file is missing or differs: {installed}")
    digest.update(len(relative).to_bytes(8, "big"))
    digest.update(relative)
    digest.update(len(data).to_bytes(8, "big"))
    digest.update(data)
print(digest.hexdigest())
PY
)
java_path=$(command -v "$JAVA_BIN")
export JAVA_HOME
JAVA_HOME=$(dirname "$(dirname "$(readlink -f "$java_path")")")
/usr/bin/timeout 1200s "$FOLIA_BUILD_DIR/gradlew" -p "$FOLIA_BUILD_DIR" \
  applyAllPatches :folia-server:createPaperclipJar --no-daemon --no-configuration-cache
mapfile -t paperclips < <(find "$FOLIA_BUILD_DIR/folia-server/build/libs" -maxdepth 1 -type f -name 'folia-paperclip-*.jar' -print)
[[ "${#paperclips[@]}" == 1 ]] || { printf 'Paperclip build produced %s candidate outputs\n' "${#paperclips[@]}" >&2; exit 2; }
FOLIA_NATIVE_JAR=${paperclips[0]}

rm -rf -- "$OUTPUT"
mkdir -p "$OUTPUT/server/config"
cp "$FOLIA_NATIVE_JAR" "$OUTPUT/server/folia.jar"
cleanup_build
FOLIA_BUILD_DIR=
trap - EXIT
"$JAVA_BIN" -version > "$OUTPUT/java-version.txt" 2>&1
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

python3 - "$OUTPUT" "$FOLIA_SOURCE_REVISION" "$overlay_sha" "$ROOT" "$STEEL_WORLDGEN_BUILD_ID" "$STEEL_WORLDGEN_SOURCE_URL" <<'PY'
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
repository = Path(sys.argv[4])
load = lambda name: json.loads((root / name).read_text())
sha256 = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
def require(condition, message):
    if not condition:
        raise RuntimeError(message)
identity = load("identity.json")
client_native = load("client-native.json")
client_restart = load("client-restart.json")
before = load("metrics-before.json")
after = load("metrics-after-native.json")
restart_before = load("metrics-before-restart.json")
restart_after = load("metrics-after-restart.json")
for client in (client_native, client_restart):
    require(client["ok"] and client["protocol"] == 776, "protocol-776 client phase failed")
    require(all(count >= 9 for count in client["unique_chunks_by_phase"]), "client phase delivered too few chunks")
require(after["succeeded"] > before["succeeded"], "native importer did not generate through the worker")
require(after["failed"] == 0 and after["cancelled"] == 0 and after["in_flight"] == 0, "worker reported failed, cancelled, or active native requests")
require(restart_after == restart_before, "disabled importer contacted the worker during restart")
for name in ("folia-native.log", "folia-restart.log"):
    log = (root / name).read_text(errors="replace").lower()
    require("chunk system error" not in log and "unrecoverablechunksystemfailure" not in log, f"chunk-system failure in {name}")
    require("future status not complete after scheduling" not in log, f"obsolete synchronous-future warning in {name}")
require("steel remote noise enabled" in (root / "folia-native.log").read_text(errors="replace").lower(), "native importer did not enable")
require(identity["build"]["external_build_id"] == sys.argv[5], "worker build identity mismatch")
require(identity["build"]["source_url"] == sys.argv[6], "worker corresponding-source URL mismatch")
raw_names = (
    "identity.json",
    "client-native.json",
    "client-restart.json",
    "metrics-before.json",
    "metrics-after-native.json",
    "metrics-before-restart.json",
    "metrics-after-restart.json",
    "worker.log",
    "folia-native.log",
    "folia-restart.log",
    "java-version.txt",
)
summary = {
    "schema": "steel-native-folia-importer-e2e-v2",
    "created_at_utc": datetime.now(timezone.utc).isoformat(),
    "native_importer": True,
    "persisted_restart": True,
    "folia_upstream_revision": sys.argv[2],
    "folia_overlay_sha256": sys.argv[3],
    "folia_jar_sha256": sha256(root / "server/folia.jar"),
    "harness_sha256": sha256(repository / "integration/remote-worldgen/run-native-folia-e2e.sh"),
    "binary_sha256": {
        "steel-worldgen-service": sha256(repository / "target/release/steel-worldgen-service"),
        "steel-worldgen-probe": sha256(repository / "target/release/steel-worldgen-probe"),
        "steel-worldgen-client-bot": sha256(repository / "integration/client-bot/target/release/steel-worldgen-client-bot"),
    },
    "raw_files_sha256": {name: sha256(root / name) for name in raw_names},
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
