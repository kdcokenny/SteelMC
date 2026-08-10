package dev.steelmc.worldgen;

import java.util.Random;
import org.bukkit.generator.ChunkGenerator;
import org.bukkit.generator.WorldInfo;
import org.jetbrains.annotations.NotNull;

final class RemoteChunkGenerator extends ChunkGenerator {
    private final RemoteClient client;
    private final PluginSettings settings;

    RemoteChunkGenerator(RemoteClient client, PluginSettings settings) {
        this.client = client;
        this.settings = settings;
    }

    @Override
    public void generateNoise(
        @NotNull WorldInfo worldInfo,
        @NotNull Random random,
        int chunkX,
        int chunkZ,
        @NotNull ChunkData chunkData
    ) {
        this.client
            .get(worldInfo, chunkX, chunkZ)
            .apply(chunkData, this.settings.allowUnappliedPostprocessing());
    }

    @Override
    public boolean shouldGenerateNoise() {
        return false;
    }

    @Override
    public boolean shouldGenerateSurface() {
        return true;
    }

    @Override
    public boolean shouldGenerateCaves() {
        return true;
    }

    @Override
    public boolean shouldGenerateDecorations() {
        return true;
    }

    @Override
    public boolean shouldGenerateMobs() {
        return true;
    }

    @Override
    public boolean shouldGenerateStructures() {
        return true;
    }
}
