package dev.steelmc.worldgen;

import java.util.HexFormat;
import java.util.Objects;
import org.bukkit.generator.BiomeProvider;
import org.bukkit.generator.ChunkGenerator;
import org.bukkit.plugin.java.JavaPlugin;
import org.jetbrains.annotations.NotNull;
import org.jetbrains.annotations.Nullable;

public final class SteelRemoteWorldGenPlugin extends JavaPlugin {
    private RemoteClient client;
    private RemoteChunkGenerator generator;
    private RemoteBiomeProvider biomeProvider;

    @Override
    public void onLoad() {
        saveDefaultConfig();
        PluginSettings settings = PluginSettings.load(getConfig());
        if (settings.plaintext()) {
            getLogger().warning(
                "Worker transport is plaintext. Keep it on a private network or terminate mTLS in a trusted proxy."
            );
        }
        if (settings.allowUnappliedPostprocessing()) {
            getLogger().severe(
                "PROTOTYPE DIVERGENCE ENABLED: aquifer post-processing offsets cannot be imported by Bukkit ChunkData."
            );
        }
        this.client = new RemoteClient(settings);
        this.generator = new RemoteChunkGenerator(this.client, settings);
        this.biomeProvider = new RemoteBiomeProvider(this.client);
        getLogger().info(
            "Pinned Steel worker profile "
                + HexFormat.of().formatHex(this.client.capabilities().getProfileSha256().toByteArray())
        );
    }

    @Override
    public void onEnable() {
        getLogger().warning(
            "The Bukkit bridge is a synchronous projection prototype, not the production Moonrise scheduler patch."
        );
    }

    @Override
    public void onDisable() {
        if (this.client != null) {
            this.client.close();
        }
    }

    @Override
    public @Nullable ChunkGenerator getDefaultWorldGenerator(
        @NotNull String worldName,
        @Nullable String id
    ) {
        if (id != null && !id.isBlank() && !id.equals("overworld")) {
            throw new IllegalArgumentException("unknown Steel worker profile id: " + id);
        }
        return Objects.requireNonNull(this.generator, "plugin has not completed onLoad");
    }

    @Override
    public @Nullable BiomeProvider getDefaultBiomeProvider(
        @NotNull String worldName,
        @Nullable String id
    ) {
        return Objects.requireNonNull(this.biomeProvider, "plugin has not completed onLoad");
    }
}
