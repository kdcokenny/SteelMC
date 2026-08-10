package dev.steelmc.worldgen;

import java.time.Duration;
import java.util.Locale;
import org.bukkit.configuration.file.FileConfiguration;

record PluginSettings(
    String endpoint,
    boolean plaintext,
    String tlsCaCertificate,
    String tlsClientCertificate,
    String tlsClientKey,
    String tlsDomain,
    Duration deadline,
    String expectedMinecraftVersion,
    String expectedProfileSha256,
    boolean allowUnpinnedProfile,
    int maxCacheChunks,
    long maxCacheBytes,
    boolean allowUnappliedPostprocessing
) {
    static PluginSettings load(FileConfiguration config) {
        String endpoint = requiredString(config, "worker.endpoint");
        boolean plaintext = config.getBoolean("worker.plaintext", true);
        String tlsCaCertificate = config.getString("worker.tls.ca-certificate", "").trim();
        String tlsClientCertificate = config.getString("worker.tls.client-certificate", "").trim();
        String tlsClientKey = config.getString("worker.tls.client-key", "").trim();
        String tlsDomain = config.getString("worker.tls.domain", "").trim();
        if (plaintext) {
            RemoteClient.require(
                tlsCaCertificate.isEmpty()
                    && tlsClientCertificate.isEmpty()
                    && tlsClientKey.isEmpty()
                    && tlsDomain.isEmpty(),
                "worker.tls settings require plaintext=false"
            );
        } else {
            RemoteClient.require(
                !tlsCaCertificate.isEmpty()
                    && !tlsClientCertificate.isEmpty()
                    && !tlsClientKey.isEmpty()
                    && !tlsDomain.isEmpty(),
                "TLS requires worker.tls ca-certificate, client-certificate, client-key, and domain"
            );
        }
        long deadlineMillis = config.getLong("worker.deadline-millis", 30_000);
        RemoteClient.require(
            deadlineMillis >= 1 && deadlineMillis <= 600_000,
            "worker.deadline-millis must be in 1..=600000"
        );
        String expectedMinecraftVersion = requiredString(
            config,
            "worker.expected-minecraft-version"
        );
        String profile = config.getString("worker.expected-profile-sha256", "")
            .trim()
            .toLowerCase(Locale.ROOT);
        RemoteClient.require(
            profile.isEmpty() || profile.matches("[0-9a-f]{64}"),
            "worker.expected-profile-sha256 must be empty or 64 lowercase hex digits"
        );
        boolean allowUnpinned = config.getBoolean("worker.allow-unpinned-profile", false);
        int maxCacheChunks = config.getInt("cache.max-chunks", 2048);
        RemoteClient.require(
            maxCacheChunks >= 0 && maxCacheChunks <= 1_000_000,
            "cache.max-chunks must be in 0..=1000000"
        );
        long maxCacheBytes = config.getLong("cache.max-bytes", 256L * 1024 * 1024);
        RemoteClient.require(
            maxCacheBytes >= 0 && maxCacheBytes <= 64L * 1024 * 1024 * 1024,
            "cache.max-bytes must be in 0..=68719476736"
        );
        return new PluginSettings(
            endpoint,
            plaintext,
            tlsCaCertificate,
            tlsClientCertificate,
            tlsClientKey,
            tlsDomain,
            Duration.ofMillis(deadlineMillis),
            expectedMinecraftVersion,
            profile,
            allowUnpinned,
            maxCacheChunks,
            maxCacheBytes,
            config.getBoolean("prototype.allow-unapplied-postprocessing", false)
        );
    }

    private static String requiredString(FileConfiguration config, String path) {
        String value = config.getString(path, "").trim();
        RemoteClient.require(!value.isEmpty(), path + " must not be empty");
        return value;
    }
}
