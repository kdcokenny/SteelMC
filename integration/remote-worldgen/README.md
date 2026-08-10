# Reproducible remote-worldgen integration evidence

- `compose.yml`: pinned, non-root, read-only worker on an internal Docker network. Set an immutable `STEEL_WORLDGEN_BUILD_ID` and exact seed.
- `kubernetes.yaml`: four-replica, one-generation-thread-per-process mTLS/resource/security/network-policy template; replace the image digest and provide the named secrets. Kubernetes `Service` routing is not content-aware; place a canonical-hash-aware authenticated gateway in front if cross-replica cache locality matters.
- `run-e2e.sh`: cold local Folia/plugin/worker/Azalea test plus cancellation/retry and two-process determinism proof.
- `run-mtls-smoke.sh`: ephemeral-CA direct-mTLS health/generation acceptance and missing-client-certificate rejection proof.
- `run-cross-language-test.sh`: generates with the current Rust tree and makes Java decode, validate, and apply those exact bytes; it requires only immutable build/source metadata.
- `run-e2e.sh` additionally requires the pinned Folia 26.2 build-1 paperclip and `ACCEPT_MINECRAFT_EULA=true`.
- `regenerate-golden.sh`: guarded current-Rust fixture and build-manifest generator.
- `results/`: superseded historical loopback observations, not current-tree or native-importer proof and not dedicated-host capacity promises.

Supply the exact published Folia 26.2 build-1 Paperclip (embedded revision `e48800d`) under Java 25. The internal importer is maintained against the separate source pin `57f643f10e0a9d01024773232d38ae666067d593`. The projection harness intentionally pins the published bytes rather than a locally rebuilt, timestamp-varying Paperclip archive:

```bash
FOLIA_JAR=/path/to/folia-26.2-1.jar
FOLIA_JAR_SHA256=6726da42d6a4edc4961a43cdccfd7ebf5fea75e7b1342266532ed143df6736e7
revision=$(git rev-parse HEAD)
FOLIA_JAR="$FOLIA_JAR" \
FOLIA_JAR_SHA256="$FOLIA_JAR_SHA256" \
STEEL_WORLDGEN_BUILD_ID="$revision" \
STEEL_WORLDGEN_SOURCE_URL="https://github.com/Steel-Foundation/SteelMC/tree/$revision" \
ACCEPT_MINECRAFT_EULA=true \
./integration/remote-worldgen/run-e2e.sh
```

Apply and compile the experimental internal Folia importer separately using [`../folia-fork/README.md`](../folia-fork/README.md); the Bukkit projection E2E uses the published Folia JAR and is not native-importer evidence. A plugin-free diagnostic currently fails closed on a Steel/Folia BIOMES parity mismatch, so keep the importer disabled until that documented foundation is resolved.

The harness refuses a jar-byte mismatch, requires Java 25, runs Gradle and client-bot tests, proves cancellation/drain plus same-position retry, compares artifacts from two clean worker processes, and writes a hash-linked `evidence-summary.json`.

The script writes logs and JSON beneath `artifacts/remote-worldgen-e2e/`. It deliberately enables the Bukkit post-processing divergence and must not be used as a production deployment recipe.

The worker enforces both global physical-work admission and a source-IP admission ceiling (`STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER`); cancellation retains both permits until detached physical work drains.

Before applying the Kubernetes template, replace every `REPLACE_*` value. `STEEL_WORLDGEN_SOURCE_URL` must resolve to the exact modified source identified by the worker's advertised source digest; a moving branch URL is not sufficient for a distributed service.
