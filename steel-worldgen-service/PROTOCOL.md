# Steel detached world-generation protocol V1

The source of truth is [`proto/steel/worldgen/v1/worldgen.proto`](proto/steel/worldgen/v1/worldgen.proto). Protobuf supplies framing and forward-compatible fields; the rules below define semantic canonicalization. All artifact hashes cover the exact uncompressed protobuf bytes returned in `GenerateResponse.artifact`.

## Compatibility and request key

V1 is pinned to Minecraft `26.2`, Steel `0.15.2+mc26.2`, stage interval `BIOMES -> NOISE`, one fixed worker profile, and fresh generation. Clients first compare the capability fingerprints. A mismatch is not a cache miss: it is `FAILED_PRECONDITION`.

The canonical request preimage is concatenated without protobuf serialization:

1. ASCII `SWGREQ1\0` (8 bytes).
2. Minecraft version as big-endian `u16` length plus UTF-8 bytes.
3. Dimension key in the same form.
4. Seed `i64`, chunk X `i32`, chunk Z `i32`, min Y `i32`, and height `u32`, all big-endian.
5. First and last stage as one byte each.
6. Exactly 32 generator-fingerprint bytes and 32 registry-fingerprint bytes.
7. Big-endian `u16` zero: the V1 generation-context extension length.

`request_id`, profile display name, deadlines, tracing, authentication, accepted transport compression, and protobuf unknown fields are excluded. SHA-256 of the preimage is the content/cache key.

Published vector:

```text
preimage = 5357475245513100000432362e3200136d696e6563726166743a6f766572776f726c64000000000000350b0000000000000000ffffffc00000018003040000000000000000000000000000000000000000000000000000000000000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff0000
sha256   = d63f74fb044c0c93fbd48b1fdca3a4ef20c81d6b6e51b4b78d5cc0462c2c1c68
```

## Artifact semantics

A V1 NOISE artifact contains only the center chunk:

- canonical named block states (`minecraft:name` plus properties sorted by name);
- canonical biome identifiers;
- 24 sections for the overworld profile, ordered by increasing section Y;
- block cells in Y-Z-X order (`index = y*256 + z*16 + x`);
- biome quart cells in Y-Z-X order (`index = y*16 + z*4 + x`);
- the two stage-owned heightmaps `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG`, x-fast and Z-major, storing first-available Y relative to min Y;
- per-section aquifer post-processing offsets, using vanilla's 12-bit local position encoding.

Final heightmaps do not exist at NOISE and must not be fabricated. BIOMES is a required input/status and is included in the artifact so a consumer can verify the exact context; it is not claimed as a NOISE mutation.

Dictionaries are structurally sorted by namespaced key and then `(property name, value)` tuples. Property names and values use Minecraft identifier-path characters so consumers never interpolate unescaped grammar. Dictionaries and local palettes are minimal: every entry must be referenced. Every section uses a sorted local palette of global dictionary indexes. A homogeneous palette has `bits_per_entry=0` and empty data. Otherwise width is `ceil(log2(local_palette_length))`. Values are densely packed LSB-first across byte boundaries.

Published bit vector:

```text
values [0,1,2,3,4,5,6,7,0], width 3 -> 88 c6 fa 00
```

## Validation order and bounds

1. Enforce the gRPC message bound.
2. Enforce declared uncompressed size and the 8 MiB artifact bound.
3. Hash exact artifact bytes and compare SHA-256.
4. Decode protobuf.
5. Validate version, fingerprints, request key, dimension/seed/coordinates/range/stages.
6. Validate canonical dictionaries, section continuity, exact cell counts, palette widths/indexes/data lengths, exactly two NOISE heightmaps, height ranges, and post-processing offsets.
7. Translate named states/biomes against the consumer registry.
8. Apply all mutations to a detached center chunk, then publish its status.

The registry fingerprint hashes canonical named block states and biome identifiers in structural lexical order; registry numeric ordering is not part of the digest. Unknown protobuf fields may be ignored only after V1 negotiation. Unknown enum values, registry names, properties, or property values are fatal. No numeric Steel or Minecraft registry ID is a cross-language contract.

## Unsupported contexts

V1 capabilities advertise `supports_blending=false`, `supports_retrogen=false`, and `steel_resumable=false`. A client must not silently fall back after the worker accepted an authoritative remote request; policy for retry or local generation belongs to Folia before status publication.
