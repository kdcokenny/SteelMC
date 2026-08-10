# Folia 26.2 Bukkit projection prototype

This plugin is a test adapter for the Steel V1 artifact. It is **not** the faithful Moonrise production integration. It synchronously calls the worker from `ChunkGenerator.generateNoise`, imports named block states, verifies both NOISE heightmaps against the projected blocks, and lets Folia continue with native Surface, Carvers, Features, and later stages.

## Build

Java 25 is required; the Gradle wrapper and its distribution checksum are committed.

```bash
./gradlew test shadowJar --no-daemon
```

The API dependency is pinned to `dev.folia:folia-api:26.2.build.1-beta`; the live evidence used a local jar built from Folia `57f643f10e0a9d01024773232d38ae666067d593`. The shaded jar deliberately does not relocate gRPC/protobuf. Shadow merges service descriptors with duplicate inclusion so Netty and DNS providers remain discoverable in Folia's plugin classloader.

## Configure

Copy the shadow jar to `plugins/`, then configure only the generator:

```yaml
# bukkit.yml
worlds:
  world:
    generator: SteelRemoteWorldGen:overworld
```

Do not configure `biome-provider` for the tested prototype. See the architecture document for why remote NOISE is the wrong unit for structure biome searches.

```yaml
# plugins/SteelRemoteWorldGen/config.yml
worker:
  endpoint: "http://127.0.0.1:50051"
  plaintext: true
  # For direct mTLS set plaintext=false and configure:
  # tls:
  #   ca-certificate: "/path/server-ca.crt"
  #   client-certificate: "/path/client.crt"
  #   client-key: "/path/client.key"
  #   domain: "steel-worldgen-26-2-v1"
  deadline-millis: 120000
  expected-minecraft-version: "26.2"
  expected-profile-sha256: "<64 lowercase hex characters from GetCapabilities>"
  allow-unpinned-profile: false
cache:
  max-chunks: 4096
  max-bytes: 268435456
prototype:
  allow-unapplied-postprocessing: true
```

The cache enforces both completed-entry and encoded-byte limits; in-flight duplicate requests share one future but are not retained past those limits.

This prototype deliberately leaves Bukkit `getBaseHeight` unimplemented. Folia therefore delegates base-height queries to its native generator, which can diverge from remotely projected blocks; the production NOISE importer must publish Steel's validated WG heightmaps into the center `ProtoChunk` instead.

The post-processing opt-in is mandatory when the worker returns aquifer offsets because Bukkit `ChunkData` has no import API. Enabling it logs a severe divergence warning. Keep it false outside an explicit prototype. With `plaintext=false`, the plugin requires a private CA, client certificate/key, and certificate domain and configures direct mutual TLS through its shaded Netty transport. Keep certificate files readable only by the Folia service account.

## Validation

The plugin validates capability pins, response identity, request and artifact SHA-256, canonical named dictionaries, all palette bounds, section order, both WG heightmaps, and post-processing encodings before applying blocks. The golden test parses the Rust-produced fixture in `steel-worldgen-service/test_assets/`; its byte hash and exact embedded build identity are recorded by `golden-manifest.json`. Regenerate it only through `integration/remote-worldgen/regenerate-golden.sh`.

See [`../remote-worldgen/ARCHITECTURE.md`](../remote-worldgen/ARCHITECTURE.md).
