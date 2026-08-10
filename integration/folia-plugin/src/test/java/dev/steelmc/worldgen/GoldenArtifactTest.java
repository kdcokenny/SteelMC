package dev.steelmc.worldgen;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import dev.steelmc.worldgen.protocol.v1.BlockProperty;
import dev.steelmc.worldgen.protocol.v1.ChunkArtifactV1;
import dev.steelmc.worldgen.protocol.v1.Capabilities;
import dev.steelmc.worldgen.protocol.v1.ChunkSection;
import dev.steelmc.worldgen.protocol.v1.Compression;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.GenerationContext;
import dev.steelmc.worldgen.protocol.v1.HeightmapType;
import dev.steelmc.worldgen.protocol.v1.Stage;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.lang.reflect.Proxy;
import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import org.bukkit.HeightMap;
import org.bukkit.block.data.BlockData;
import org.bukkit.generator.ChunkGenerator.ChunkData;
import org.junit.jupiter.api.Test;

final class GoldenArtifactTest {
    private static final String RESOURCE = "/noise-v1-overworld-seed-13579-x-6-z2.pb";
    private static final String SHA256_RESOURCE = RESOURCE + ".sha256";

    @Test
    void rustArtifactDecodesAndSatisfiesCanonicalBounds() throws Exception {
        String override = System.getenv("STEEL_WORLDGEN_ARTIFACT_TEST_OVERRIDE");
        byte[] bytes;
        String expectedSha256;
        if (override == null || override.isBlank()) {
            InputStream input = GoldenArtifactTest.class.getResourceAsStream(RESOURCE);
            assertNotNull(input, "Rust golden artifact is missing");
            try (input) {
                bytes = input.readAllBytes();
            }
            InputStream expectedInput = GoldenArtifactTest.class.getResourceAsStream(SHA256_RESOURCE);
            assertNotNull(expectedInput, "Rust golden artifact SHA-256 is missing");
            try (expectedInput) {
                expectedSha256 = new String(expectedInput.readAllBytes(), StandardCharsets.US_ASCII).trim();
            }
        } else {
            bytes = Files.readAllBytes(Path.of(override));
            expectedSha256 = System.getenv("STEEL_WORLDGEN_ARTIFACT_TEST_SHA256");
            assertNotNull(expectedSha256, "current-tree artifact SHA-256 is missing");
        }
        assertEquals(
            expectedSha256,
            HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes))
        );

        ChunkArtifactV1 artifact = ChunkArtifactV1.parseFrom(bytes);
        assertEquals(1, artifact.getArtifactVersion());
        assertEquals("26.2", artifact.getMinecraftVersion());
        assertEquals("minecraft:overworld", artifact.getDimensionKey());
        assertEquals(13_579, artifact.getSeed());
        assertEquals(-6, artifact.getChunkX());
        assertEquals(2, artifact.getChunkZ());
        assertEquals(-64, artifact.getMinY());
        assertEquals(384, artifact.getHeight());
        assertEquals(24, artifact.getSectionsCount());
        assertEquals(24, artifact.getPostprocessingCount());
        assertEquals(
            List.of(HeightmapType.HEIGHTMAP_WORLD_SURFACE_WG, HeightmapType.HEIGHTMAP_OCEAN_FLOOR_WG),
            artifact.getHeightmapsList().stream().map(heightmap -> heightmap.getType()).sorted().toList()
        );
        assertTrue(
            artifact.getBiomesList().stream().sorted().toList().equals(artifact.getBiomesList()),
            "biome dictionary is not canonical"
        );
        assertTrue(
            artifact.getBlockStatesList().stream().map(state -> state.getName()).sorted().toList().equals(
                artifact.getBlockStatesList().stream().map(state -> state.getName()).toList()
            ),
            "block-state dictionary is not canonical"
        );
        for (int index = 0; index < artifact.getSectionsCount(); index++) {
            ChunkSection section = artifact.getSections(index);
            assertEquals(-4 + index, section.getSectionY());
            PaletteCodec.validate(section.getBlockStates(), 4096, artifact.getBlockStatesCount());
            PaletteCodec.validate(section.getBiomes(), 64, artifact.getBiomesCount());
        }
        artifact.getBlockStatesList().forEach(state -> {
            List<String> propertyNames = state.getPropertiesList().stream()
                .map(BlockProperty::getName)
                .toList();
            assertEquals(propertyNames.stream().sorted().toList(), propertyNames);
        });
        artifact.getHeightmapsList().forEach(heightmap ->
            assertEquals(256, heightmap.getFirstAvailableRelativeToMinYCount())
        );

        GenerateRequest request = GenerateRequest.newBuilder()
            .setRequestId(com.google.protobuf.ByteString.copyFrom(new byte[16]))
            .setMinecraftVersion(artifact.getMinecraftVersion())
            .setProfileId("golden-test")
            .setDimensionKey(artifact.getDimensionKey())
            .setSeed(artifact.getSeed())
            .setChunkX(artifact.getChunkX())
            .setChunkZ(artifact.getChunkZ())
            .setMinY(artifact.getMinY())
            .setHeight(artifact.getHeight())
            .setFirstStage(Stage.STAGE_BIOMES)
            .setLastStage(Stage.STAGE_NOISE)
            .setExpectedGeneratorSha256(artifact.getGeneratorSha256())
            .setExpectedRegistrySha256(artifact.getRegistrySha256())
            .addAcceptedCompression(Compression.COMPRESSION_NONE)
            .setGenerationContext(GenerationContext.GENERATION_CONTEXT_FRESH)
            .build();
        Capabilities capabilities = Capabilities.newBuilder()
            .setGeneratorSha256(artifact.getGeneratorSha256())
            .setRegistrySha256(artifact.getRegistrySha256())
            .build();
        ArtifactView view = ArtifactView.decode(
            artifact,
            request,
            capabilities,
            description -> proxy(BlockData.class, description),
            name -> new ArtifactView.ResolvedBiome(null)
        );
        AtomicInteger blocksWritten = new AtomicInteger();
        ChunkData output = (ChunkData) Proxy.newProxyInstance(
            GoldenArtifactTest.class.getClassLoader(),
            new Class<?>[] {ChunkData.class},
            (proxy, method, arguments) -> switch (method.getName()) {
                case "getMinHeight" -> artifact.getMinY();
                case "getMaxHeight" -> artifact.getMinY() + artifact.getHeight();
                case "setBlock" -> {
                    int x = (int) arguments[0];
                    int y = (int) arguments[1];
                    int z = (int) arguments[2];
                    assertTrue(x >= 0 && x < 16 && z >= 0 && z < 16);
                    assertTrue(y >= artifact.getMinY() && y < artifact.getMinY() + artifact.getHeight());
                    blocksWritten.incrementAndGet();
                    yield null;
                }
                case "getHeight" -> projectedHeight(artifact, (HeightMap) arguments[0], (int) arguments[1], (int) arguments[2]);
                default -> defaultValue(method.getReturnType());
            }
        );
        view.apply(output, true);
        assertEquals(artifact.getSectionsCount() * 4096, blocksWritten.get());
    }

    private static int projectedHeight(ChunkArtifactV1 artifact, HeightMap type, int x, int z) {
        HeightmapType wireType = switch (type) {
            case WORLD_SURFACE_WG -> HeightmapType.HEIGHTMAP_WORLD_SURFACE_WG;
            case OCEAN_FLOOR_WG -> HeightmapType.HEIGHTMAP_OCEAN_FLOOR_WG;
            default -> throw new AssertionError("unexpected heightmap " + type);
        };
        var heightmap = artifact.getHeightmapsList().stream()
            .filter(candidate -> candidate.getType() == wireType)
            .findFirst()
            .orElseThrow();
        return artifact.getMinY() + heightmap.getFirstAvailableRelativeToMinY(z * 16 + x) - 1;
    }

    @SuppressWarnings("unchecked")
    private static <T> T proxy(Class<T> type, String description) {
        return (T) Proxy.newProxyInstance(
            GoldenArtifactTest.class.getClassLoader(),
            new Class<?>[] {type},
            (proxy, method, arguments) -> switch (method.getName()) {
                case "getAsString", "toString" -> description;
                case "hashCode" -> System.identityHashCode(proxy);
                case "equals" -> proxy == arguments[0];
                default -> defaultValue(method.getReturnType());
            }
        );
    }

    private static Object defaultValue(Class<?> type) {
        if (!type.isPrimitive() || type == void.class) return null;
        if (type == boolean.class) return false;
        if (type == char.class) return '\0';
        if (type == byte.class) return (byte) 0;
        if (type == short.class) return (short) 0;
        if (type == int.class) return 0;
        if (type == long.class) return 0L;
        if (type == float.class) return 0F;
        if (type == double.class) return 0D;
        throw new AssertionError("unsupported primitive " + type);
    }
}
