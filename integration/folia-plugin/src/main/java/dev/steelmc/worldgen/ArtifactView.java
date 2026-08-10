package dev.steelmc.worldgen;

import dev.steelmc.worldgen.protocol.v1.BlockProperty;
import dev.steelmc.worldgen.protocol.v1.BlockState;
import dev.steelmc.worldgen.protocol.v1.Capabilities;
import dev.steelmc.worldgen.protocol.v1.ChunkArtifactV1;
import dev.steelmc.worldgen.protocol.v1.ChunkSection;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.PackedPalette;
import dev.steelmc.worldgen.protocol.v1.PostProcessingSection;
import dev.steelmc.worldgen.protocol.v1.Stage;
import io.papermc.paper.registry.RegistryAccess;
import io.papermc.paper.registry.RegistryKey;
import java.util.BitSet;
import java.util.EnumMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.function.Function;
import org.bukkit.Bukkit;
import org.bukkit.HeightMap;
import org.bukkit.NamespacedKey;
import org.bukkit.Registry;
import org.bukkit.block.Biome;
import org.bukkit.block.data.BlockData;
import org.bukkit.generator.ChunkGenerator.ChunkData;

final class ArtifactView {
    private static final int BLOCK_VOLUME = 16 * 16 * 16;
    private static final int BIOME_VOLUME = 4 * 4 * 4;

    private final ChunkArtifactV1 artifact;
    private final BlockData[] blockStates;
    private final ResolvedBiome[] biomes;
    private final EnumMap<HeightMap, int[]> heightmaps;
    private final int postprocessingPositions;

    private ArtifactView(
        ChunkArtifactV1 artifact,
        BlockData[] blockStates,
        ResolvedBiome[] biomes,
        EnumMap<HeightMap, int[]> heightmaps,
        int postprocessingPositions
    ) {
        this.artifact = artifact;
        this.blockStates = blockStates;
        this.biomes = biomes;
        this.heightmaps = heightmaps;
        this.postprocessingPositions = postprocessingPositions;
    }

    static ArtifactView decode(
        ChunkArtifactV1 artifact,
        GenerateRequest request,
        Capabilities capabilities
    ) {
        Registry<Biome> biomeRegistry = RegistryAccess.registryAccess().getRegistry(RegistryKey.BIOME);
        return decode(
            artifact,
            request,
            capabilities,
            Bukkit::createBlockData,
            name -> {
                Biome biome = biomeRegistry.get(NamespacedKey.fromString(name));
                return biome == null ? null : new ResolvedBiome(biome);
            }
        );
    }

    static ArtifactView decode(
        ChunkArtifactV1 artifact,
        GenerateRequest request,
        Capabilities capabilities,
        Function<String, BlockData> blockStateResolver,
        Function<String, ResolvedBiome> biomeResolver
    ) {
        RemoteClient.require(artifact.getArtifactVersion() == 1, "unsupported artifact version");
        RemoteClient.require(
            artifact.getMinecraftVersion().equals(request.getMinecraftVersion()),
            "artifact Minecraft version mismatch"
        );
        RemoteClient.require(
            artifact.getCanonicalRequestSha256().equals(
                com.google.protobuf.ByteString.copyFrom(RemoteClient.canonicalRequestSha256(request))
            ),
            "artifact canonical request digest mismatch"
        );
        RemoteClient.require(
            artifact.getGeneratorSha256().equals(capabilities.getGeneratorSha256()),
            "artifact generator fingerprint mismatch"
        );
        RemoteClient.require(
            artifact.getRegistrySha256().equals(capabilities.getRegistrySha256()),
            "artifact registry fingerprint mismatch"
        );
        RemoteClient.require(artifact.getDimensionKey().equals(request.getDimensionKey()), "artifact dimension mismatch");
        RemoteClient.require(artifact.getSeed() == request.getSeed(), "artifact seed mismatch");
        RemoteClient.require(artifact.getChunkX() == request.getChunkX(), "artifact chunk X mismatch");
        RemoteClient.require(artifact.getChunkZ() == request.getChunkZ(), "artifact chunk Z mismatch");
        RemoteClient.require(artifact.getMinY() == request.getMinY(), "artifact minimum Y mismatch");
        RemoteClient.require(artifact.getHeight() == request.getHeight(), "artifact height mismatch");
        RemoteClient.require(
            artifact.getCompletedStagesList().equals(List.of(Stage.STAGE_BIOMES, Stage.STAGE_NOISE)),
            "NOISE artifact must contain exactly BIOMES then NOISE"
        );
        RemoteClient.require(artifact.getHeight() > 0 && artifact.getHeight() <= 4096, "invalid artifact height");
        RemoteClient.require(artifact.getHeight() % 16 == 0 && artifact.getMinY() % 16 == 0, "unaligned artifact range");

        BlockData[] blockStates = decodeBlockStates(artifact.getBlockStatesList(), blockStateResolver);
        ResolvedBiome[] biomes = decodeBiomes(artifact.getBiomesList(), biomeResolver);
        int sectionCount = Math.toIntExact(artifact.getHeight() / 16);
        RemoteClient.require(artifact.getSectionsCount() == sectionCount, "artifact section count mismatch");
        BitSet usedBlockStates = new BitSet(blockStates.length);
        BitSet usedBiomes = new BitSet(biomes.length);
        for (int index = 0; index < sectionCount; index++) {
            ChunkSection section = artifact.getSections(index);
            int expectedY = Math.floorDiv(artifact.getMinY(), 16) + index;
            RemoteClient.require(section.getSectionY() == expectedY, "artifact sections are not contiguous");
            usedBlockStates.or(PaletteCodec.validate(section.getBlockStates(), BLOCK_VOLUME, blockStates.length));
            usedBiomes.or(PaletteCodec.validate(section.getBiomes(), BIOME_VOLUME, biomes.length));
        }
        RemoteClient.require(
            usedBlockStates.cardinality() == blockStates.length,
            "block-state dictionary contains an unused entry"
        );
        RemoteClient.require(
            usedBiomes.cardinality() == biomes.length,
            "biome dictionary contains an unused entry"
        );

        EnumMap<HeightMap, int[]> heightmaps = decodeHeightmaps(artifact.getHeightmapsList(), artifact.getHeight());
        RemoteClient.require(
            heightmaps.size() == 2
                && heightmaps.containsKey(HeightMap.WORLD_SURFACE_WG)
                && heightmaps.containsKey(HeightMap.OCEAN_FLOOR_WG),
            "NOISE artifact must contain exactly the two world-generation heightmaps"
        );
        int postprocessingPositions = validatePostprocessing(artifact, sectionCount);
        return new ArtifactView(
            artifact,
            blockStates,
            biomes,
            heightmaps,
            postprocessingPositions
        );
    }

    void apply(ChunkData output, boolean allowUnappliedPostprocessing) {
        RemoteClient.require(output.getMinHeight() == this.artifact.getMinY(), "ChunkData minimum Y mismatch");
        RemoteClient.require(
            output.getMaxHeight() - output.getMinHeight() == this.artifact.getHeight(),
            "ChunkData height mismatch"
        );
        RemoteClient.require(
            allowUnappliedPostprocessing || this.postprocessingPositions == 0,
            "artifact contains aquifer post-processing positions that Bukkit ChunkData cannot import"
        );

        for (ChunkSection section : this.artifact.getSectionsList()) {
            PackedPalette palette = section.getBlockStates();
            int sectionMinY = section.getSectionY() * 16;
            for (int index = 0; index < BLOCK_VOLUME; index++) {
                int local = PaletteCodec.unpack(palette, index);
                int global = palette.getEntries(local);
                int x = index & 15;
                int z = (index >>> 4) & 15;
                int y = index >>> 8;
                output.setBlock(x, sectionMinY + y, z, this.blockStates[global]);
            }
        }

        for (HeightMap type : List.of(HeightMap.WORLD_SURFACE_WG, HeightMap.OCEAN_FLOOR_WG)) {
            int[] expected = this.heightmaps.get(type);
            RemoteClient.require(expected != null, "artifact is missing heightmap " + type);
            for (int z = 0; z < 16; z++) {
                for (int x = 0; x < 16; x++) {
                    int projectedFirstAvailable = output.getHeight(type, x, z) + 1;
                    int firstAvailable = this.artifact.getMinY() + expected[z * 16 + x];
                    RemoteClient.require(
                        projectedFirstAvailable == firstAvailable,
                        "ChunkData heightmap projection mismatch for "
                            + type
                            + " at ("
                            + x
                            + ","
                            + z
                            + "): artifact="
                            + firstAvailable
                            + ", projected="
                            + projectedFirstAvailable
                    );
                }
            }
        }
    }

    Biome biomeAt(int blockX, int blockY, int blockZ) {
        int clampedY = Math.max(
            this.artifact.getMinY(),
            Math.min(blockY, this.artifact.getMinY() + Math.toIntExact(this.artifact.getHeight()) - 1)
        );
        int sectionIndex = Math.floorDiv(clampedY - this.artifact.getMinY(), 16);
        ChunkSection section = this.artifact.getSections(sectionIndex);
        int localX = Math.floorMod(blockX, 16) >>> 2;
        int localY = Math.floorMod(clampedY, 16) >>> 2;
        int localZ = Math.floorMod(blockZ, 16) >>> 2;
        int packedIndex = localY * 16 + localZ * 4 + localX;
        PackedPalette palette = section.getBiomes();
        return this.biomes[palette.getEntries(PaletteCodec.unpack(palette, packedIndex))].value();
    }

    private static BlockData[] decodeBlockStates(
        List<BlockState> states,
        Function<String, BlockData> resolver
    ) {
        RemoteClient.require(!states.isEmpty() && states.size() <= 65_536, "invalid block-state dictionary size");
        BlockData[] result = new BlockData[states.size()];
        BlockState previous = null;
        for (int index = 0; index < states.size(); index++) {
            BlockState state = states.get(index);
            NamespacedKey stateKey = NamespacedKey.fromString(state.getName());
            RemoteClient.require(
                state.getName().length() <= 256
                    && stateKey != null
                    && stateKey.toString().equals(state.getName()),
                "block identifier is not explicitly namespaced and canonical"
            );
            List<BlockProperty> properties = state.getPropertiesList();
            Map<String, String> expectedProperties = new TreeMap<>();
            for (BlockProperty property : properties) {
                RemoteClient.require(
                    !property.getName().isEmpty()
                        && property.getName().length() <= 64
                        && property.getName().chars().allMatch(ArtifactView::isIdentifierPathCharacter)
                        && !property.getValue().isEmpty()
                        && property.getValue().length() <= 128
                        && property.getValue().chars().allMatch(ArtifactView::isIdentifierPathCharacter),
                    "invalid block property"
                );
                RemoteClient.require(
                    expectedProperties.put(property.getName(), property.getValue()) == null,
                    "duplicate block property name"
                );
            }
            RemoteClient.require(
                properties.stream().map(BlockProperty::getName).toList().equals(
                    expectedProperties.keySet().stream().toList()
                ),
                "block properties are not strictly sorted by name"
            );
            if (previous != null) {
                RemoteClient.require(
                    compareBlockStates(previous, state) < 0,
                    "block-state dictionary is not strictly sorted"
                );
            }

            String description = serializeBlockState(state);
            BlockData parsed = resolver.apply(description);
            RemoteClient.require(parsed != null, "worker block state is absent from the Folia registry: " + description);
            ParsedBlockState parsedState = parseBukkitBlockState(parsed.getAsString());
            RemoteClient.require(
                parsedState.name().equals(state.getName())
                    && parsedState.properties().equals(expectedProperties),
                "worker block state does not resolve exactly in the Folia registry: " + description
            );
            result[index] = parsed;
            previous = state;
        }
        return result;
    }

    private static String serializeBlockState(BlockState state) {
        StringBuilder serialized = new StringBuilder(state.getName());
        if (state.getPropertiesCount() != 0) {
            serialized.append('[');
            for (int index = 0; index < state.getPropertiesCount(); index++) {
                if (index != 0) {
                    serialized.append(',');
                }
                BlockProperty property = state.getProperties(index);
                serialized.append(property.getName()).append('=').append(property.getValue());
            }
            serialized.append(']');
        }
        return serialized.toString();
    }

    private static ParsedBlockState parseBukkitBlockState(String description) {
        int propertiesStart = description.indexOf('[');
        if (propertiesStart < 0) {
            return new ParsedBlockState(description, Map.of());
        }
        RemoteClient.require(description.endsWith("]"), "Folia returned a malformed block-state description");
        Map<String, String> properties = new TreeMap<>();
        String body = description.substring(propertiesStart + 1, description.length() - 1);
        if (!body.isEmpty()) {
            for (String entry : body.split(",", -1)) {
                int equals = entry.indexOf('=');
                RemoteClient.require(equals > 0 && equals == entry.lastIndexOf('='), "malformed Folia block property");
                RemoteClient.require(
                    properties.put(entry.substring(0, equals), entry.substring(equals + 1)) == null,
                    "duplicate Folia block property"
                );
            }
        }
        return new ParsedBlockState(description.substring(0, propertiesStart), properties);
    }

    private static int compareBlockStates(BlockState left, BlockState right) {
        int name = left.getName().compareTo(right.getName());
        if (name != 0) {
            return name;
        }
        int common = Math.min(left.getPropertiesCount(), right.getPropertiesCount());
        for (int index = 0; index < common; index++) {
            BlockProperty leftProperty = left.getProperties(index);
            BlockProperty rightProperty = right.getProperties(index);
            int propertyName = leftProperty.getName().compareTo(rightProperty.getName());
            if (propertyName != 0) {
                return propertyName;
            }
            int propertyValue = leftProperty.getValue().compareTo(rightProperty.getValue());
            if (propertyValue != 0) {
                return propertyValue;
            }
        }
        return Integer.compare(left.getPropertiesCount(), right.getPropertiesCount());
    }

    private static ResolvedBiome[] decodeBiomes(
        List<String> names,
        Function<String, ResolvedBiome> resolver
    ) {
        RemoteClient.require(!names.isEmpty() && names.size() <= 4_096, "invalid biome dictionary size");
        ResolvedBiome[] result = new ResolvedBiome[names.size()];
        String previous = null;
        for (int index = 0; index < names.size(); index++) {
            String name = names.get(index);
            RemoteClient.require(previous == null || previous.compareTo(name) < 0, "biome dictionary is not strictly sorted");
            NamespacedKey key = NamespacedKey.fromString(name);
            RemoteClient.require(
                name.length() <= 256 && key != null && key.toString().equals(name),
                "biome identifier is not explicitly namespaced and canonical"
            );
            ResolvedBiome biome = resolver.apply(name);
            RemoteClient.require(biome != null, "worker biome is absent from Folia registry: " + name);
            result[index] = biome;
            previous = name;
        }
        return result;
    }

    private static EnumMap<HeightMap, int[]> decodeHeightmaps(
        List<dev.steelmc.worldgen.protocol.v1.Heightmap> maps,
        int height
    ) {
        RemoteClient.require(
            maps.size() == 2
                && maps.get(0).getType() == dev.steelmc.worldgen.protocol.v1.HeightmapType.HEIGHTMAP_WORLD_SURFACE_WG
                && maps.get(1).getType() == dev.steelmc.worldgen.protocol.v1.HeightmapType.HEIGHTMAP_OCEAN_FLOOR_WG,
            "NOISE heightmaps are not in canonical order"
        );
        EnumMap<HeightMap, int[]> result = new EnumMap<>(HeightMap.class);
        for (dev.steelmc.worldgen.protocol.v1.Heightmap map : maps) {
            HeightMap type = switch (map.getType()) {
                case HEIGHTMAP_WORLD_SURFACE -> HeightMap.WORLD_SURFACE;
                case HEIGHTMAP_MOTION_BLOCKING -> HeightMap.MOTION_BLOCKING;
                case HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES -> HeightMap.MOTION_BLOCKING_NO_LEAVES;
                case HEIGHTMAP_OCEAN_FLOOR -> HeightMap.OCEAN_FLOOR;
                case HEIGHTMAP_WORLD_SURFACE_WG -> HeightMap.WORLD_SURFACE_WG;
                case HEIGHTMAP_OCEAN_FLOOR_WG -> HeightMap.OCEAN_FLOOR_WG;
                case HEIGHTMAP_UNSPECIFIED, UNRECOGNIZED ->
                    throw new IllegalStateException("invalid heightmap type");
            };
            RemoteClient.require(map.getFirstAvailableRelativeToMinYCount() == 256, "heightmap has wrong length");
            int[] values = map.getFirstAvailableRelativeToMinYList().stream()
                .mapToInt(Integer::intValue)
                .toArray();
            for (int value : values) {
                RemoteClient.require(value >= 0 && value <= height, "heightmap value is outside the artifact range");
            }
            RemoteClient.require(result.put(type, values) == null, "duplicate heightmap type");
        }
        return result;
    }

    record ResolvedBiome(Biome value) {
    }

    private record ParsedBlockState(String name, Map<String, String> properties) {
    }

    private static boolean isIdentifierPathCharacter(int character) {
        return character >= 'a' && character <= 'z'
            || character >= '0' && character <= '9'
            || character == '_'
            || character == '-'
            || character == '.'
            || character == '/';
    }

    private static int validatePostprocessing(ChunkArtifactV1 artifact, int sectionCount) {
        RemoteClient.require(artifact.getPostprocessingCount() == sectionCount, "post-processing section count mismatch");
        int count = 0;
        for (int index = 0; index < sectionCount; index++) {
            PostProcessingSection section = artifact.getPostprocessing(index);
            RemoteClient.require(
                section.getSectionY() == Math.floorDiv(artifact.getMinY(), 16) + index,
                "post-processing sections are not contiguous"
            );
            RemoteClient.require(
                section.getPackedOffsetsCount() <= 4096,
                "too many post-processing offsets in one section"
            );
            for (int packed : section.getPackedOffsetsList()) {
                RemoteClient.require(packed >= 0 && packed <= 0x0fff, "invalid post-processing offset");
                count++;
            }
        }
        return count;
    }

}
