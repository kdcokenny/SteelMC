package dev.steelmc.worldgen;

import io.papermc.paper.registry.RegistryAccess;
import io.papermc.paper.registry.RegistryKey;
import java.util.List;
import java.util.stream.StreamSupport;
import org.bukkit.Registry;
import org.bukkit.block.Biome;
import org.bukkit.generator.BiomeProvider;
import org.bukkit.generator.WorldInfo;
import org.jetbrains.annotations.NotNull;

final class RemoteBiomeProvider extends BiomeProvider {
    private final RemoteClient client;
    private final List<Biome> possibleBiomes;

    RemoteBiomeProvider(RemoteClient client) {
        this.client = client;
        Registry<Biome> registry = RegistryAccess.registryAccess().getRegistry(RegistryKey.BIOME);
        this.possibleBiomes = StreamSupport.stream(registry.spliterator(), false).toList();
    }

    @Override
    public @NotNull Biome getBiome(
        @NotNull WorldInfo worldInfo,
        int x,
        int y,
        int z
    ) {
        return this.client
            .get(worldInfo, Math.floorDiv(x, 16), Math.floorDiv(z, 16))
            .biomeAt(x, y, z);
    }

    @Override
    public @NotNull List<Biome> getBiomes(@NotNull WorldInfo worldInfo) {
        return this.possibleBiomes;
    }
}
