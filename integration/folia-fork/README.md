# Maintained Folia 26.2 Steel NOISE importer

This tree contains the maintained Paperweight patch/source form for Folia
`57f643f10e0a9d01024773232d38ae666067d593` (Paper
`1f7285664c3a11690a641e19ffcc90321fcc7fde`). Java 25 is required.

```bash
git clone https://github.com/PaperMC/Folia.git /tmp/folia-26.2
git -C /tmp/folia-26.2 checkout 57f643f10e0a9d01024773232d38ae666067d593
cp -a integration/folia-fork/folia-server/. /tmp/folia-26.2/folia-server/
cd /tmp/folia-26.2
./gradlew applyAllPatches --no-daemon --no-configuration-cache
./gradlew :folia-server:compileJava --no-daemon --no-configuration-cache
./gradlew :folia-server:test --tests org.bukkit.support.suite.NormalTestSuite --no-daemon --no-configuration-cache
./gradlew :folia-server:test --tests org.bukkit.support.suite.VanillaFeatureTestSuite --no-daemon --no-configuration-cache
```

The fork owns the native hook. The Bukkit plugin under `../folia-plugin`
is only a projection/E2E prototype and is not loaded by this importer.

## Runtime evidence

A plugin-free fresh-world diagnostic initially found a fail-closed BIOMES mismatch
at chunks `(31,2)`/`(32,2)`. The root cause was Steel using double multiplication
where Folia 26.2's compiled random sources use float multiplication before widening
`nextDouble`. After correcting all three matching Steel RNG paths, the native harness
completed 1,260 exploration-window imports with no worker failures, delivered all
five protocol-776 client phases, shut down cleanly, and rejoined the persisted world
with this importer disabled and no additional worker calls.

Run `../remote-worldgen/run-native-folia-e2e.sh` against a Paperclip built from this
checkout to reproduce the matrix. This evidence covers the exercised fresh profile,
not arbitrary structure context. Production use remains blocked on explicit
structure/Beardifier identity and Moonrise priority-update transfer.

## Scope and failure policy

The selected world's internal Moonrise `generateNoise` status performs an
asynchronous gRPC request, prepares all sections/maps/post-processing away
from the protected center, and commits only after complete validation. Other
worlds retain native Folia generation. A selected-world mismatch, unsupported
context, deadline, transport failure, or invalid artifact fails the status;
there is no retry or native fallback.

This is deliberately V1 **fresh BIOMES -> NOISE only**: no blending, retrogen,
`ImposterProtoChunk`, custom generator, flat/debug profile, or Steel-resumable
artifact. The configured exact `NoiseBasedChunkGenerator` settings key, world
seed/range/dimension, worker profile SHA, and the full local block-state/biome
registry SHA are pinned. `/paper reload` does not replace the client.

The current V1 protobuf has no priority field/update RPC and no Folia
structure/Beardifier context identity. Consequently this importer is limited
to the explicit fresh vanilla noise-generator profile. It does not claim the
architecture's priority transfer or arbitrary structure-context support. Those
require a protobuf and Rust dispatcher extension.

## Configuration

`paper-global.yml` is restart-only. Enabling requires a 64-character lowercase
profile SHA and mutual TLS unless plaintext is explicitly selected:

```yaml
steel-remote-worldgen:
  enabled: false
  target-world: world
  expected-noise-settings: minecraft:overworld
  endpoint: https://steel-worldgen.internal:50051
  deadline-millis: 120000
  max-in-flight: 1
  max-queued: 4096
  expected-minecraft-version: "26.2"
  expected-profile-sha256: "<64 lowercase hex>"
  plaintext: false
  tls:
    ca-certificate: /run/secrets/server-ca.crt
    client-certificate: /run/secrets/folia-client.crt
    client-key: /run/secrets/folia-client.key
    domain: steel-worldgen-26-2-v1
```

The vendored schema is canonical-source checked in CI with:

```bash
cmp steel-worldgen-service/proto/steel/worldgen/v1/worldgen.proto \
  integration/folia-fork/folia-server/src/main/proto/steel/worldgen/v1/worldgen.proto
```

## Patch queue

* `0011-Allow-async-chunk-status-futures.patch` removes the immediate `.join` and
  obsolete incomplete-future warning, retains Moonrise protections until physical
  completion, and publishes status only after uncancelled success. Cancellation is
  linearized against the importer commit so either cancellation suppresses mutation
  and publication or a winning commit publishes normally.
* `0012-Use-Steel-remote-NOISE.patch` installs the internal hook, bootstrap,
  close, and cooperative cancellation forwarding.
* Paper patch `0008-Configure-Steel-remote-worldgen.patch` adds startup-only
  global configuration.
* `build.gradle.kts.patch` pins protobuf/gRPC generation and runtime libraries.
* `src/main/java/io/papermc/paper/worldgen/steel/` contains ordinary fork
  sources; generated protobuf Java is never checked in or modified.
* Maintained native tests cover commit/cancellation linearization, response-envelope
  limits and identity, detached real-`ProtoChunk` preparation, late malformed-data
  rejection, atomic assignment, and commit-precondition failure.

## Upstream provenance and licensing

This patch modifies Folia/Paper code from the pins above. Folia is at
<https://github.com/PaperMC/Folia>; Paper's inherited server code is GPL-3.0
with contributor-specific MIT grants documented by upstream. SteelMC additions
are distributed under AGPL-3.0-or-later. Distributors must preserve notices
and provide corresponding source as required by those licenses.
