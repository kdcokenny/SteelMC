package io.papermc.paper.worldgen.steel;

import com.google.protobuf.ByteString;
import com.mojang.brigadier.exceptions.CommandSyntaxException;
import dev.steelmc.worldgen.protocol.v1.BlockProperty;
import dev.steelmc.worldgen.protocol.v1.Capabilities;
import dev.steelmc.worldgen.protocol.v1.ChunkArtifactV1;
import dev.steelmc.worldgen.protocol.v1.ChunkSection;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.HeightmapType;
import dev.steelmc.worldgen.protocol.v1.PackedPalette;
import dev.steelmc.worldgen.protocol.v1.PostProcessingSection;
import dev.steelmc.worldgen.protocol.v1.Stage;
import it.unimi.dsi.fastutil.shorts.ShortArrayList;
import it.unimi.dsi.fastutil.shorts.ShortList;
import java.util.BitSet;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import net.minecraft.commands.arguments.blocks.BlockStateParser;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.util.Mth;
import net.minecraft.util.SimpleBitStorage;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.server.level.ServerLevel;

/** Strict Steel V1 decoder and atomic NOISE import plan for a protected fresh ProtoChunk. */
final class SteelNoiseArtifact {
    private static final int BLOCK_VOLUME = 16 * 16 * 16;
    private static final int BIOME_VOLUME = 4 * 4 * 4;

    private SteelNoiseArtifact() {
    }

    static ImportPlan prepare(
        final ServerLevel level,
        final ProtoChunk center,
        final ChunkArtifactV1 artifact,
        final GenerateRequest request,
        final Capabilities capabilities
    ) {
        require(artifact.getArtifactVersion() == 1, "unsupported artifact version");
        require(artifact.getMinecraftVersion().equals(request.getMinecraftVersion()), "artifact Minecraft version mismatch");
        require(
            artifact.getCanonicalRequestSha256().equals(ByteString.copyFrom(SteelRemoteNoise.canonicalRequestSha256(request))),
            "artifact canonical request digest mismatch"
        );
        require(artifact.getGeneratorSha256().equals(capabilities.getGeneratorSha256()), "artifact generator fingerprint mismatch");
        require(artifact.getRegistrySha256().equals(capabilities.getRegistrySha256()), "artifact registry fingerprint mismatch");
        require(artifact.getDimensionKey().equals(request.getDimensionKey()), "artifact dimension mismatch");
        require(artifact.getSeed() == request.getSeed(), "artifact seed mismatch");
        require(artifact.getChunkX() == request.getChunkX(), "artifact chunk X mismatch");
        require(artifact.getChunkZ() == request.getChunkZ(), "artifact chunk Z mismatch");
        require(artifact.getMinY() == request.getMinY(), "artifact minimum Y mismatch");
        require(artifact.getHeight() == request.getHeight(), "artifact height mismatch");
        require(
            artifact.getCompletedStagesList().equals(List.of(Stage.STAGE_BIOMES, Stage.STAGE_NOISE)),
            "NOISE artifact must contain exactly BIOMES then NOISE"
        );
        require(artifact.getHeight() > 0 && artifact.getHeight() <= 4096, "invalid artifact height");
        require(artifact.getHeight() % 16 == 0 && artifact.getMinY() % 16 == 0, "unaligned artifact range");
        require(center.getMinY() == artifact.getMinY() && center.getHeight() == artifact.getHeight(), "target chunk range mismatch");
        require(
            center.getPos().x() == artifact.getChunkX() && center.getPos().z() == artifact.getChunkZ(),
            "target chunk position mismatch"
        );

        final BlockState[] blockStates = decodeBlockStates(level, artifact.getBlockStatesList());
        final Identifier[] biomes = decodeBiomes(level, artifact.getBiomesList());
        final int sectionCount = artifact.getHeight() / 16;
        require(center.getSections().length == sectionCount, "target section count mismatch");
        require(artifact.getSectionsCount() == sectionCount, "artifact section count mismatch");
        final BitSet usedBlockStates = new BitSet(blockStates.length);
        final BitSet usedBiomes = new BitSet(biomes.length);
        for (int index = 0; index < sectionCount; index++) {
            final ChunkSection section = artifact.getSections(index);
            final int expectedY = Math.floorDiv(artifact.getMinY(), 16) + index;
            require(section.getSectionY() == expectedY, "artifact sections are not contiguous");
            usedBlockStates.or(PaletteCodec.validate(section.getBlockStates(), BLOCK_VOLUME, blockStates.length));
            usedBiomes.or(PaletteCodec.validate(section.getBiomes(), BIOME_VOLUME, biomes.length));
        }
        require(usedBlockStates.cardinality() == blockStates.length, "block-state dictionary contains an unused entry");
        require(usedBiomes.cardinality() == biomes.length, "biome dictionary contains an unused entry");

        final int[][] artifactHeightmaps = decodeHeightmaps(artifact, artifact.getHeight());
        final ShortList[] preparedPostprocessing = decodePostprocessing(artifact, sectionCount);

        final LevelChunkSection[] originalSections = center.getSections().clone();
        final ShortList[] targetPostprocessing = center.getPostProcessing();
        require(targetPostprocessing != null && targetPostprocessing.length == sectionCount, "target post-processing array mismatch");
        final ShortList[] originalPostprocessing = targetPostprocessing.clone();
        requireFresh(center, originalSections, originalPostprocessing);

        final LevelChunkSection[] preparedSections = new LevelChunkSection[sectionCount];
        for (int sectionIndex = 0; sectionIndex < sectionCount; sectionIndex++) {
            final LevelChunkSection source = center.getSection(sectionIndex);
            final ChunkSection encoded = artifact.getSections(sectionIndex);
            final PackedPalette biomePalette = encoded.getBiomes();
            for (int index = 0; index < BIOME_VOLUME; index++) {
                final int x = index & 3;
                final int z = (index >>> 2) & 3;
                final int y = index >>> 4;
                final Identifier expected = biomes[biomePalette.getEntries(PaletteCodec.unpack(biomePalette, index))];
                require(source.getNoiseBiome(x, y, z).is(expected), "artifact biome differs from the completed BIOMES stage");
            }

            final LevelChunkSection prepared = source.copy();
            final PackedPalette blockPalette = encoded.getBlockStates();
            for (int index = 0; index < BLOCK_VOLUME; index++) {
                final int x = index & 15;
                final int z = (index >>> 4) & 15;
                final int y = index >>> 8;
                final int dictionaryIndex = blockPalette.getEntries(PaletteCodec.unpack(blockPalette, index));
                prepared.setBlockState(x, y, z, blockStates[dictionaryIndex], false);
            }
            preparedSections[sectionIndex] = prepared;
        }

        final int[] computedSurface = computeHeightmap(preparedSections, Heightmap.Types.WORLD_SURFACE_WG);
        final int[] computedOceanFloor = computeHeightmap(preparedSections, Heightmap.Types.OCEAN_FLOOR_WG);
        requireHeightmapsEqual(artifactHeightmaps[0], computedSurface, "WORLD_SURFACE_WG");
        requireHeightmapsEqual(artifactHeightmaps[1], computedOceanFloor, "OCEAN_FLOOR_WG");
        final Heightmap preparedSurface = prepareHeightmap(center, Heightmap.Types.WORLD_SURFACE_WG, computedSurface);
        final Heightmap preparedOceanFloor = prepareHeightmap(center, Heightmap.Types.OCEAN_FLOOR_WG, computedOceanFloor);

        return new ImportPlan(
            center,
            center.getPos().x(),
            center.getPos().z(),
            originalSections,
            originalPostprocessing,
            center.heightmaps.get(Heightmap.Types.WORLD_SURFACE_WG),
            center.heightmaps.get(Heightmap.Types.OCEAN_FLOOR_WG),
            preparedSections,
            preparedPostprocessing,
            preparedSurface,
            preparedOceanFloor
        );
    }

    private static BlockState[] decodeBlockStates(
        final ServerLevel level,
        final List<dev.steelmc.worldgen.protocol.v1.BlockState> states
    ) {
        require(!states.isEmpty() && states.size() <= 65_536, "invalid block-state dictionary size");
        final BlockState[] result = new BlockState[states.size()];
        dev.steelmc.worldgen.protocol.v1.BlockState previous = null;
        for (int index = 0; index < states.size(); index++) {
            final dev.steelmc.worldgen.protocol.v1.BlockState state = states.get(index);
            final Identifier stateKey = canonicalIdentifier(state.getName(), "block");
            final Map<String, String> expectedProperties = new TreeMap<>();
            for (final BlockProperty property : state.getPropertiesList()) {
                require(
                    !property.getName().isEmpty()
                        && property.getName().length() <= 64
                        && property.getName().chars().allMatch(SteelNoiseArtifact::isIdentifierPathCharacter)
                        && !property.getValue().isEmpty()
                        && property.getValue().length() <= 128
                        && property.getValue().chars().allMatch(SteelNoiseArtifact::isIdentifierPathCharacter),
                    "invalid block property"
                );
                require(expectedProperties.put(property.getName(), property.getValue()) == null, "duplicate block property name");
            }
            require(
                state.getPropertiesList().stream().map(BlockProperty::getName).toList().equals(expectedProperties.keySet().stream().toList()),
                "block properties are not strictly sorted by name"
            );
            if (previous != null) {
                require(compareBlockStates(previous, state) < 0, "block-state dictionary is not strictly sorted");
            }

            final String description = serializeBlockState(state);
            final BlockStateParser.BlockResult parsed;
            try {
                parsed = BlockStateParser.parseForBlock(level.registryAccess().lookupOrThrow(Registries.BLOCK), description, false);
            } catch (final CommandSyntaxException exception) {
                throw new IllegalStateException("worker block state is absent from the Folia registry: " + description, exception);
            }
            require(parsed.nbt() == null, "worker block state unexpectedly contains NBT");
            require(parsed.blockState().typeHolder().is(stateKey), "worker block identifier did not resolve exactly: " + description);
            final Map<String, String> actualProperties = new TreeMap<>();
            parsed.blockState().getValues().forEach(value -> {
                final String replaced = actualProperties.put(value.property().getName(), value.valueName());
                require(replaced == null, "Folia block state contains a duplicate property");
            });
            require(
                actualProperties.equals(expectedProperties),
                "worker block state does not specify the complete Folia property set: " + description
            );
            result[index] = parsed.blockState();
            previous = state;
        }
        return result;
    }

    private static Identifier[] decodeBiomes(final ServerLevel level, final List<String> names) {
        require(!names.isEmpty() && names.size() <= 4_096, "invalid biome dictionary size");
        final Registry<Biome> registry = level.registryAccess().lookupOrThrow(Registries.BIOME);
        final Identifier[] result = new Identifier[names.size()];
        String previous = null;
        for (int index = 0; index < names.size(); index++) {
            final String name = names.get(index);
            require(previous == null || previous.compareTo(name) < 0, "biome dictionary is not strictly sorted");
            final Identifier identifier = canonicalIdentifier(name, "biome");
            require(registry.containsKey(identifier), "worker biome is absent from the Folia registry: " + name);
            result[index] = identifier;
            previous = name;
        }
        return result;
    }

    private static int[][] decodeHeightmaps(final ChunkArtifactV1 artifact, final int height) {
        require(
            artifact.getHeightmapsCount() == 2
                && artifact.getHeightmaps(0).getType() == HeightmapType.HEIGHTMAP_WORLD_SURFACE_WG
                && artifact.getHeightmaps(1).getType() == HeightmapType.HEIGHTMAP_OCEAN_FLOOR_WG,
            "NOISE heightmaps are not in canonical order"
        );
        final int[][] result = new int[2][];
        for (int mapIndex = 0; mapIndex < 2; mapIndex++) {
            final dev.steelmc.worldgen.protocol.v1.Heightmap map = artifact.getHeightmaps(mapIndex);
            require(map.getFirstAvailableRelativeToMinYCount() == 256, "heightmap has wrong length");
            final int[] values = map.getFirstAvailableRelativeToMinYList().stream().mapToInt(Integer::intValue).toArray();
            for (final int value : values) {
                require(value >= 0 && value <= height, "heightmap value is outside the artifact range");
            }
            result[mapIndex] = values;
        }
        return result;
    }

    private static ShortList[] decodePostprocessing(final ChunkArtifactV1 artifact, final int sectionCount) {
        require(artifact.getPostprocessingCount() == sectionCount, "post-processing section count mismatch");
        final ShortList[] result = new ShortList[sectionCount];
        for (int index = 0; index < sectionCount; index++) {
            final PostProcessingSection section = artifact.getPostprocessing(index);
            require(
                section.getSectionY() == Math.floorDiv(artifact.getMinY(), 16) + index,
                "post-processing sections are not contiguous"
            );
            require(section.getPackedOffsetsCount() <= 4096, "too many post-processing offsets in one section");
            final ShortArrayList positions = new ShortArrayList(section.getPackedOffsetsCount());
            for (final int packed : section.getPackedOffsetsList()) {
                require(packed >= 0 && packed <= 0x0fff, "invalid post-processing offset");
                positions.add((short)packed);
            }
            result[index] = positions;
        }
        return result;
    }

    private static int[] computeHeightmap(final LevelChunkSection[] sections, final Heightmap.Types type) {
        final int[] values = new int[256];
        for (int z = 0; z < 16; z++) {
            for (int x = 0; x < 16; x++) {
                search:
                for (int sectionIndex = sections.length - 1; sectionIndex >= 0; sectionIndex--) {
                    final LevelChunkSection section = sections[sectionIndex];
                    for (int y = 15; y >= 0; y--) {
                        if (type.isOpaque().test(section.getBlockState(x, y, z))) {
                            values[z * 16 + x] = sectionIndex * 16 + y + 1;
                            break search;
                        }
                    }
                }
            }
        }
        return values;
    }

    private static Heightmap prepareHeightmap(final ProtoChunk center, final Heightmap.Types type, final int[] values) {
        final int bits = Mth.ceillog2(center.getHeight() + 1);
        final long[] packed = new SimpleBitStorage(bits, 256, values).getRaw();
        final Heightmap result = new Heightmap(center, type);
        require(result.getRawData().length == packed.length, "prepared heightmap storage length mismatch");
        System.arraycopy(packed, 0, result.getRawData(), 0, packed.length);
        return result;
    }

    private static void requireHeightmapsEqual(final int[] expected, final int[] actual, final String type) {
        for (int index = 0; index < 256; index++) {
            require(expected[index] == actual[index], type + " artifact heightmap mismatch at index " + index);
        }
    }

    private static void requireFresh(
        final ProtoChunk center,
        final LevelChunkSection[] expectedSections,
        final ShortList[] expectedPostprocessing
    ) {
        require(center.getClass() == ProtoChunk.class, "remote NOISE requires an exact ProtoChunk");
        require(center.getPersistedStatus() == ChunkStatus.BIOMES, "remote NOISE requires an exact BIOMES parent");
        require(center.getBelowZeroRetrogen() == null, "remote NOISE does not support retrogen");
        require(center.getBlendingData() == null, "remote NOISE does not support blending data");
        require(center.getSections().length == expectedSections.length, "target sections changed during import");
        for (int index = 0; index < expectedSections.length; index++) {
            require(center.getSection(index) == expectedSections[index], "target section identity changed during import");
            require(center.getSection(index).hasOnlyAir(), "fresh BIOMES chunk contains pre-NOISE blocks");
        }
        final ShortList[] current = center.getPostProcessing();
        require(current != null && current.length == expectedPostprocessing.length, "target post-processing array changed during import");
        for (int index = 0; index < current.length; index++) {
            require(current[index] == expectedPostprocessing[index], "target post-processing identity changed during import");
            require(current[index] == null || current[index].isEmpty(), "fresh BIOMES chunk contains pre-NOISE post-processing");
        }
    }

    private static Identifier canonicalIdentifier(final String text, final String kind) {
        final Identifier identifier = Identifier.tryParse(text);
        require(
            text.length() <= 256 && text.indexOf(':') > 0 && identifier != null && identifier.toString().equals(text),
            kind + " identifier is not explicitly namespaced and canonical"
        );
        return identifier;
    }

    private static String serializeBlockState(final dev.steelmc.worldgen.protocol.v1.BlockState state) {
        final StringBuilder result = new StringBuilder(state.getName());
        if (state.getPropertiesCount() != 0) {
            result.append('[');
            for (int index = 0; index < state.getPropertiesCount(); index++) {
                if (index != 0) {
                    result.append(',');
                }
                final BlockProperty property = state.getProperties(index);
                result.append(property.getName()).append('=').append(property.getValue());
            }
            result.append(']');
        }
        return result.toString();
    }

    private static int compareBlockStates(
        final dev.steelmc.worldgen.protocol.v1.BlockState left,
        final dev.steelmc.worldgen.protocol.v1.BlockState right
    ) {
        final int name = left.getName().compareTo(right.getName());
        if (name != 0) {
            return name;
        }
        final int common = Math.min(left.getPropertiesCount(), right.getPropertiesCount());
        for (int index = 0; index < common; index++) {
            final BlockProperty leftProperty = left.getProperties(index);
            final BlockProperty rightProperty = right.getProperties(index);
            final int propertyName = leftProperty.getName().compareTo(rightProperty.getName());
            if (propertyName != 0) {
                return propertyName;
            }
            final int propertyValue = leftProperty.getValue().compareTo(rightProperty.getValue());
            if (propertyValue != 0) {
                return propertyValue;
            }
        }
        return Integer.compare(left.getPropertiesCount(), right.getPropertiesCount());
    }

    private static boolean isIdentifierPathCharacter(final int character) {
        return character >= 'a' && character <= 'z'
            || character >= '0' && character <= '9'
            || character == '_'
            || character == '-'
            || character == '.'
            || character == '/';
    }

    static void require(final boolean condition, final String message) {
        if (!condition) {
            throw new IllegalStateException(message);
        }
    }

    static final class ImportPlan {
        private final ProtoChunk target;
        private final int chunkX;
        private final int chunkZ;
        private final LevelChunkSection[] originalSections;
        private final ShortList[] originalPostprocessing;
        private final Heightmap originalSurface;
        private final Heightmap originalOceanFloor;
        private final LevelChunkSection[] preparedSections;
        private final ShortList[] preparedPostprocessing;
        private final Heightmap preparedSurface;
        private final Heightmap preparedOceanFloor;

        private ImportPlan(
            final ProtoChunk target,
            final int chunkX,
            final int chunkZ,
            final LevelChunkSection[] originalSections,
            final ShortList[] originalPostprocessing,
            final Heightmap originalSurface,
            final Heightmap originalOceanFloor,
            final LevelChunkSection[] preparedSections,
            final ShortList[] preparedPostprocessing,
            final Heightmap preparedSurface,
            final Heightmap preparedOceanFloor
        ) {
            this.target = target;
            this.chunkX = chunkX;
            this.chunkZ = chunkZ;
            this.originalSections = originalSections;
            this.originalPostprocessing = originalPostprocessing;
            this.originalSurface = originalSurface;
            this.originalOceanFloor = originalOceanFloor;
            this.preparedSections = preparedSections;
            this.preparedPostprocessing = preparedPostprocessing;
            this.preparedSurface = preparedSurface;
            this.preparedOceanFloor = preparedOceanFloor;
        }

        ProtoChunk commit() {
            require(this.target.getPos().x() == this.chunkX && this.target.getPos().z() == this.chunkZ, "target position changed during import");
            requireFresh(this.target, this.originalSections, this.originalPostprocessing);
            require(
                this.target.heightmaps.get(Heightmap.Types.WORLD_SURFACE_WG) == this.originalSurface
                    && this.target.heightmaps.get(Heightmap.Types.OCEAN_FLOOR_WG) == this.originalOceanFloor,
                "target world-generation heightmaps changed during import"
            );

            System.arraycopy(this.preparedSections, 0, this.target.getSections(), 0, this.preparedSections.length);
            this.target.heightmaps.put(Heightmap.Types.WORLD_SURFACE_WG, this.preparedSurface);
            this.target.heightmaps.put(Heightmap.Types.OCEAN_FLOOR_WG, this.preparedOceanFloor);
            System.arraycopy(
                this.preparedPostprocessing,
                0,
                this.target.getPostProcessing(),
                0,
                this.preparedPostprocessing.length
            );
            return this.target;
        }
    }

    private static final class PaletteCodec {
        private PaletteCodec() {
        }

        static BitSet validate(final PackedPalette palette, final int volume, final int dictionarySize) {
            final int entries = palette.getEntriesCount();
            require(entries >= 1, "palette is empty");
            for (int index = 0; index < entries; index++) {
                final int value = palette.getEntries(index);
                require(value >= 0 && value < dictionarySize, "palette dictionary index out of bounds");
                if (index != 0) {
                    require(palette.getEntries(index - 1) < value, "palette is not strictly sorted");
                }
            }
            final int expectedBits = entries == 1 ? 0 : Integer.SIZE - Integer.numberOfLeadingZeros(entries - 1);
            require(palette.getBitsPerEntry() == expectedBits, "palette width is not canonical");
            final long totalBits = (long)volume * expectedBits;
            final int expectedBytes = Math.toIntExact(Math.ceilDiv(totalBits, 8));
            require(palette.getData().size() == expectedBytes, "packed palette has wrong length");
            final int trailingBits = (int)(totalBits % 8);
            if (trailingBits != 0) {
                final int validMask = (1 << trailingBits) - 1;
                final int last = Byte.toUnsignedInt(palette.getData().byteAt(expectedBytes - 1));
                require((last & ~validMask) == 0, "packed palette has nonzero padding bits");
            }
            final BitSet usedLocalEntries = new BitSet(entries);
            final BitSet usedDictionaryEntries = new BitSet(dictionarySize);
            for (int index = 0; index < volume; index++) {
                final int local = unpack(palette, index);
                require(local >= 0 && local < entries, "packed palette index out of bounds");
                usedLocalEntries.set(local);
                usedDictionaryEntries.set(palette.getEntries(local));
            }
            require(usedLocalEntries.cardinality() == entries, "palette contains an unused entry");
            return usedDictionaryEntries;
        }

        static int unpack(final PackedPalette palette, final int index) {
            final int width = palette.getBitsPerEntry();
            if (width == 0) {
                return 0;
            }
            final int startBit = Math.multiplyExact(index, width);
            int value = 0;
            for (int bit = 0; bit < width; bit++) {
                final int sourceBit = startBit + bit;
                final int sourceByte = Byte.toUnsignedInt(palette.getData().byteAt(sourceBit >>> 3));
                value |= ((sourceByte >>> (sourceBit & 7)) & 1) << bit;
            }
            return value;
        }
    }
}
