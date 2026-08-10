# Steel world-generation worker

`steel-worldgen-service` is a fixed-profile, RAM-only gRPC worker for detached Minecraft 26.2 chunk generation. It owns no world files. For each accepted request it drives Steel through the exact `BIOMES -> NOISE` boundary and returns a canonical protobuf artifact for the center chunk.

## Quick start

```bash
STEEL_WORLDGEN_SEED=13579 cargo run -p steel-worldgen-service --release
# another shell
STEEL_WORLDGEN_ENDPOINT=http://127.0.0.1:50051 \
  cargo run -p steel-worldgen-service --release --bin steel-worldgen-probe
```

The listener defaults to loopback. A plaintext non-loopback bind is rejected unless `STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE=true` is explicitly set. Dedicated-host deployments should use direct mutual TLS or a private-network mTLS proxy.

## Configuration

| Variable | Default | Meaning |
|---|---:|---|
| `STEEL_WORLDGEN_SEED` | required | Exact signed 64-bit world seed |
| `STEEL_WORLDGEN_BIND` | `127.0.0.1:50051` | gRPC listen address |
| `STEEL_WORLDGEN_PROFILE_ID` | `default` | Operator profile name |
| `STEEL_WORLDGEN_GENERATOR` | `minecraft:overworld` | Built-in Steel generator |
| `STEEL_WORLDGEN_DIMENSION` | generator ID | Wire dimension key |
| `STEEL_WORLDGEN_THREADS` | `1` | Must remain `1` for deterministic output; scale with worker processes |
| `STEEL_WORLDGEN_MAX_IN_FLIGHT` | thread count | Global admitted physical generation jobs |
| `STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER` | global maximum | Physical generation jobs admitted per source IP; each connection reserves eight additional control-plane streams |
| `STEEL_WORLDGEN_REQUEST_TIMEOUT_MS` | `30000` | Server deadline, `1..=600000` |
| `STEEL_WORLDGEN_MAX_CACHE_ENTRIES` | `1024` | Complete artifact LRU-like FIFO bound; `0` disables |
| `STEEL_WORLDGEN_MAX_CACHE_BYTES` | `268435456` | Total encoded cache weight bound; `0` disables |
| `STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE` | `false` | Explicit exception for private plaintext non-loopback binds |
| `STEEL_WORLDGEN_SOURCE_URL` | project repository on loopback | Exact no-charge corresponding-source URL; mandatory for non-loopback binds and advertised by capabilities |
| `STEEL_WORLDGEN_TLS_CERT` | unset | PEM server certificate chain |
| `STEEL_WORLDGEN_TLS_KEY` | unset | PEM server key |
| `STEEL_WORLDGEN_TLS_CLIENT_CA` | unset | PEM roots required to authenticate clients |

All three TLS paths are required together. The diagnostic clients support server-authenticated TLS with `STEEL_WORLDGEN_CLIENT_CA` and `..._DOMAIN`; add `..._CERT` and `..._KEY` for mTLS. Generator fingerprints bind every compiled workspace crate source, generated Rust output, extracted build asset, dependency lockfile, compiler, target, build profile, and optional external `STEEL_WORLDGEN_BUILD_ID`; dirty local builds cannot share a production cache identity accidentally.

## RPCs

- `GetCapabilities`: immutable profile, version, limits, feature flags, and fingerprints.
- `Generate`: one bounded unary request and detached artifact.
- `Cancel`: idempotently suppresses publication for a matching active request. Work still waiting for the serialized scheduling epoch exits cooperatively. Steel stage methods are synchronous, so work already inside a stage may finish in a bounded detached task after cancellation or timeout.
- `GetMetrics`: process-lifetime request counters. Use an authenticated metrics gateway before exposing externally.
- `grpc.health.v1.Health/Check`: standard serving-status probe for the worker service.

The service rejects version/profile/fingerprint mismatches, stages other than `BIOMES -> NOISE`, unsupported generation contexts, invalid coordinates, malformed lengths, and compression it did not advertise. Blending and retrogen are not implemented.

## Diagnostics

Set `STEEL_WORLDGEN_HEALTH_ONLY=true` on `steel-worldgen-probe` for a lightweight standard gRPC health check, `STEEL_WORLDGEN_METRICS_ONLY=true` for counters, or `STEEL_WORLDGEN_CANCEL_TEST=true` for an active cancellation-and-drain check instead of a normal artifact probe.

For a load run, start a fresh worker with matching admission/cache capacity, then benchmark an unused coordinate grid. The benchmark now rejects prewarmed “cold” entries, warm misses, excess concurrency, failures, cancellations, and contaminated metrics.

```bash
# worker shell
STEEL_WORLDGEN_SEED=13579 STEEL_WORLDGEN_MAX_IN_FLIGHT=16 \
  cargo run -p steel-worldgen-service --release --bin steel-worldgen-service
# client shell: 100 cold + 100 warm requests, JSON to stdout
STEEL_WORLDGEN_BENCH_SIDE=10 STEEL_WORLDGEN_BENCH_CONCURRENCY=16 \
STEEL_WORLDGEN_BENCH_START_X=1000 STEEL_WORLDGEN_BENCH_START_Z=1000 \
  cargo run -p steel-worldgen-service --release --bin steel-worldgen-bench
```

`integration/remote-worldgen/run-mtls-smoke.sh` produces authenticated-health/generation and unauthenticated-rejection evidence. `run-e2e.sh` additionally runs cancellation/drain followed by a safe same-position request, two-process byte determinism, the Folia projection, and protocol-776 exploration.

See [`PROTOCOL.md`](PROTOCOL.md) and [`../integration/remote-worldgen/ARCHITECTURE.md`](../integration/remote-worldgen/ARCHITECTURE.md).

Direct mTLS authenticates clients but Tonic's stream limit is per TCP connection. Production ingress must additionally bound authenticated connections and control-plane request rates; Generate work itself remains globally and source-IP bounded in the worker.

## Image licensing inventory

The runtime image embeds `/LICENSE`, `/steelmc-corresponding-source.tar`, `/steelmc-cargo-metadata.json`, and `/steelmc-third-party-licenses.tar`; its OCI source URL and revision labels match the capability advertisement. The metadata and license archive inventory the resolved Rust dependency graph and redistribute dependency notice/license files.
