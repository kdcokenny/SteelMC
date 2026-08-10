package io.papermc.paper.worldgen.steel;

import com.google.protobuf.ByteString;
import dev.steelmc.worldgen.protocol.v1.Capabilities;
import dev.steelmc.worldgen.protocol.v1.ChunkArtifactV1;
import dev.steelmc.worldgen.protocol.v1.ChunkSection;
import dev.steelmc.worldgen.protocol.v1.Compression;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.GenerationContext;
import dev.steelmc.worldgen.protocol.v1.HeightmapType;
import dev.steelmc.worldgen.protocol.v1.PackedPalette;
import dev.steelmc.worldgen.protocol.v1.PostProcessingSection;
import dev.steelmc.worldgen.protocol.v1.Stage;
import java.util.Collections;
import net.minecraft.core.RegistryAccess;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import org.bukkit.support.RegistryHelper;
import org.bukkit.support.environment.VanillaFeature;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotSame;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

@VanillaFeature
class SteelNoiseArtifactTest {
    private static final int MIN_Y = -64;
    private static final int HEIGHT = 384;
    private static final int SECTION_COUNT = HEIGHT / 16;
    private static final byte[] GENERATOR = new byte[32];
    private static final byte[] REGISTRY = new byte[32];

    @Test
    void prepareIsDetachedAndCommitAtomicallyInstallsNativeNoiseState() {
        final RegistryAccess registries = RegistryHelper.registryAccess();
        final ProtoChunk chunk = freshBiomesChunk(registries);
        final LevelChunkSection[] originals = chunk.getSections().clone();
        final Fixture fixture = fixture();

        final SteelNoiseArtifact.ImportPlan plan = SteelNoiseArtifact.prepare(
            registries, chunk, fixture.artifact, fixture.request, fixture.capabilities
        );
        assertFreshAndUnchanged(chunk, originals);

        assertSame(chunk, plan.commit());
        assertEquals(ChunkStatus.BIOMES, chunk.getPersistedStatus());
        for (int index = 0; index < SECTION_COUNT; index++) {
            assertNotSame(originals[index], chunk.getSection(index));
            assertSame(Blocks.STONE.defaultBlockState(), chunk.getSection(index).getBlockState(0, 0, 0));
            assertTrue(chunk.getSection(index).getNoiseBiome(0, 0, 0).is(Biomes.PLAINS));
        }
        assertEquals(320, chunk.heightmaps.get(Heightmap.Types.WORLD_SURFACE_WG).getFirstAvailable(0, 0));
        assertEquals(320, chunk.heightmaps.get(Heightmap.Types.OCEAN_FLOOR_WG).getFirstAvailable(15, 15));
    }

    @Test
    void lateValidationFailureLeavesTheNativeChunkUntouched() {
        final RegistryAccess registries = RegistryHelper.registryAccess();
        final ProtoChunk chunk = freshBiomesChunk(registries);
        final LevelChunkSection[] originals = chunk.getSections().clone();
        final Fixture fixture = fixture();
        final var badOceanFloor = fixture.artifact.getHeightmaps(1).toBuilder()
            .setFirstAvailableRelativeToMinY(255, HEIGHT - 1)
            .build();
        final ChunkArtifactV1 malformed = fixture.artifact.toBuilder().setHeightmaps(1, badOceanFloor).build();

        assertThrows(
            IllegalStateException.class,
            () -> SteelNoiseArtifact.prepare(registries, chunk, malformed, fixture.request, fixture.capabilities)
        );
        assertFreshAndUnchanged(chunk, originals);
    }

    @Test
    void commitPreconditionFailurePerformsNoPartialAssignment() {
        final RegistryAccess registries = RegistryHelper.registryAccess();
        final ProtoChunk chunk = freshBiomesChunk(registries);
        final LevelChunkSection[] originals = chunk.getSections().clone();
        final Fixture fixture = fixture();
        final SteelNoiseArtifact.ImportPlan plan = SteelNoiseArtifact.prepare(
            registries, chunk, fixture.artifact, fixture.request, fixture.capabilities
        );
        chunk.heightmaps.put(Heightmap.Types.WORLD_SURFACE_WG, new Heightmap(chunk, Heightmap.Types.WORLD_SURFACE_WG));

        assertThrows(IllegalStateException.class, plan::commit);
        assertEquals(ChunkStatus.BIOMES, chunk.getPersistedStatus());
        for (int index = 0; index < SECTION_COUNT; index++) {
            assertSame(originals[index], chunk.getSection(index));
            assertTrue(chunk.getSection(index).hasOnlyAir());
        }
    }

    private static ProtoChunk freshBiomesChunk(final RegistryAccess registries) {
        final ProtoChunk chunk = new ProtoChunk(
            new ChunkPos(6, 2),
            UpgradeData.EMPTY,
            LevelHeightAccessor.create(MIN_Y, HEIGHT),
            PalettedContainerFactory.create(registries),
            null
        );
        chunk.setPersistedStatus(ChunkStatus.BIOMES);
        return chunk;
    }

    private static void assertFreshAndUnchanged(final ProtoChunk chunk, final LevelChunkSection[] originals) {
        assertEquals(ChunkStatus.BIOMES, chunk.getPersistedStatus());
        for (int index = 0; index < SECTION_COUNT; index++) {
            assertSame(originals[index], chunk.getSection(index));
            assertTrue(chunk.getSection(index).hasOnlyAir());
        }
        assertTrue(chunk.heightmaps.isEmpty());
    }

    private static Fixture fixture() {
        final GenerateRequest request = GenerateRequest.newBuilder()
            .setRequestId(ByteString.copyFrom(new byte[16]))
            .setMinecraftVersion("26.2")
            .setProfileId("test")
            .setDimensionKey("minecraft:overworld")
            .setSeed(13579)
            .setChunkX(6)
            .setChunkZ(2)
            .setMinY(MIN_Y)
            .setHeight(HEIGHT)
            .setFirstStage(Stage.STAGE_BIOMES)
            .setLastStage(Stage.STAGE_NOISE)
            .setExpectedGeneratorSha256(ByteString.copyFrom(GENERATOR))
            .setExpectedRegistrySha256(ByteString.copyFrom(REGISTRY))
            .addAcceptedCompression(Compression.COMPRESSION_NONE)
            .setGenerationContext(GenerationContext.GENERATION_CONTEXT_FRESH)
            .build();
        final Capabilities capabilities = Capabilities.newBuilder()
            .setGeneratorSha256(ByteString.copyFrom(GENERATOR))
            .setRegistrySha256(ByteString.copyFrom(REGISTRY))
            .build();
        final PackedPalette singleton = PackedPalette.newBuilder().addEntries(0).setBitsPerEntry(0).build();
        final ChunkArtifactV1.Builder artifact = ChunkArtifactV1.newBuilder()
            .setArtifactVersion(1)
            .setMinecraftVersion("26.2")
            .setCanonicalRequestSha256(ByteString.copyFrom(SteelRemoteNoise.canonicalRequestSha256(request)))
            .setGeneratorSha256(ByteString.copyFrom(GENERATOR))
            .setRegistrySha256(ByteString.copyFrom(REGISTRY))
            .setDimensionKey("minecraft:overworld")
            .setSeed(13579)
            .setChunkX(6)
            .setChunkZ(2)
            .setMinY(MIN_Y)
            .setHeight(HEIGHT)
            .addCompletedStages(Stage.STAGE_BIOMES)
            .addCompletedStages(Stage.STAGE_NOISE)
            .addBlockStates(dev.steelmc.worldgen.protocol.v1.BlockState.newBuilder().setName("minecraft:stone"))
            .addBiomes("minecraft:plains");
        for (int index = 0; index < SECTION_COUNT; index++) {
            artifact.addSections(ChunkSection.newBuilder()
                .setSectionY(Math.floorDiv(MIN_Y, 16) + index)
                .setBlockStates(singleton)
                .setBiomes(singleton));
            artifact.addPostprocessing(PostProcessingSection.newBuilder()
                .setSectionY(Math.floorDiv(MIN_Y, 16) + index));
        }
        artifact.addHeightmaps(dev.steelmc.worldgen.protocol.v1.Heightmap.newBuilder()
            .setType(HeightmapType.HEIGHTMAP_WORLD_SURFACE_WG)
            .addAllFirstAvailableRelativeToMinY(Collections.nCopies(256, HEIGHT)));
        artifact.addHeightmaps(dev.steelmc.worldgen.protocol.v1.Heightmap.newBuilder()
            .setType(HeightmapType.HEIGHTMAP_OCEAN_FLOOR_WG)
            .addAllFirstAvailableRelativeToMinY(Collections.nCopies(256, HEIGHT)));
        return new Fixture(request, capabilities, artifact.build());
    }

    private record Fixture(GenerateRequest request, Capabilities capabilities, ChunkArtifactV1 artifact) {
    }
}
