#!/usr/bin/env bash
set -euo pipefail

# Reproducible cold Folia + Steel worker + protocol-776 Azalea exploration test.
: "${FOLIA_JAR:?set FOLIA_JAR to the published Folia 26.2 build-1 jar}"
: "${FOLIA_JAR_SHA256:?set FOLIA_JAR_SHA256 to the exact supplied jar SHA-256}"
: "${STEEL_WORLDGEN_BUILD_ID:?set STEEL_WORLDGEN_BUILD_ID to an immutable source/image identifier}"
: "${STEEL_WORLDGEN_SOURCE_URL:?set the exact public corresponding-source URL}"
export STEEL_WORLDGEN_BUILD_ID STEEL_WORLDGEN_SOURCE_URL
: "${ACCEPT_MINECRAFT_EULA:?set ACCEPT_MINECRAFT_EULA=true after reading the Minecraft EULA}"
[[ "$ACCEPT_MINECRAFT_EULA" == true ]] || { echo "ACCEPT_MINECRAFT_EULA must equal true" >&2; exit 2; }
PINNED_FOLIA_JAR_SHA256=6726da42d6a4edc4961a43cdccfd7ebf5fea75e7b1342266532ed143df6736e7
PINNED_FOLIA_BUILD_REVISION=e48800d
[[ "$FOLIA_JAR_SHA256" == "$PINNED_FOLIA_JAR_SHA256" ]] || {
  echo "FOLIA_JAR_SHA256 must equal the pinned Folia 26.2 build-1 digest" >&2
  exit 2
}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT=${STEEL_E2E_OUTPUT:-"$ROOT/artifacts/remote-worldgen-e2e/$(date -u +%Y%m%dT%H%M%SZ)"}
RUN="$OUT/server"
WORKER_PORT=${STEEL_E2E_WORKER_PORT:-50051}
SERVER_PORT=${STEEL_E2E_SERVER_PORT:-25585}
[[ ! -e "$OUT" ]] || { echo "E2E output already exists; refusing to reuse a non-cold world: $OUT" >&2; exit 2; }
mkdir -p "$RUN/plugins/SteelRemoteWorldGen"
actual_folia_sha256=$(sha256sum "$FOLIA_JAR" | awk '{print $1}')
[[ "$actual_folia_sha256" == "$FOLIA_JAR_SHA256" ]] || {
  echo "Folia jar SHA-256 mismatch: expected $FOLIA_JAR_SHA256, got $actual_folia_sha256" >&2
  exit 2
}
java_major=$(java -version 2>&1 | sed -n '1s/.*version "\([0-9]*\).*/\1/p')
[[ "$java_major" == 25 ]] || { echo "Java 25 is required, found ${java_major:-unknown}" >&2; exit 2; }
steel_head=$(git -C "$ROOT" rev-parse HEAD)
[[ "$STEEL_WORLDGEN_BUILD_ID" == "$steel_head" ]] || {
  echo "E2E build ID must equal clean Steel HEAD $steel_head" >&2
  exit 2
}
[[ -z "$(git -C "$ROOT" status --porcelain)" ]] || {
  echo "E2E evidence requires a clean source tree" >&2
  git -C "$ROOT" status --short >&2
  exit 2
}
java -version > "$OUT/java-version.txt" 2>&1
rustc --version --verbose > "$OUT/rustc-version.txt"
cargo --version --verbose > "$OUT/cargo-version.txt"
uname -a > "$OUT/uname.txt"
(lscpu || true) > "$OUT/lscpu.txt"
printf '%s\n' "$steel_head" > "$OUT/steel-head.txt"

cd "$ROOT"
STEEL_WORLDGEN_BUILD_ID="$STEEL_WORLDGEN_BUILD_ID" \
  cargo build --release --locked -p steel-worldgen-service \
    --bin steel-worldgen-service --bin steel-worldgen-probe --bin steel-worldgen-bench
(cd integration/folia-plugin && ./gradlew --no-daemon --dependency-verification strict test shadowJar)
(cd integration/client-bot && cargo test --release --locked && cargo build --release --locked)

cp "$FOLIA_JAR" "$RUN/folia.jar"
cp integration/folia-plugin/build/libs/steel-remote-worldgen-folia-0.1.0+mc26.2.jar "$RUN/plugins/"
python3 - "$RUN/plugins/steel-remote-worldgen-folia-0.1.0+mc26.2.jar" \
  "$STEEL_WORLDGEN_BUILD_ID" "$STEEL_WORLDGEN_SOURCE_URL" <<'PY'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as archive:
    notice = archive.read("META-INF/SteelMC-SOURCE.txt").decode()
for expected in sys.argv[2:]:
    if expected not in notice:
        raise SystemExit(f"plugin source notice is missing {expected!r}")
PY
printf 'eula=true\n' > "$RUN/eula.txt"
cat > "$RUN/server.properties" <<EOF
online-mode=false
server-ip=127.0.0.1
level-seed=13579
view-distance=4
simulation-distance=4
spawn-protection=0
server-port=$SERVER_PORT
motd=Steel remote worldgen integration test
sync-chunk-writes=true
EOF
cat > "$RUN/bukkit.yml" <<'EOF'
settings:
  allow-end: false
worlds:
  world:
    # Deliberately keep Folia's native biome provider. The prototype's remote
    # BiomeProvider would over-generate NOISE for structure biome searches.
    generator: SteelRemoteWorldGen:overworld
EOF
cat > "$RUN/ops.json" <<'EOF'
[{"uuid":"76a51c1b-1113-35b8-979e-c2640f303760","name":"SteelProbe","level":4,"bypassesPlayerLimit":true}]
EOF

worker_pid=
worker2_pid=
server_pid=
cleanup() {
  [[ -z "$server_pid" ]] || kill "$server_pid" 2>/dev/null || true
  [[ -z "$worker2_pid" ]] || kill "$worker2_pid" 2>/dev/null || true
  [[ -z "$worker_pid" ]] || kill "$worker_pid" 2>/dev/null || true
  [[ -z "$server_pid" ]] || wait "$server_pid" 2>/dev/null || true
  [[ -z "$worker2_pid" ]] || wait "$worker2_pid" 2>/dev/null || true
  [[ -z "$worker_pid" ]] || wait "$worker_pid" 2>/dev/null || true
}
trap cleanup EXIT

STEEL_WORLDGEN_SEED=13579 \
STEEL_WORLDGEN_GENERATOR=minecraft:overworld \
STEEL_WORLDGEN_BIND="127.0.0.1:$WORKER_PORT" \
STEEL_WORLDGEN_THREADS=1 \
STEEL_WORLDGEN_MAX_IN_FLIGHT=64 \
STEEL_WORLDGEN_MAX_CACHE_ENTRIES=4096 \
STEEL_WORLDGEN_MAX_CACHE_BYTES=1073741824 \
STEEL_WORLDGEN_REQUEST_TIMEOUT_MS=120000 \
"$ROOT/target/release/steel-worldgen-service" > "$OUT/worker-preflight.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 60); do
  grep -q 'worker ready' "$OUT/worker-preflight.log" && break
  kill -0 "$worker_pid"
  sleep 1
done
grep -q 'worker ready' "$OUT/worker-preflight.log" || { cat "$OUT/worker-preflight.log" >&2; exit 1; }

STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" \
"$ROOT/target/release/steel-worldgen-probe" > "$OUT/profile-probe.json"
profile_sha256=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["profile_sha256"])' < "$OUT/profile-probe.json")

# Prove active cancellation drains and the same position can be retried.
timeout 45s env   STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT"   STEEL_WORLDGEN_SEED=13579   STEEL_WORLDGEN_CANCEL_TEST_TIMEOUT_MS=30000   STEEL_WORLDGEN_CHUNK_X=12345   STEEL_WORLDGEN_CHUNK_Z=-12345   STEEL_WORLDGEN_CANCEL_TEST=true   "$ROOT/target/release/steel-worldgen-probe" > "$OUT/cancellation.json"
timeout 30s env   STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT"   STEEL_WORLDGEN_SEED=13579   STEEL_WORLDGEN_CHUNK_X=12345   STEEL_WORLDGEN_CHUNK_Z=-12345   "$ROOT/target/release/steel-worldgen-probe" > "$OUT/cancellation-retry.json"

# Drain and stop the preflight worker so both determinism workers begin with empty RAM caches.
kill "$worker_pid"
wait "$worker_pid"
worker_pid=
STEEL_WORLDGEN_SEED=13579 STEEL_WORLDGEN_GENERATOR=minecraft:overworld \
STEEL_WORLDGEN_BIND="127.0.0.1:$WORKER_PORT" STEEL_WORLDGEN_THREADS=1 \
STEEL_WORLDGEN_MAX_IN_FLIGHT=64 STEEL_WORLDGEN_MAX_CACHE_ENTRIES=4096 \
STEEL_WORLDGEN_MAX_CACHE_BYTES=1073741824 STEEL_WORLDGEN_REQUEST_TIMEOUT_MS=120000 \
"$ROOT/target/release/steel-worldgen-service" > "$OUT/worker-determinism-1.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 60); do
  grep -q 'worker ready' "$OUT/worker-determinism-1.log" && break
  kill -0 "$worker_pid"
  sleep 1
done
grep -q 'worker ready' "$OUT/worker-determinism-1.log" || { cat "$OUT/worker-determinism-1.log" >&2; exit 1; }

# Prove two clean, identically built worker processes return identical bytes.
timeout 30s env   STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT"   STEEL_WORLDGEN_SEED=13579   STEEL_WORLDGEN_CHUNK_X=-6   STEEL_WORLDGEN_CHUNK_Z=2   STEEL_WORLDGEN_ARTIFACT_OUT="$OUT/determinism-worker-1.pb"   "$ROOT/target/release/steel-worldgen-probe" > "$OUT/determinism-worker-1.json"
WORKER2_PORT=${STEEL_E2E_SECOND_WORKER_PORT:-$((WORKER_PORT + 1))}
STEEL_WORLDGEN_SEED=13579 STEEL_WORLDGEN_GENERATOR=minecraft:overworld STEEL_WORLDGEN_BIND="127.0.0.1:$WORKER2_PORT" STEEL_WORLDGEN_THREADS=1 STEEL_WORLDGEN_MAX_IN_FLIGHT=64 STEEL_WORLDGEN_MAX_CACHE_ENTRIES=4096 STEEL_WORLDGEN_MAX_CACHE_BYTES=1073741824 STEEL_WORLDGEN_REQUEST_TIMEOUT_MS=120000 "$ROOT/target/release/steel-worldgen-service" > "$OUT/worker-2.log" 2>&1 &
worker2_pid=$!
for _ in $(seq 1 60); do
  grep -q 'worker ready' "$OUT/worker-2.log" && break
  kill -0 "$worker2_pid"
  sleep 1
done
grep -q 'worker ready' "$OUT/worker-2.log" || { cat "$OUT/worker-2.log" >&2; exit 1; }
timeout 30s env   STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER2_PORT"   STEEL_WORLDGEN_SEED=13579   STEEL_WORLDGEN_CHUNK_X=-6   STEEL_WORLDGEN_CHUNK_Z=2   STEEL_WORLDGEN_ARTIFACT_OUT="$OUT/determinism-worker-2.pb"   "$ROOT/target/release/steel-worldgen-probe" > "$OUT/determinism-worker-2.json"
cmp "$OUT/determinism-worker-1.pb" "$OUT/determinism-worker-2.pb"
kill "$worker2_pid"
wait "$worker2_pid"
worker2_pid=
kill "$worker_pid"
wait "$worker_pid"
worker_pid=

# The measured Folia phase gets a third fresh process and an empty artifact cache.
STEEL_WORLDGEN_SEED=13579 STEEL_WORLDGEN_GENERATOR=minecraft:overworld \
STEEL_WORLDGEN_BIND="127.0.0.1:$WORKER_PORT" STEEL_WORLDGEN_THREADS=1 \
STEEL_WORLDGEN_MAX_IN_FLIGHT=64 STEEL_WORLDGEN_MAX_CACHE_ENTRIES=4096 \
STEEL_WORLDGEN_MAX_CACHE_BYTES=1073741824 STEEL_WORLDGEN_REQUEST_TIMEOUT_MS=120000 \
"$ROOT/target/release/steel-worldgen-service" > "$OUT/worker.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 60); do
  grep -q 'worker ready' "$OUT/worker.log" && break
  kill -0 "$worker_pid"
  sleep 1
done
grep -q 'worker ready' "$OUT/worker.log" || { cat "$OUT/worker.log" >&2; exit 1; }

cat > "$RUN/plugins/SteelRemoteWorldGen/config.yml" <<EOF
worker:
  endpoint: "http://127.0.0.1:$WORKER_PORT"
  plaintext: true
  deadline-millis: 120000
  expected-minecraft-version: "26.2"
  expected-profile-sha256: "$profile_sha256"
  allow-unpinned-profile: false
cache:
  max-chunks: 4096
  max-bytes: 268435456
prototype:
  allow-unapplied-postprocessing: true
EOF

(cd "$RUN" && java -Xms1G -Xmx2G -jar folia.jar --nogui) > "$OUT/folia.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 180); do
  grep -q 'Done (' "$OUT/folia.log" && break
  kill -0 "$server_pid"
  sleep 1
done
grep -q 'Done (' "$OUT/folia.log" || { tail -150 "$OUT/folia.log" >&2; exit 1; }
grep -Fq "Folia 26.2-1-ver/26.2.x@$PINNED_FOLIA_BUILD_REVISION" "$OUT/folia.log" || {
  echo 'published Folia build revision mismatch' >&2
  exit 1
}
grep -Fq "Pinned Steel worker profile $profile_sha256" "$OUT/folia.log" || {
  echo 'Steel plugin did not load the pinned worker profile' >&2
  exit 1
}
grep -Fq 'Bukkit bridge is a synchronous projection prototype' "$OUT/folia.log" || {
  echo 'Steel plugin did not reach onEnable' >&2
  exit 1
}
STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" \
STEEL_WORLDGEN_METRICS_ONLY=true \
"$ROOT/target/release/steel-worldgen-probe" > "$OUT/metrics-before-client.json"

STEEL_CLIENT_ADDRESS="127.0.0.1:$SERVER_PORT" \
STEEL_CLIENT_USERNAME=SteelProbe \
STEEL_CLIENT_DWELL_TICKS=200 \
STEEL_CLIENT_VIEW_DISTANCE=4 \
timeout 90s "$ROOT/integration/client-bot/target/release/steel-worldgen-client-bot" \
  > "$OUT/client.log" 2>&1
python3 - "$OUT/client.log" "$OUT/client-summary.json" <<'PY'
import json, pathlib, sys
objects = []
for line in pathlib.Path(sys.argv[1]).read_text().splitlines():
    try:
        objects.append(json.loads(line))
    except json.JSONDecodeError:
        pass
if not objects or not objects[-1].get("ok"):
    raise SystemExit(f"client probe failed: {objects[-1] if objects else 'no JSON result'}")
summary = json.dumps(objects[-1], indent=2, sort_keys=True) + "\n"
pathlib.Path(sys.argv[2]).write_text(summary)
print(summary, end="")
PY

STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" \
STEEL_WORLDGEN_METRICS_ONLY=true \
"$ROOT/target/release/steel-worldgen-probe" > "$OUT/metrics-after-client.json"
python3 -   "$OUT/metrics-before-client.json"   "$OUT/metrics-after-client.json"   "$OUT/client-summary.json"   "$OUT/client-worker-delta.json" <<'PY'
import json, pathlib, sys
before = json.loads(pathlib.Path(sys.argv[1]).read_text())
after = json.loads(pathlib.Path(sys.argv[2]).read_text())
client = json.loads(pathlib.Path(sys.argv[3]).read_text())
delta = after["succeeded"] - before["succeeded"]
delivered = sum(client["unique_chunks_by_phase"])
if delta < delivered:
    raise SystemExit(f"only {delta} successful worker RPCs for {delivered} phase-delivered chunks")
if after["failed"] != before["failed"] or after["cancelled"] != before["cancelled"]:
    raise SystemExit("worker recorded a failure or cancellation during client exploration")
if before.get("poisoned") or after.get("poisoned"):
    raise SystemExit("worker was quarantined during client exploration")
summary = {
    "successful_worker_rpcs_during_client": delta,
    "phase_unique_chunks_total": delivered,
    "worker_failures_during_client": 0,
    "worker_cancellations_during_client": 0,
}
encoded = json.dumps(summary, indent=2, sort_keys=True) + "\n"
pathlib.Path(sys.argv[4]).write_text(encoded)
print(encoded, end="")
PY

kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true
server_pid=
STEEL_WORLDGEN_ENDPOINT="http://127.0.0.1:$WORKER_PORT" \
STEEL_WORLDGEN_BENCH_SIDE=1 STEEL_WORLDGEN_BENCH_CONCURRENCY=1 \
STEEL_WORLDGEN_BENCH_WARM_PASSES=0 \
STEEL_WORLDGEN_BENCH_START_X=20000 STEEL_WORLDGEN_BENCH_START_Z=20000 \
"$ROOT/target/release/steel-worldgen-bench" > "$OUT/worker-metrics.json"
if grep -q 'ERROR.*ChunkTaskScheduler' "$OUT/folia.log"; then
  echo 'Moonrise reported a chunk-system error' >&2
  exit 1
fi
if grep -qE 'zip file closed|Failed to submit a listener notification task|Unexpected exception from an event executor' "$OUT/folia.log"; then
  echo 'Plugin gRPC transport did not shut down before its classloader closed' >&2
  exit 1
fi
plugin_jar="$RUN/plugins/steel-remote-worldgen-folia-0.1.0+mc26.2.jar"
python3 -   "$OUT"   "$FOLIA_JAR_SHA256"   "$(sha256sum "$plugin_jar" | awk '{print $1}')"   "$STEEL_WORLDGEN_BUILD_ID"   "$steel_head" <<'PY'
import datetime, hashlib, json, pathlib, sys
out = pathlib.Path(sys.argv[1])
def load(name):
    return json.loads((out / name).read_text())
def sha(name):
    return hashlib.sha256((out / name).read_bytes()).hexdigest()
profile = load("profile-probe.json")
client = load("client-summary.json")
delta = load("client-worker-delta.json")
summary = {
    "schema": "steel-remote-worldgen-e2e-v3",
    "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "pins": {
        "steel_head": sys.argv[5],
        "steel_version": profile["steel_version"],
        "minecraft_version": profile["minecraft_version"],
        "folia_published_build_revision": "e48800d",
        "maintained_importer_upstream_pin": "57f643f10e0a9d01024773232d38ae666067d593",
        "paper_commit": "1f7285664c3a11690a641e19ffcc90321fcc7fde",
        "azalea_commit": "6249c295d353b9b3ef68f665b311cba39211fd19",
        "folia_jar_sha256": sys.argv[2],
        "plugin_jar_sha256": sys.argv[3],
    },
    "build": profile["build"],
    "requested_build_id": sys.argv[4],
    "profile_sha256": profile["profile_sha256"],
    "generator_sha256": profile["generator_sha256"],
    "registry_sha256": profile["registry_sha256"],
    "determinism": {
        "chunk": [-6, 2],
        "artifact_sha256": sha("determinism-worker-1.pb"),
        "two_clean_processes_equal": True,
    },
    "cancellation": {
        "active_cancel_and_drain": load("cancellation.json"),
        "same_position_retry": load("cancellation-retry.json"),
    },
    "client": client,
    "worker_during_client": delta,
    "benchmark_smoke": load("worker-metrics.json"),
    "binary_sha256": {
        "steel-worldgen-service": hashlib.sha256((pathlib.Path.cwd() / "target/release/steel-worldgen-service").read_bytes()).hexdigest(),
        "steel-worldgen-probe": hashlib.sha256((pathlib.Path.cwd() / "target/release/steel-worldgen-probe").read_bytes()).hexdigest(),
        "steel-worldgen-bench": hashlib.sha256((pathlib.Path.cwd() / "target/release/steel-worldgen-bench").read_bytes()).hexdigest(),
        "client-bot": hashlib.sha256((pathlib.Path.cwd() / "integration/client-bot/target/release/steel-worldgen-client-bot").read_bytes()).hexdigest(),
    },
}
raw = [
    "profile-probe.json", "cancellation.json", "cancellation-retry.json",
    "determinism-worker-1.json", "determinism-worker-2.json",
    "client-summary.json", "client-worker-delta.json",
    "metrics-before-client.json", "metrics-after-client.json",
    "worker-metrics.json", "worker-preflight.log", "worker-determinism-1.log",
    "worker.log", "worker-2.log", "folia.log", "java-version.txt",
    "rustc-version.txt", "cargo-version.txt", "uname.txt", "lscpu.txt", "steel-head.txt",
]
summary["raw_sha256"] = {name: sha(name) for name in raw}
summary["harness_sha256"] = hashlib.sha256((pathlib.Path.cwd() / "integration/remote-worldgen/run-e2e.sh").read_bytes()).hexdigest()
(out / "evidence-summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
PY
printf 'E2E evidence: %s\n' "$OUT"
