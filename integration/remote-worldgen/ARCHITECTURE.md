# Remote world generation for Folia 26.2

## Status

This branch implements a version-pinned Steel worker, cross-language artifact, deliberately limited Bukkit projection prototype, protocol-776 client harness, and an experimental internal Moonrise/Folia importer for the explicit fresh vanilla-noise V1 profile. The maintained patch queue applies and compiles at the pinned Folia revision. Keep the importer disabled: a plugin-free runtime diagnostic found an underlying Steel/Folia BIOMES parity mismatch, described below. The Bukkit evidence is not proof of faithful native NOISE import, and V1 still lacks remote priority updates and explicit Folia structure/Beardifier context identity.

Exact source pins used by the checked-in evidence:

| Component | Pin |
|---|---|
| Steel base | `ddfaf4650` (`0.15.2+mc26.2`) |
| Minecraft / protocol | `26.2` / `776` |
| Folia | `57f643f10e0a9d01024773232d38ae666067d593` |
| Published Folia build-1 baseline | embedded revision `e48800d`, JAR SHA-256 `6726da42d6a4edc4961a43cdccfd7ebf5fea75e7b1342266532ed143df6736e7` |
| Paper upstream | `1f7285664c3a11690a641e19ffcc90321fcc7fde` |
| Azalea | `6249c295d353b9b3ef68f665b311cba39211fd19` |
| Rust toolchain | `nightly-2026-07-23` |

The `Cargo.toml` version suffix remains authoritative for Steel's target. Run `update-minecraft-src.sh` before changing vanilla-observable generation behavior; the implementation here adds orchestration and serialization around the existing generator and does not replace extracted vanilla data.

## Ownership boundary

Folia remains authoritative for tickets, priorities, the 19x19 dependency cache, holder identity, chunk lifecycle/status, cancellation policy, plugins, persistence, entity/POI state, and `.mca` files. A worker owns a fixed startup profile and returns one detached center-chunk mutation. It never mounts or writes a Folia world.

The first boundary is fresh `NOISE`:

- prerequisite context: structure starts/references and BIOMES under exact fingerprints;
- mutations: block states, `WORLD_SURFACE_WG`, `OCEAN_FLOOR_WG`, and aquifer post-processing offsets;
- excluded: Surface, Carvers, Features, lighting, entities, POI, persistence, blending, and retrogen.

Steel's ticket levels can permit several stages with the same dependency radius. A new optional `WorldConfig.generation_status_ceiling` therefore clamps the worker's headless world to exact NOISE. The encoder also asserts the holder status and recomputes both world-generation heightmaps from encoded sections. These checks prevent a timing-dependent Surface/Carvers snapshot from being mislabeled as NOISE. Live Steel worlds set no ceiling.

## Why the Bukkit plugin is only a prototype

The plugin under `integration/folia-plugin` configures a synchronous `ChunkGenerator.generateNoise` projection. It proved Java gRPC discovery, cross-language decoding, named state translation, block import, exact world-generation heightmaps, startup, joins, teleports, and continued chunk delivery. It intentionally requires an opt-in when an artifact contains aquifer post-processing offsets because `ChunkData` cannot import those offsets. It also lacks Blender, BelowZeroRetrogen, Beardifier/StructureManager transfer, Moonrise priority updates, prompt cancellation, and authoritative status error policy.

Do not configure its remote `BiomeProvider` for the prototype world. Folia's structure biome searches call the provider outside a center NOISE request and would turn biome lookups into unnecessary NOISE artifacts. The tested prototype keeps Folia's native 26.2 biome provider and uses Steel only for the block projection. Exact fingerprints and seed make divergence detectable, but this is still not the production context contract.

## Native importer runtime blocker

A plugin-free diagnostic used a locally built patched Folia JAR at source pin `57f643f10e0a9d01024773232d38ae666067d593`, a fresh seed-`13579` world, and the pinned protocol-776 Azalea client. Startup and the first 25 remote NOISE requests succeeded. After teleporting to `(512, 128, 0)`, the importer correctly failed closed at chunks `(31,2)`/`(32,2)` because Steel's artifact BIOMES did not match Folia's already-completed BIOMES stage. One observed quart cell was Steel `minecraft:stony_shore` versus Folia `minecraft:lush_caves`; another was the reverse. Folia's sampled climate target also differed from Steel's at the quantized depth/continentalness boundary.

This is a generator-parity foundation blocker, not permission to ignore the precondition or overwrite Folia's BIOMES. The internal importer must remain disabled until the architecture chooses and proves either bit-compatible Steel 26.2 climate/density generation or an explicit canonical Folia BIOMES-context transfer. Structure/Beardifier context and remote priority transfer remain separate V1 gaps. No plugin-free persisted native-importer success is claimed.

## Native Folia overlay requirements

The active 26.2 scheduler is Moonrise, not obsolete vanilla `ChunkMap` methods. The hook belongs at:

```text
folia-server/src/minecraft/java/net/minecraft/world/level/chunk/status/ChunkStatusTasks.java
generateNoise(...)
```

The fork must:

1. Reserve and construct dependencies through `ChunkTaskScheduler` exactly as today.
2. Build a request only for a fresh center chunk under a startup-pinned worker profile.
3. Return a non-blocking future; `ChunkUpgradeGenericStatusTask` currently calls `.join()` on an incomplete status future and must complete progression from a continuation. Cancellation of an already-running status must retain chunk protections until the physical source future drains; cancelling only a dependent `CompletableFuture` is not upstream cancellation.
4. Verify response identity, request key, fingerprints, byte length/hash, semantic bounds, registry names/properties, and complete context before mutation.
5. Apply blocks, both WG heightmaps, and post-processing offsets directly to the center `ProtoChunk`.
6. Publish NOISE only after the complete validated apply. No partial mutation may escape.
7. Propagate Moonrise cancellation and priority changes to the client. The worker cooperatively drops queued requests, but Steel currently cannot interrupt synchronous stage code, so cancellation may suppress publication while already-started bounded physical work drains.
8. Define an explicit retry/fail-local/fail-world policy. Current Moonrise sends non-cancellation generation exceptions to `unrecoverableChunkSystemFailure`; silently substituting local generation after partial remote acceptance is unsafe.
9. Reject blending and retrogen until their complete input and mutation semantics are implemented.

The complete experimental importer overlay is provided in `integration/folia-fork/`. Its scheduler and configuration patches plus ordinary Java importer sources apply cleanly to the pinned head, and `:folia-server:compileJava` passes under Java 25. Compilation proves API compatibility, not runtime world-generation parity.

The old `ChunkMap.scheduleChunkLoad`, `GenerationChunkHolder.applyStep`, and similar vanilla paths throw `UnsupportedOperationException` in current Folia and are not integration points. Velocity has no chunk-generation lifecycle and is also not an integration point.

## Deployment topology

Run several immutable worker replicas on dedicated CPU/memory hosts. Route by the canonical request hash with bounded retries only before publication. A replica owns one seed/generator/dimension profile for its lifetime. Arbitrary per-request profiles require a bounded world cache plus explicit shutdown; they are intentionally absent.

The worker serializes headless scheduler epochs per world because epoch publication is a global boundary. Multi-thread restart tests produced different artifacts, so the supported deterministic configuration requires `STEEL_WORLDGEN_THREADS=1`; scale with immutable worker processes/replicas, not threads inside one world. `MAX_IN_FLIGHT` bounds real work even after the caller times out: the physical permit is held by the detached task until Steel finishes.

Worker generator fingerprints include a canonical source-tree digest, dependency lockfile, compiler/target/build-profile identity, and the deployment build ID. Use the pinned multi-stage Dockerfile and internal Compose example. The image runs non-root, read-only, with all Linux capabilities dropped. Its base image and Rust builder are digest-pinned, and Docker builds require an immutable `STEEL_WORLDGEN_BUILD_ID`. The sample Compose network is internal and explicitly opts into plaintext; do not publish that port.

For an external network, either:

- mount a server certificate, key, and client CA and enable direct worker mTLS; or
- keep the worker on loopback/private networking and terminate mutually authenticated TLS in a narrowly configured proxy.

A plaintext non-loopback bind fails startup unless the operator explicitly sets `STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE=true`. mTLS authenticates clients; certificate issuance/rotation and authorization policy remain deployment responsibilities. Do not expose `GetMetrics` unauthenticated.

The Kubernetes example is a versioned, immutable profile stack. Its seed Secret is immutable, selectors include a profile-version label, and rollouts use `Recreate`; deploy a new versioned Deployment/Service and coordinate Folia endpoint/profile-pin changes rather than mixing old and new fingerprints behind one Service. The mounted mTLS Secret must contain `server.crt`, `server.key`, `client-ca.crt`, `server-ca.crt`, `probe-client.crt`, and `probe-client.key`; the server certificate must cover the configured probe domain. Startup, readiness, and liveness execute `/steel-worldgen-probe` as a standard authenticated gRPC health probe. A fatal generation failure first marks health `NOT_SERVING`, drains admitted RPCs through graceful server shutdown, and exits so the workload manager restarts a clean RAM world.

## Failure and cache semantics

- Cache key: canonical semantic request hash, never request ID/deadline/auth/compression.
- Cache value: exact uncompressed artifact bytes plus SHA-256.
- Duplicate active request IDs are rejected.
- Physical Generate work is bounded globally and by source IP; per-connection HTTP/2 streams are bounded with eight control-plane slots beyond that peer's Generate ceiling. Tonic does not globally cap authenticated TCP connections, so production ingress must also enforce connection/rate limits. Generate overflow is `RESOURCE_EXHAUSTED`.
- Cancellation requires both request ID and canonical hash. Unknown/already-finished cancellation is an idempotent `found=false`.
- Timeout/cancellation suppresses the RPC result. Queued worker work is cancelled cooperatively; an already-started synchronous Steel stage may drain in a detached, still-bounded task.
- Any non-cancellation Steel generation error quarantines the worker, marks gRPC health `NOT_SERVING`, and rejects later generation until process restart. A possibly partially mutated holder is never retried in place.
- A worker restart loses RAM cache and chunk context by design. Artifacts remain deterministic under the exact profile; the cancellation test retries only after physical cancellation drains without a generation-stage failure.

## Validation and evidence

Commands run for this branch include:

```bash
cargo check --workspace
cargo test -p steel-core
cargo test -p steel-worldgen-service
cargo clippy -r --all-targets --all-features
cargo fmt --all --check
(cd integration/folia-plugin && ./gradlew test shadowJar --no-daemon)
(cd integration/client-bot && cargo check --locked)
```

The executable projection E2E harness is `integration/remote-worldgen/run-e2e.sh`. It requires the exact published Folia 26.2 build-1 JAR pinned by SHA-256 and explicit EULA acceptance. It creates a cold world, starts the worker and Bukkit plugin, joins with pinned Azalea, waits through keepalives, teleports across four 512-block-separated views, verifies chunk receipt in every phase, and rejects Moonrise chunk-system errors. It does not load the internal importer.

Historical, superseded loopback observations are archived in `integration/remote-worldgen/results/`; they predate the current internal importer and final source identity:

- real client: spawn in 5.113 s, 1,000 ticks, 49 keepalives, 557 chunk events, and 117/117/117/117/89 unique target-proximate chunks across the five phases;
- E2E worker: 1,256 successful RPCs during the measured client-exploration window and zero worker failures/cancellations;
- generator/RPC load: 100 cold chunks at 55.63 chunks/s and 100 warm cache hits at 1,618.66 chunks/s, concurrency 16, loopback;
- the E2E harness compares exact `(-6,2)` artifact bytes from two clean one-thread worker processes; the checked hash and build identity live in its generated evidence summary;
- unsupported in-process generation thread counts are rejected at startup; deterministic scale-out uses independent one-thread processes;
- direct mTLS accepted a CA-authenticated client and rejected a client without a certificate.

These are development-host loopback measurements on an i7-1260P, not WAN or dedicated-host capacity claims. The cold figure includes Steel dependency generation, canonical encoding, gRPC, decoding, and validation. Warm throughput is cache/RPC throughput. The client result includes Folia Surface through delivery and must not be called pure worldgen performance. Benchmark dedicated replicas with production RTT, TLS, CPU quotas, cache policy, and Folia MSPT before sizing.

## Licensing

Steel is AGPL-3.0-or-later. Folia/Paper and Minecraft server artifacts have their own licenses and distribution terms. The E2E harness takes a local `FOLIA_JAR`; it does not redistribute Minecraft or Folia binaries. Review the obligations of a network-deployed modified Steel service and any Folia fork before deployment.

The worker enforces both global physical-work admission and a source-IP admission ceiling (`STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER`); cancellation retains both permits until detached physical work drains.
