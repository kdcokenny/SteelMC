package dev.steelmc.worldgen;

import com.google.protobuf.ByteString;
import dev.steelmc.worldgen.protocol.v1.Capabilities;
import dev.steelmc.worldgen.protocol.v1.ChunkArtifactV1;
import dev.steelmc.worldgen.protocol.v1.Compression;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.GenerateResponse;
import dev.steelmc.worldgen.protocol.v1.GenerationContext;
import dev.steelmc.worldgen.protocol.v1.GetCapabilitiesRequest;
import dev.steelmc.worldgen.protocol.v1.Stage;
import dev.steelmc.worldgen.protocol.v1.WorldGenServiceGrpc;
import io.grpc.ManagedChannel;
import io.grpc.netty.shaded.io.grpc.netty.GrpcSslContexts;
import io.grpc.netty.shaded.io.grpc.netty.NettyChannelBuilder;
import io.grpc.netty.shaded.io.netty.channel.EventLoopGroup;
import io.grpc.netty.shaded.io.netty.channel.MultiThreadIoEventLoopGroup;
import io.grpc.netty.shaded.io.netty.channel.nio.NioIoHandler;
import io.grpc.netty.shaded.io.netty.channel.socket.nio.NioSocketChannel;
import java.io.File;
import java.net.InetSocketAddress;
import java.net.URI;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.time.Duration;
import java.util.HexFormat;
import java.util.List;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;
import org.bukkit.generator.WorldInfo;

final class RemoteClient implements AutoCloseable {
    private static final byte[] REQUEST_DOMAIN = new byte[] {
        0x53, 0x57, 0x47, 0x52, 0x45, 0x51, 0x31, 0x00
    };
    private static final int MAX_ARTIFACT_BYTES = 8 * 1024 * 1024;

    private final ManagedChannel channel;
    private final EventLoopGroup eventLoopGroup;
    private final WorldGenServiceGrpc.WorldGenServiceBlockingStub stub;
    private final Duration deadline;
    private final int maxCacheChunks;
    private final long maxCacheBytes;
    private final AtomicLong cachedBytes = new AtomicLong();
    private final ConcurrentHashMap<ChunkKey, CompletableFuture<CachedArtifact>> cache =
        new ConcurrentHashMap<>();
    private final ConcurrentLinkedQueue<ChunkKey> insertionOrder = new ConcurrentLinkedQueue<>();
    private final Capabilities capabilities;

    RemoteClient(PluginSettings settings) {
        URI endpoint = URI.create(settings.endpoint());
        RemoteClient.require(endpoint.getHost() != null, "worker.endpoint must be an absolute HTTP(S) URI");
        int defaultPort = settings.plaintext() ? 80 : 443;
        int port = endpoint.getPort() < 0 ? defaultPort : endpoint.getPort();
        if (settings.plaintext()) {
            RemoteClient.require("http".equals(endpoint.getScheme()), "plaintext worker endpoint must use http://");
        } else {
            RemoteClient.require("https".equals(endpoint.getScheme()), "TLS worker endpoint must use https://");
        }
        this.eventLoopGroup = new MultiThreadIoEventLoopGroup(1, NioIoHandler.newFactory());
        NettyChannelBuilder builder = NettyChannelBuilder
            .forAddress(new InetSocketAddress(endpoint.getHost(), port))
            .eventLoopGroup(this.eventLoopGroup)
            .channelType(NioSocketChannel.class)
            .maxInboundMessageSize(MAX_ARTIFACT_BYTES + 64 * 1024);
        if (settings.plaintext()) {
            builder.usePlaintext();
        } else {
            try {
                builder
                    .sslContext(
                        GrpcSslContexts
                            .forClient()
                            .trustManager(new File(settings.tlsCaCertificate()))
                            .keyManager(
                                new File(settings.tlsClientCertificate()),
                                new File(settings.tlsClientKey())
                            )
                            .build()
                    )
                    .overrideAuthority(settings.tlsDomain());
            } catch (javax.net.ssl.SSLException exception) {
                this.eventLoopGroup.shutdownGracefully(0, 5, TimeUnit.SECONDS);
                throw new IllegalStateException("failed to configure worker mutual TLS", exception);
            }
        }
        ManagedChannel builtChannel;
        try {
            builtChannel = builder.build();
        } catch (RuntimeException exception) {
            this.eventLoopGroup.shutdownGracefully(0, 5, TimeUnit.SECONDS);
            throw exception;
        }
        this.channel = builtChannel;
        this.deadline = settings.deadline();
        this.maxCacheChunks = settings.maxCacheChunks();
        this.maxCacheBytes = settings.maxCacheBytes();
        this.stub = WorldGenServiceGrpc.newBlockingStub(this.channel).withCompression("gzip");
        try {
            this.capabilities = this.deadlineStub()
                .getCapabilities(GetCapabilitiesRequest.getDefaultInstance());
            validateCapabilities(settings);
        } catch (RuntimeException exception) {
            shutdownTransport();
            throw exception;
        }
    }

    Capabilities capabilities() {
        return this.capabilities;
    }

    ArtifactView get(WorldInfo worldInfo, int chunkX, int chunkZ) {
        validateWorld(worldInfo);
        ChunkKey key = new ChunkKey(worldInfo.getUID(), chunkX, chunkZ);
        CompletableFuture<CachedArtifact> created = new CompletableFuture<>();
        CompletableFuture<CachedArtifact> existing = this.cache.putIfAbsent(key, created);
        if (existing != null) {
            return join(existing).view();
        }

        try {
            CachedArtifact artifact = fetch(chunkX, chunkZ);
            created.complete(artifact);
            this.cachedBytes.addAndGet(artifact.encodedBytes());
            this.insertionOrder.add(key);
            evictIfNeeded();
            return artifact.view();
        } catch (RuntimeException exception) {
            created.completeExceptionally(exception);
            this.cache.remove(key, created);
            throw exception;
        }
    }

    private CachedArtifact fetch(int chunkX, int chunkZ) {
        byte[] requestId = uuidBytes(UUID.randomUUID());
        GenerateRequest request = GenerateRequest.newBuilder()
            .setRequestId(ByteString.copyFrom(requestId))
            .setMinecraftVersion(this.capabilities.getMinecraftVersion())
            .setProfileId(this.capabilities.getProfileId())
            .setDimensionKey(this.capabilities.getDimensionKey())
            .setSeed(this.capabilities.getSeed())
            .setChunkX(chunkX)
            .setChunkZ(chunkZ)
            .setMinY(this.capabilities.getMinY())
            .setHeight(this.capabilities.getHeight())
            .setFirstStage(Stage.STAGE_BIOMES)
            .setLastStage(Stage.STAGE_NOISE)
            .setExpectedGeneratorSha256(this.capabilities.getGeneratorSha256())
            .setExpectedRegistrySha256(this.capabilities.getRegistrySha256())
            .addAcceptedCompression(Compression.COMPRESSION_NONE)
            .setGenerationContext(GenerationContext.GENERATION_CONTEXT_FRESH)
            .build();
        byte[] canonicalRequest = canonicalRequestSha256(request);
        GenerateResponse response = this.deadlineStub().generate(request);
        require(response.getRequestId().equals(request.getRequestId()), "response request id mismatch");
        require(
            MessageDigest.isEqual(canonicalRequest, response.getCanonicalRequestSha256().toByteArray()),
            "response canonical request digest mismatch"
        );
        require(
            response.getGeneratorSha256().equals(this.capabilities.getGeneratorSha256()),
            "response generator fingerprint mismatch"
        );
        require(
            response.getRegistrySha256().equals(this.capabilities.getRegistrySha256()),
            "response registry fingerprint mismatch"
        );
        require(response.getArtifactVersion() == 1, "response artifact version mismatch");
        require(
            response.getCompression() == Compression.COMPRESSION_NONE,
            "worker returned unsupported application compression"
        );
        byte[] bytes = response.getArtifact().toByteArray();
        require(bytes.length == response.getUncompressedSize(), "artifact size mismatch");
        require(bytes.length <= MAX_ARTIFACT_BYTES, "artifact exceeds client size limit");
        require(
            MessageDigest.isEqual(sha256(bytes), response.getArtifactSha256().toByteArray()),
            "artifact SHA-256 mismatch"
        );
        try {
            ChunkArtifactV1 artifact = ChunkArtifactV1.parseFrom(bytes);
            return new CachedArtifact(
                ArtifactView.decode(artifact, request, this.capabilities),
                bytes.length
            );
        } catch (com.google.protobuf.InvalidProtocolBufferException exception) {
            throw new IllegalStateException("worker returned malformed artifact protobuf", exception);
        }
    }

    private WorldGenServiceGrpc.WorldGenServiceBlockingStub deadlineStub() {
        return this.stub.withDeadlineAfter(this.deadline.toMillis(), TimeUnit.MILLISECONDS);
    }

    private void validateCapabilities(PluginSettings settings) {
        require(this.capabilities.getProtocolMajor() == 1, "worker protocol major is not 1");
        require(this.capabilities.getArtifactVersionsList().contains(1), "worker does not support artifact V1");
        require(
            this.capabilities.getCompletedStagesList().equals(List.of(Stage.STAGE_BIOMES, Stage.STAGE_NOISE)),
            "worker advertises an unsupported stage interval"
        );
        require(
            this.capabilities.getCompressionList().equals(List.of(Compression.COMPRESSION_NONE)),
            "worker advertises unknown or unsupported artifact compression"
        );
        require(
            this.capabilities.getMinecraftVersion().equals(settings.expectedMinecraftVersion()),
            "worker Minecraft version does not match plugin configuration"
        );
        require(
            !this.capabilities.getProfileId().isEmpty()
                && this.capabilities.getProfileId().length() <= 128
                && this.capabilities.getProfileId().chars().allMatch(character -> character >= 0x20 && character <= 0x7e),
            "invalid worker profile id"
        );
        require(this.capabilities.getGeneratorSha256().size() == 32, "invalid generator fingerprint");
        require(this.capabilities.getRegistrySha256().size() == 32, "invalid registry fingerprint");
        require(this.capabilities.getProfileSha256().size() == 32, "invalid profile fingerprint");
        require(
            this.capabilities.getMaxRequestBytes() == 64 * 1024,
            "worker request bound differs from the V1 client"
        );
        require(
            this.capabilities.getMaxArtifactBytes() == MAX_ARTIFACT_BYTES,
            "worker artifact bound differs from the V1 client"
        );
        require(
            this.capabilities.getMaxInFlight() >= 1
                && this.capabilities.getMaxInFlight() <= 4096
                && this.capabilities.getMaxInFlightPerPeer() >= 1
                && this.capabilities.getMaxInFlightPerPeer() <= this.capabilities.getMaxInFlight(),
            "invalid worker global or per-peer concurrency bound"
        );
        require(this.capabilities.getProtocolMinor() >= 1, "worker protocol minor lacks source metadata");
        require(
            this.capabilities.getCorrespondingSourceUrl().startsWith("https://")
                || this.capabilities.getCorrespondingSourceUrl().startsWith("http://"),
            "worker does not advertise an HTTP(S) corresponding-source location"
        );
        require(
            this.capabilities.getSourceSha256().matches("[0-9a-f]{64}"),
            "worker source digest is invalid"
        );
        require(
            this.capabilities.getLicenseExpression().equals("AGPL-3.0-or-later"),
            "worker license expression is unsupported"
        );
        for (String identity : List.of(
            this.capabilities.getExternalBuildId(),
            this.capabilities.getRustcId(),
            this.capabilities.getCargoId(),
            this.capabilities.getBuildTarget(),
            this.capabilities.getBuildConfiguration()
        )) {
            require(
                !identity.isEmpty()
                    && identity.length() <= 4096
                    && identity.chars().allMatch(character -> character >= 0x20 && character <= 0x7e),
                "worker build attestation contains invalid text"
            );
        }
        require(!this.capabilities.getSteelResumable(), "prototype does not support Steel-resumable artifacts");
        require(!this.capabilities.getSupportsBlending(), "prototype requires fresh-only worker profile");
        require(!this.capabilities.getSupportsRetrogen(), "prototype requires fresh-only worker profile");
        String actualProfile = HexFormat.of().formatHex(this.capabilities.getProfileSha256().toByteArray());
        if (!settings.allowUnpinnedProfile()) {
            require(
                !settings.expectedProfileSha256().isBlank(),
                "worker.expected-profile-sha256 is required unless allow-unpinned-profile is true"
            );
            require(
                actualProfile.equalsIgnoreCase(settings.expectedProfileSha256()),
                "worker profile fingerprint does not match plugin configuration"
            );
        }
    }

    private void validateWorld(WorldInfo worldInfo) {
        require(worldInfo.getSeed() == this.capabilities.getSeed(), "Folia world seed does not match worker profile");
        require(worldInfo.getMinHeight() == this.capabilities.getMinY(), "Folia minimum Y does not match worker profile");
        require(
            worldInfo.getMaxHeight() - worldInfo.getMinHeight() == this.capabilities.getHeight(),
            "Folia world height does not match worker profile"
        );
    }

    private void evictIfNeeded() {
        while (this.cache.size() > this.maxCacheChunks || this.cachedBytes.get() > this.maxCacheBytes) {
            ChunkKey oldest = this.insertionOrder.poll();
            if (oldest == null) {
                return;
            }
            CompletableFuture<CachedArtifact> value = this.cache.get(oldest);
            if (value != null && value.isDone() && this.cache.remove(oldest, value)) {
                CachedArtifact artifact = join(value);
                this.cachedBytes.addAndGet(-artifact.encodedBytes());
            }
        }
    }

    private static CachedArtifact join(CompletableFuture<CachedArtifact> future) {
        try {
            return future.join();
        } catch (CompletionException exception) {
            Throwable cause = exception.getCause();
            if (cause instanceof RuntimeException runtimeException) {
                throw runtimeException;
            }
            throw exception;
        }
    }

    static byte[] canonicalRequestSha256(GenerateRequest request) {
        MessageDigest digest = newSha256();
        digest.update(REQUEST_DOMAIN);
        putLengthPrefixed(digest, request.getMinecraftVersion());
        putLengthPrefixed(digest, request.getDimensionKey());
        digest.update(ByteBuffer.allocate(Long.BYTES).putLong(request.getSeed()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getChunkX()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getChunkZ()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getMinY()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getHeight()).array());
        digest.update((byte) request.getFirstStageValue());
        digest.update((byte) request.getLastStageValue());
        digest.update(request.getExpectedGeneratorSha256().toByteArray());
        digest.update(request.getExpectedRegistrySha256().toByteArray());
        digest.update(new byte[] {0, 0});
        return digest.digest();
    }

    private static void putLengthPrefixed(MessageDigest digest, String value) {
        byte[] bytes = value.getBytes(StandardCharsets.UTF_8);
        require(bytes.length <= 0xffff, "canonical request string exceeds u16 length");
        digest.update(ByteBuffer.allocate(Short.BYTES).putShort((short) bytes.length).array());
        digest.update(bytes);
    }

    private static byte[] uuidBytes(UUID uuid) {
        return ByteBuffer.allocate(16)
            .putLong(uuid.getMostSignificantBits())
            .putLong(uuid.getLeastSignificantBits())
            .array();
    }

    static byte[] sha256(byte[] bytes) {
        return newSha256().digest(bytes);
    }

    private static MessageDigest newSha256() {
        try {
            return MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException exception) {
            throw new IllegalStateException("JVM has no SHA-256 provider", exception);
        }
    }

    static void require(boolean condition, String message) {
        if (!condition) {
            throw new IllegalStateException(message);
        }
    }

    private void shutdownTransport() {
        this.channel.shutdownNow();
        this.eventLoopGroup.shutdownGracefully(0, 5, TimeUnit.SECONDS);
        try {
            boolean channelTerminated = this.channel.awaitTermination(5, TimeUnit.SECONDS);
            boolean eventLoopTerminated = this.eventLoopGroup.terminationFuture().await(5, TimeUnit.SECONDS);
            if (!channelTerminated || !eventLoopTerminated) {
                throw new IllegalStateException("gRPC transport did not terminate");
            }
        } catch (InterruptedException exception) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException("interrupted while closing gRPC transport", exception);
        }
    }

    @Override
    public void close() {
        shutdownTransport();
    }

    private record CachedArtifact(ArtifactView view, int encodedBytes) {}

    private record ChunkKey(UUID worldId, int x, int z) {
        private ChunkKey {
            Objects.requireNonNull(worldId, "worldId");
        }
    }
}
