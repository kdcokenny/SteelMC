package io.papermc.paper.worldgen.steel;

import com.google.common.util.concurrent.FutureCallback;
import com.google.common.util.concurrent.Futures;
import com.google.common.util.concurrent.ListenableFuture;
import com.google.common.util.concurrent.MoreExecutors;
import com.google.protobuf.ByteString;
import dev.steelmc.worldgen.protocol.v1.CancelRequest;
import dev.steelmc.worldgen.protocol.v1.CancelResponse;
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
import io.papermc.paper.configuration.GlobalConfiguration;
import it.unimi.dsi.fastutil.shorts.ShortList;
import java.io.File;
import java.net.InetSocketAddress;
import java.net.URI;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.blending.Blender;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/** Startup-pinned asynchronous Steel V1 NOISE client. */
public final class SteelRemoteNoise implements AutoCloseable {
    private static final Logger LOGGER = LoggerFactory.getLogger(SteelRemoteNoise.class);
    private static final byte[] REQUEST_DOMAIN = new byte[] {0x53, 0x57, 0x47, 0x52, 0x45, 0x51, 0x31, 0x00};
    private static final byte[] REGISTRY_DOMAIN = "steel-worldgen-registry-v2".getBytes(StandardCharsets.UTF_8);
    private static final int MAX_ARTIFACT_BYTES = 8 * 1024 * 1024;
    private static final AtomicReference<SteelRemoteNoise> INSTANCE = new AtomicReference<>();

    private final String targetWorld;
    private final long deadlineMillis;
    private final int maxInFlight;
    private final ResourceKey<NoiseGeneratorSettings> expectedNoiseSettings;
    private final ManagedChannel channel;
    private final EventLoopGroup eventLoopGroup;
    private final WorldGenServiceGrpc.WorldGenServiceFutureStub futureStub;
    private final Capabilities capabilities;
    private final ThreadPoolExecutor importerExecutor;
    private final ConcurrentHashMap<ChunkKey, RequestContext> requests = new ConcurrentHashMap<>();
    private final ConcurrentHashMap<ChunkKey, CancellationReservation> preAdmissionCancellations = new ConcurrentHashMap<>();
    private final ArrayDeque<RequestContext> admissionQueue = new ArrayDeque<>();
    private final int maxQueued;
    private final AtomicBoolean closed = new AtomicBoolean();
    private int activeRequests;

    private SteelRemoteNoise(final MinecraftServer server, final GlobalConfiguration.SteelRemoteWorldgen settings) {
        validateSettings(settings);
        this.targetWorld = settings.targetWorld;
        this.deadlineMillis = settings.deadlineMillis;
        this.maxInFlight = settings.maxInFlight;
        this.maxQueued = settings.maxQueued;
        this.expectedNoiseSettings = ResourceKey.create(
            Registries.NOISE_SETTINGS,
            canonicalIdentifier(settings.expectedNoiseSettings, "expected noise settings")
        );

        final URI endpoint = URI.create(settings.endpoint);
        final int defaultPort = settings.plaintext ? 80 : 443;
        final int port = endpoint.getPort() < 0 ? defaultPort : endpoint.getPort();
        this.eventLoopGroup = new MultiThreadIoEventLoopGroup(1, NioIoHandler.newFactory());
        final NettyChannelBuilder builder = NettyChannelBuilder
            .forAddress(new InetSocketAddress(endpoint.getHost(), port))
            .eventLoopGroup(this.eventLoopGroup)
            .channelType(NioSocketChannel.class)
            .maxInboundMessageSize(MAX_ARTIFACT_BYTES + 64 * 1024);
        if (settings.plaintext) {
            builder.usePlaintext();
        } else {
            try {
                builder
                    .sslContext(
                        GrpcSslContexts
                            .forClient()
                            .trustManager(new File(settings.tls.caCertificate))
                            .keyManager(new File(settings.tls.clientCertificate), new File(settings.tls.clientKey))
                            .build()
                    )
                    .overrideAuthority(settings.tls.domain);
            } catch (final javax.net.ssl.SSLException exception) {
                this.eventLoopGroup.shutdownGracefully(0, 5, TimeUnit.SECONDS);
                throw new IllegalStateException("failed to configure Steel worker mutual TLS", exception);
            }
        }

        ManagedChannel builtChannel = null;
        Capabilities loadedCapabilities = null;
        try {
            builtChannel = builder.build();
            loadedCapabilities = WorldGenServiceGrpc
                .newBlockingStub(builtChannel)
                .withCompression("gzip")
                .withDeadlineAfter(this.deadlineMillis, TimeUnit.MILLISECONDS)
                .getCapabilities(GetCapabilitiesRequest.getDefaultInstance());
            validateCapabilities(settings, loadedCapabilities, registryFingerprint(server));
        } catch (final RuntimeException exception) {
            if (builtChannel != null) {
                builtChannel.shutdownNow();
            }
            this.eventLoopGroup.shutdownGracefully(0, 5, TimeUnit.SECONDS);
            throw exception;
        }
        this.channel = builtChannel;
        this.capabilities = loadedCapabilities;
        this.futureStub = WorldGenServiceGrpc.newFutureStub(this.channel).withCompression("gzip");
        this.importerExecutor = new ThreadPoolExecutor(
            this.maxInFlight,
            this.maxInFlight,
            0L,
            TimeUnit.MILLISECONDS,
            new java.util.concurrent.ArrayBlockingQueue<>(Math.max(1, this.maxInFlight)),
            Thread.ofPlatform().name("Steel-remote-NOISE-importer-", 0).daemon(true).factory(),
            new ThreadPoolExecutor.AbortPolicy()
        );
    }

    /** Initializes the startup-only client after Paper global configuration and registries are available. */
    public static void start(final MinecraftServer server) {
        final GlobalConfiguration.SteelRemoteWorldgen settings = GlobalConfiguration.get().steelRemoteWorldgen;
        if (!settings.enabled) {
            return;
        }
        final SteelRemoteNoise created = new SteelRemoteNoise(server, settings);
        if (!INSTANCE.compareAndSet(null, created)) {
            created.close();
            throw new IllegalStateException("Steel remote worldgen was initialized more than once");
        }
        LOGGER.info(
            "Steel remote NOISE enabled for world '{}' with profile {} and at most {} in-flight requests",
            created.targetWorld,
            created.capabilities.getProfileId(),
            created.maxInFlight
        );
    }

    /** Returns true only for the explicitly selected world; other worlds retain native Folia generation. */
    public static boolean handles(final ServerLevel level) {
        final SteelRemoteNoise instance = INSTANCE.get();
        return instance != null && instance.targetWorld.equals(level.getWorld().getName());
    }

    /** Starts a fail-closed asynchronous BIOMES-to-NOISE request for the protected center chunk. */
    public static CompletableFuture<ChunkAccess> generateNoise(
        final ServerLevel level,
        final ChunkGenerator generator,
        final Blender blender,
        final ChunkPos regionCenter,
        final ChunkAccess chunk
    ) {
        final SteelRemoteNoise instance = INSTANCE.get();
        if (instance == null || !instance.targetWorld.equals(level.getWorld().getName())) {
            return CompletableFuture.failedFuture(new IllegalStateException("Steel remote NOISE was called for an unselected world"));
        }
        try {
            return instance.generate(level, generator, blender, regionCenter, chunk);
        } catch (final RuntimeException exception) {
            return CompletableFuture.failedFuture(exception);
        }
    }

    /**
     * Cooperatively cancels an admitted NOISE request and forwards the V1 Cancel RPC when already active.
     *
     * @return whether cancellation won, the request was already terminal, or it was not admitted yet
     */
    public static CancellationResult cancel(final ServerLevel level, final int chunkX, final int chunkZ) {
        final SteelRemoteNoise instance = INSTANCE.get();
        return instance == null
            ? CancellationResult.NOT_FOUND
            : instance.cancelRequest(new ChunkKey(level, chunkX, chunkZ));
    }

    public enum CancellationResult {
        CANCELLED,
        TERMINAL,
        NOT_FOUND,
    }

    /** Reserves cancellation across the gap before a running status admits its request context. */
    public static CancellationReservation reserveCancellationBeforeAdmission(
        final ServerLevel level,
        final int chunkX,
        final int chunkZ
    ) {
        final SteelRemoteNoise instance = INSTANCE.get();
        return instance == null ? null : instance.reserveCancellation(new ChunkKey(level, chunkX, chunkZ));
    }

    /** Clears an unused pre-admission reservation when the local status task was cancelled before it ran. */
    public static void clearCancellationBeforeAdmission(final CancellationReservation reservation) {
        if (reservation != null) {
            reservation.owner.preAdmissionCancellations.remove(reservation.key, reservation);
        }
    }

    /** Idempotently closes the startup-pinned client from MinecraftServer.stopPart2. */
    public static void closeInstance() {
        final SteelRemoteNoise instance = INSTANCE.getAndSet(null);
        if (instance != null) {
            instance.close();
        }
    }

    private CompletableFuture<ChunkAccess> generate(
        final ServerLevel level,
        final ChunkGenerator generator,
        final Blender blender,
        final ChunkPos regionCenter,
        final ChunkAccess chunk
    ) {
        SteelNoiseArtifact.require(!this.closed.get(), "Steel remote NOISE client is closed");
        SteelNoiseArtifact.require(chunk.getClass() == ProtoChunk.class, "remote NOISE requires an exact ProtoChunk");
        final ProtoChunk center = (ProtoChunk)chunk;
        SteelNoiseArtifact.require(center.getPersistedStatus() == ChunkStatus.BIOMES, "remote NOISE requires an exact BIOMES parent");
        SteelNoiseArtifact.require(center.getBelowZeroRetrogen() == null, "remote NOISE does not support retrogen");
        SteelNoiseArtifact.require(center.getBlendingData() == null, "remote NOISE does not support blending data");
        SteelNoiseArtifact.require(blender.isEmpty(), "remote NOISE does not support a nonempty Blender");
        SteelNoiseArtifact.require(center.getPos().equals(regionCenter), "remote NOISE target is not the region center");
        SteelNoiseArtifact.require(
            level.dimension().identifier().toString().equals(this.capabilities.getDimensionKey()),
            "Folia dimension does not match the Steel profile"
        );
        SteelNoiseArtifact.require(level.getSeed() == this.capabilities.getSeed(), "Folia seed does not match the Steel profile");
        SteelNoiseArtifact.require(level.getMinY() == this.capabilities.getMinY(), "Folia minimum Y does not match the Steel profile");
        SteelNoiseArtifact.require(level.getHeight() == this.capabilities.getHeight(), "Folia height does not match the Steel profile");
        SteelNoiseArtifact.require(
            generator.getClass() == NoiseBasedChunkGenerator.class
                && ((NoiseBasedChunkGenerator)generator).stable(this.expectedNoiseSettings),
            "selected world does not use the configured exact vanilla noise settings preset"
        );
        for (final var section : center.getSections()) {
            SteelNoiseArtifact.require(section.hasOnlyAir(), "fresh BIOMES chunk contains pre-NOISE blocks");
        }
        final ShortList[] postprocessing = center.getPostProcessing();
        SteelNoiseArtifact.require(postprocessing != null, "target has no post-processing array");
        for (final ShortList positions : postprocessing) {
            SteelNoiseArtifact.require(positions == null || positions.isEmpty(), "fresh BIOMES chunk contains pre-NOISE post-processing");
        }

        final byte[] requestId = uuidBytes(UUID.randomUUID());
        final GenerateRequest request = GenerateRequest.newBuilder()
            .setRequestId(ByteString.copyFrom(requestId))
            .setMinecraftVersion(this.capabilities.getMinecraftVersion())
            .setProfileId(this.capabilities.getProfileId())
            .setDimensionKey(this.capabilities.getDimensionKey())
            .setSeed(this.capabilities.getSeed())
            .setChunkX(center.getPos().x())
            .setChunkZ(center.getPos().z())
            .setMinY(this.capabilities.getMinY())
            .setHeight(this.capabilities.getHeight())
            .setFirstStage(Stage.STAGE_BIOMES)
            .setLastStage(Stage.STAGE_NOISE)
            .setExpectedGeneratorSha256(this.capabilities.getGeneratorSha256())
            .setExpectedRegistrySha256(this.capabilities.getRegistrySha256())
            .addAcceptedCompression(Compression.COMPRESSION_NONE)
            .setGenerationContext(GenerationContext.GENERATION_CONTEXT_FRESH)
            .build();
        final RequestContext context = new RequestContext(
            new ChunkKey(level, center.getPos().x(), center.getPos().z()),
            center,
            request,
            canonicalRequestSha256(request)
        );
        if (this.consumePreAdmissionCancellation(context)) {
            return context.result;
        }
        final RequestContext collision = this.requests.putIfAbsent(context.key, context);
        SteelNoiseArtifact.require(collision == null, "duplicate Steel remote NOISE request for one chunk");
        if (this.consumePreAdmissionCancellation(context)) {
            return context.result;
        }
        this.admit(context);
        return context.result;
    }

    private CancellationReservation reserveCancellation(final ChunkKey key) {
        final CancellationReservation created = new CancellationReservation(this, key);
        final CancellationReservation existing = this.preAdmissionCancellations.putIfAbsent(key, created);
        if (existing != null) {
            return existing;
        }
        final RequestContext context = this.requests.get(key);
        if (context != null && this.preAdmissionCancellations.remove(key, created)) {
            created.result = switch (this.cancelRequest(key)) {
                case CANCELLED -> CancellationReservationResult.CANCELLED;
                case TERMINAL -> CancellationReservationResult.TERMINAL;
                case NOT_FOUND -> CancellationReservationResult.NOT_FOUND;
            };
        } else {
            created.result = CancellationReservationResult.NOT_FOUND;
        }
        return created;
    }

    private boolean consumePreAdmissionCancellation(final RequestContext context) {
        final CancellationReservation reservation = this.preAdmissionCancellations.remove(context.key);
        if (reservation == null) {
            return false;
        }
        context.commitGate.tryCancel(() -> false);
        context.result.completeExceptionally(new CancellationException("Steel remote NOISE request was cancelled before admission"));
        this.finish(context);
        return true;
    }

    private void admit(final RequestContext context) {
        boolean start = false;
        synchronized (this.admissionQueue) {
            if (this.closed.get()) {
                this.requests.remove(context.key, context);
                context.result.completeExceptionally(new IllegalStateException("Steel remote NOISE client is closed"));
                return;
            }
            if (this.activeRequests < this.maxInFlight) {
                this.activeRequests++;
                context.activeSlot = true;
                start = true;
            } else if (this.admissionQueue.size() < this.maxQueued) {
                context.queued = true;
                this.admissionQueue.addLast(context);
            } else {
                this.requests.remove(context.key, context);
                context.result.completeExceptionally(new IllegalStateException("Steel remote NOISE admission queue is full"));
            }
        }
        if (start) {
            this.startRequest(context);
        }
    }

    private void startRequest(final RequestContext context) {
        if (context.commitGate.isCancelled() || this.closed.get()) {
            context.result.completeExceptionally(new CancellationException("Steel remote NOISE request was cancelled before dispatch"));
            this.finish(context);
            return;
        }

        final ListenableFuture<GenerateResponse> call;
        try {
            call = this.futureStub
                .withDeadlineAfter(this.deadlineMillis, TimeUnit.MILLISECONDS)
                .generate(context.request);
            context.call = call;
            if (context.commitGate.isCancelled()) {
                call.cancel(true);
                this.sendCancel(context);
            }
        } catch (final RuntimeException exception) {
            context.result.completeExceptionally(context.failure(exception));
            this.finish(context);
            return;
        }
        Futures.addCallback(call, new FutureCallback<>() {
            @Override
            public void onSuccess(final GenerateResponse response) {
                if (context.commitGate.isCancelled()) {
                    context.result.completeExceptionally(new CancellationException("Steel remote NOISE request was cancelled"));
                    SteelRemoteNoise.this.finish(context);
                    return;
                }
                try {
                    SteelRemoteNoise.this.importerExecutor.execute(() -> SteelRemoteNoise.this.decodeAndCommit(context, response));
                } catch (final RejectedExecutionException exception) {
                    context.result.completeExceptionally(context.failure(exception));
                    SteelRemoteNoise.this.finish(context);
                }
            }

            @Override
            public void onFailure(final Throwable throwable) {
                context.result.completeExceptionally(context.failure(throwable));
                SteelRemoteNoise.this.finish(context);
            }
        }, MoreExecutors.directExecutor());
    }

    private void decodeAndCommit(final RequestContext context, final GenerateResponse response) {
        try {
            final ChunkArtifactV1 artifact = validateAndParseResponse(
                response,
                context.request,
                context.canonicalRequestSha256,
                this.capabilities
            );
            final SteelNoiseArtifact.ImportPlan plan = SteelNoiseArtifact.prepare(
                context.key.level,
                context.center,
                artifact,
                context.request,
                this.capabilities
            );
            context.result.complete(context.commitGate.commit(plan::commit));
        } catch (final Throwable throwable) {
            context.result.completeExceptionally(context.failure(throwable));
        } finally {
            this.finish(context);
        }
    }

    static ChunkArtifactV1 validateAndParseResponse(
        final GenerateResponse response,
        final GenerateRequest request,
        final byte[] canonicalRequestSha256,
        final Capabilities capabilities
    ) throws com.google.protobuf.InvalidProtocolBufferException {
        SteelNoiseArtifact.require(response.getArtifact().size() <= MAX_ARTIFACT_BYTES, "artifact exceeds client size limit");
        SteelNoiseArtifact.require(
            response.getUncompressedSize() <= MAX_ARTIFACT_BYTES
                && response.getUncompressedSize() == response.getArtifact().size(),
            "artifact size mismatch"
        );
        SteelNoiseArtifact.require(
            MessageDigest.isEqual(sha256(response.getArtifact().toByteArray()), response.getArtifactSha256().toByteArray()),
            "artifact SHA-256 mismatch"
        );
        SteelNoiseArtifact.require(response.getRequestId().equals(request.getRequestId()), "response request id mismatch");
        SteelNoiseArtifact.require(
            MessageDigest.isEqual(canonicalRequestSha256, response.getCanonicalRequestSha256().toByteArray()),
            "response canonical request digest mismatch"
        );
        SteelNoiseArtifact.require(
            response.getGeneratorSha256().equals(capabilities.getGeneratorSha256()),
            "response generator fingerprint mismatch"
        );
        SteelNoiseArtifact.require(
            response.getRegistrySha256().equals(capabilities.getRegistrySha256()),
            "response registry fingerprint mismatch"
        );
        SteelNoiseArtifact.require(response.getArtifactVersion() == 1, "response artifact version mismatch");
        SteelNoiseArtifact.require(
            response.getCompression() == Compression.COMPRESSION_NONE,
            "worker returned unsupported application compression"
        );
        return ChunkArtifactV1.parseFrom(response.getArtifact());
    }

    private CancellationResult cancelRequest(final ChunkKey key) {
        final RequestContext context = this.requests.get(key);
        if (context == null) {
            return CancellationResult.NOT_FOUND;
        }
        final CancellationResult result = context.commitGate.tryCancel(context.result::isDone);
        if (result == CancellationResult.TERMINAL) {
            return result;
        }

        boolean removedQueued = false;
        synchronized (this.admissionQueue) {
            if (context.queued) {
                removedQueued = this.admissionQueue.remove(context);
                context.queued = false;
            }
        }
        if (removedQueued) {
            context.result.completeExceptionally(new CancellationException("queued Steel remote NOISE request was cancelled"));
            this.finish(context);
            return CancellationResult.CANCELLED;
        }
        final ListenableFuture<GenerateResponse> call = context.call;
        if (call != null) {
            call.cancel(true);
            this.sendCancel(context);
        }
        return CancellationResult.CANCELLED;
    }

    private void sendCancel(final RequestContext context) {
        if (!context.cancelSent.compareAndSet(false, true)) {
            return;
        }
        final CancelRequest cancel = CancelRequest.newBuilder()
            .setRequestId(context.request.getRequestId())
            .setCanonicalRequestSha256(ByteString.copyFrom(context.canonicalRequestSha256))
            .build();
        try {
            final ListenableFuture<CancelResponse> future = this.futureStub
                .withDeadlineAfter(this.deadlineMillis, TimeUnit.MILLISECONDS)
                .cancel(cancel);
            Futures.addCallback(future, new FutureCallback<>() {
                @Override
                public void onSuccess(final CancelResponse ignored) {
                }

                @Override
                public void onFailure(final Throwable throwable) {
                    LOGGER.warn("Steel remote NOISE Cancel RPC failed for ({}, {})", context.key.chunkX, context.key.chunkZ, throwable);
                }
            }, MoreExecutors.directExecutor());
        } catch (final RuntimeException exception) {
            LOGGER.warn("Could not start Steel remote NOISE Cancel RPC for ({}, {})", context.key.chunkX, context.key.chunkZ, exception);
        }
    }

    private void finish(final RequestContext context) {
        if (!context.finished.compareAndSet(false, true)) {
            return;
        }
        this.requests.remove(context.key, context);
        if (!context.activeSlot) {
            return;
        }

        RequestContext next = null;
        synchronized (this.admissionQueue) {
            while (!this.admissionQueue.isEmpty()) {
                final RequestContext candidate = this.admissionQueue.removeFirst();
                candidate.queued = false;
                if (!candidate.commitGate.isCancelled()) {
                    candidate.activeSlot = true;
                    next = candidate;
                    break;
                }
                candidate.result.completeExceptionally(new CancellationException("queued Steel remote NOISE request was cancelled"));
                candidate.finished.set(true);
                this.requests.remove(candidate.key, candidate);
            }
            if (next == null) {
                this.activeRequests--;
            }
        }
        if (next != null) {
            this.startRequest(next);
        }
    }

    private static void validateSettings(final GlobalConfiguration.SteelRemoteWorldgen settings) {
        SteelNoiseArtifact.require(!settings.targetWorld.isBlank(), "steel-remote-worldgen.target-world must not be empty");
        canonicalIdentifier(settings.expectedNoiseSettings, "steel-remote-worldgen.expected-noise-settings");
        SteelNoiseArtifact.require(
            settings.expectedMinecraftVersion.equals("26.2"),
            "steel-remote-worldgen.expected-minecraft-version must be 26.2 for this fork"
        );
        SteelNoiseArtifact.require(
            settings.expectedProfileSha256.matches("[0-9a-f]{64}"),
            "steel-remote-worldgen.expected-profile-sha256 must be 64 lowercase hex digits"
        );
        SteelNoiseArtifact.require(
            settings.deadlineMillis >= 1 && settings.deadlineMillis <= 600_000,
            "steel-remote-worldgen.deadline-millis must be in 1..=600000"
        );
        SteelNoiseArtifact.require(
            settings.maxInFlight >= 1 && settings.maxInFlight <= 4096,
            "steel-remote-worldgen.max-in-flight must be in 1..=4096"
        );
        SteelNoiseArtifact.require(
            settings.maxQueued >= 0 && settings.maxQueued <= 65_536,
            "steel-remote-worldgen.max-queued must be in 0..=65536"
        );

        final URI endpoint;
        try {
            endpoint = URI.create(settings.endpoint);
        } catch (final IllegalArgumentException exception) {
            throw new IllegalStateException("steel-remote-worldgen.endpoint is not a URI", exception);
        }
        SteelNoiseArtifact.require(
            endpoint.isAbsolute() && endpoint.getHost() != null,
            "steel-remote-worldgen.endpoint must be an absolute HTTP(S) URI"
        );
        SteelNoiseArtifact.require(
            endpoint.getRawUserInfo() == null
                && (endpoint.getRawPath() == null || endpoint.getRawPath().isEmpty())
                && endpoint.getRawQuery() == null
                && endpoint.getRawFragment() == null,
            "steel-remote-worldgen.endpoint must not contain user-info, path, query, or fragment"
        );
        SteelNoiseArtifact.require(
            endpoint.getPort() == -1 || endpoint.getPort() >= 1 && endpoint.getPort() <= 65_535,
            "steel-remote-worldgen.endpoint port is invalid"
        );
        if (settings.plaintext) {
            SteelNoiseArtifact.require("http".equals(endpoint.getScheme()), "plaintext Steel endpoint must use http://");
            SteelNoiseArtifact.require(
                settings.tls.caCertificate.isEmpty()
                    && settings.tls.clientCertificate.isEmpty()
                    && settings.tls.clientKey.isEmpty()
                    && settings.tls.domain.isEmpty(),
                "steel-remote-worldgen.tls settings require plaintext=false"
            );
        } else {
            SteelNoiseArtifact.require("https".equals(endpoint.getScheme()), "TLS Steel endpoint must use https://");
            SteelNoiseArtifact.require(
                !settings.tls.caCertificate.isEmpty()
                    && !settings.tls.clientCertificate.isEmpty()
                    && !settings.tls.clientKey.isEmpty()
                    && !settings.tls.domain.isEmpty(),
                "Steel mutual TLS requires ca-certificate, client-certificate, client-key, and domain"
            );
        }
    }

    private static void validateCapabilities(
        final GlobalConfiguration.SteelRemoteWorldgen settings,
        final Capabilities capabilities,
        final byte[] localRegistrySha256
    ) {
        SteelNoiseArtifact.require(capabilities.getProtocolMajor() == 1, "worker protocol major is not 1");
        SteelNoiseArtifact.require(capabilities.getArtifactVersionsList().contains(1), "worker does not support artifact V1");
        SteelNoiseArtifact.require(
            capabilities.getCompletedStagesList().equals(List.of(Stage.STAGE_BIOMES, Stage.STAGE_NOISE)),
            "worker advertises an unsupported stage interval"
        );
        SteelNoiseArtifact.require(
            capabilities.getCompressionList().equals(List.of(Compression.COMPRESSION_NONE)),
            "worker advertises unsupported artifact compression"
        );
        SteelNoiseArtifact.require(
            capabilities.getMinecraftVersion().equals(settings.expectedMinecraftVersion),
            "worker Minecraft version does not match configuration"
        );
        SteelNoiseArtifact.require(
            !capabilities.getProfileId().isEmpty()
                && capabilities.getProfileId().length() <= 128
                && capabilities.getProfileId().chars().allMatch(character -> character >= 0x20 && character <= 0x7e),
            "invalid worker profile id"
        );
        SteelNoiseArtifact.require(capabilities.getGeneratorSha256().size() == 32, "invalid generator fingerprint");
        SteelNoiseArtifact.require(capabilities.getRegistrySha256().size() == 32, "invalid registry fingerprint");
        SteelNoiseArtifact.require(capabilities.getProfileSha256().size() == 32, "invalid profile fingerprint");
        SteelNoiseArtifact.require(
            capabilities.getMaxRequestBytes() == 64 * 1024,
            "worker request bound differs from the V1 importer"
        );
        SteelNoiseArtifact.require(
            capabilities.getMaxArtifactBytes() == MAX_ARTIFACT_BYTES,
            "worker artifact bound differs from the V1 importer"
        );
        SteelNoiseArtifact.require(
            capabilities.getMaxInFlight() >= 1
                && capabilities.getMaxInFlight() <= 4096
                && capabilities.getMaxInFlightPerPeer() >= 1
                && capabilities.getMaxInFlightPerPeer() <= capabilities.getMaxInFlight(),
            "invalid worker concurrency bounds"
        );
        SteelNoiseArtifact.require(
            settings.maxInFlight <= capabilities.getMaxInFlight()
                && settings.maxInFlight <= capabilities.getMaxInFlightPerPeer(),
            "configured max-in-flight exceeds worker admission bounds"
        );
        SteelNoiseArtifact.require(capabilities.getProtocolMinor() >= 1, "worker protocol minor lacks source metadata");
        SteelNoiseArtifact.require(
            capabilities.getCorrespondingSourceUrl().startsWith("https://")
                || capabilities.getCorrespondingSourceUrl().startsWith("http://"),
            "worker does not advertise an HTTP(S) corresponding-source location"
        );
        SteelNoiseArtifact.require(capabilities.getSourceSha256().matches("[0-9a-f]{64}"), "worker source digest is invalid");
        SteelNoiseArtifact.require(
            capabilities.getLicenseExpression().equals("AGPL-3.0-or-later"),
            "worker license expression is unsupported"
        );
        for (final String identity : List.of(
            capabilities.getExternalBuildId(),
            capabilities.getRustcId(),
            capabilities.getCargoId(),
            capabilities.getBuildTarget(),
            capabilities.getBuildConfiguration()
        )) {
            SteelNoiseArtifact.require(
                !identity.isEmpty()
                    && identity.length() <= 4096
                    && identity.chars().allMatch(character -> character >= 0x20 && character <= 0x7e),
                "worker build attestation contains invalid text"
            );
        }
        SteelNoiseArtifact.require(!capabilities.getSteelResumable(), "V1 importer requires non-resumable fresh artifacts");
        SteelNoiseArtifact.require(!capabilities.getSupportsBlending(), "V1 importer requires a fresh-only worker profile");
        SteelNoiseArtifact.require(!capabilities.getSupportsRetrogen(), "V1 importer requires a fresh-only worker profile");
        SteelNoiseArtifact.require(
            HexFormat.of().formatHex(capabilities.getProfileSha256().toByteArray()).equals(settings.expectedProfileSha256),
            "worker profile fingerprint does not match configuration"
        );
        SteelNoiseArtifact.require(
            MessageDigest.isEqual(localRegistrySha256, capabilities.getRegistrySha256().toByteArray()),
            "Folia block-state/biome registry fingerprint does not match the worker"
        );
    }

    static byte[] canonicalRequestSha256(final GenerateRequest request) {
        final MessageDigest digest = newSha256();
        digest.update(REQUEST_DOMAIN);
        putU16Bytes(digest, request.getMinecraftVersion());
        putU16Bytes(digest, request.getDimensionKey());
        digest.update(ByteBuffer.allocate(Long.BYTES).putLong(request.getSeed()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getChunkX()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getChunkZ()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getMinY()).array());
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(request.getHeight()).array());
        digest.update((byte)request.getFirstStageValue());
        digest.update((byte)request.getLastStageValue());
        digest.update(request.getExpectedGeneratorSha256().toByteArray());
        digest.update(request.getExpectedRegistrySha256().toByteArray());
        digest.update(new byte[] {0, 0});
        return digest.digest();
    }

    private static byte[] registryFingerprint(final MinecraftServer server) {
        final Registry<Block> blocks = server.registryAccess().lookupOrThrow(Registries.BLOCK);
        final List<StateFingerprint> states = new ArrayList<>();
        for (final Map.Entry<ResourceKey<Block>, Block> entry : blocks.entrySet()) {
            final String name = entry.getKey().identifier().toString();
            for (final BlockState state : entry.getValue().getStateDefinition().getPossibleStates()) {
                final List<PropertyFingerprint> properties = state.getValues()
                    .map(value -> new PropertyFingerprint(value.property().getName(), value.valueName()))
                    .sorted(PropertyFingerprint.COMPARATOR)
                    .toList();
                states.add(new StateFingerprint(name, properties));
            }
        }
        states.sort(StateFingerprint.COMPARATOR);
        for (int index = 1; index < states.size(); index++) {
            SteelNoiseArtifact.require(
                StateFingerprint.COMPARATOR.compare(states.get(index - 1), states.get(index)) < 0,
                "duplicate canonical block state in Folia registry"
            );
        }

        final Registry<Biome> biomeRegistry = server.registryAccess().lookupOrThrow(Registries.BIOME);
        final List<String> biomes = biomeRegistry.keySet().stream().map(Identifier::toString).sorted().toList();
        final MessageDigest digest = newSha256();
        putU64Bytes(digest, REGISTRY_DOMAIN);
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(states.size()).array());
        for (final StateFingerprint state : states) {
            putU64Bytes(digest, state.name.getBytes(StandardCharsets.UTF_8));
            digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(state.properties.size()).array());
            for (final PropertyFingerprint property : state.properties) {
                putU64Bytes(digest, property.name.getBytes(StandardCharsets.UTF_8));
                putU64Bytes(digest, property.value.getBytes(StandardCharsets.UTF_8));
            }
        }
        digest.update(ByteBuffer.allocate(Integer.BYTES).putInt(biomes.size()).array());
        for (final String biome : biomes) {
            putU64Bytes(digest, biome.getBytes(StandardCharsets.UTF_8));
        }
        return digest.digest();
    }

    private static void putU16Bytes(final MessageDigest digest, final String value) {
        final byte[] bytes = value.getBytes(StandardCharsets.UTF_8);
        SteelNoiseArtifact.require(bytes.length <= 0xffff, "canonical request string exceeds u16 length");
        digest.update(ByteBuffer.allocate(Short.BYTES).putShort((short)bytes.length).array());
        digest.update(bytes);
    }

    private static void putU64Bytes(final MessageDigest digest, final byte[] value) {
        digest.update(ByteBuffer.allocate(Long.BYTES).putLong(value.length).array());
        digest.update(value);
    }

    private static byte[] uuidBytes(final UUID uuid) {
        return ByteBuffer.allocate(16).putLong(uuid.getMostSignificantBits()).putLong(uuid.getLeastSignificantBits()).array();
    }

    private static byte[] sha256(final byte[] value) {
        return newSha256().digest(value);
    }

    private static MessageDigest newSha256() {
        try {
            return MessageDigest.getInstance("SHA-256");
        } catch (final NoSuchAlgorithmException exception) {
            throw new IllegalStateException("JVM has no SHA-256 provider", exception);
        }
    }

    private static Identifier canonicalIdentifier(final String text, final String description) {
        final Identifier identifier = Identifier.tryParse(text);
        SteelNoiseArtifact.require(
            text.length() <= 256 && text.indexOf(':') > 0 && identifier != null && identifier.toString().equals(text),
            description + " is not an explicitly namespaced canonical identifier"
        );
        return identifier;
    }

    @Override
    public void close() {
        if (!this.closed.compareAndSet(false, true)) {
            return;
        }
        for (final RequestContext context : List.copyOf(this.requests.values())) {
            this.cancelRequest(context.key);
        }
        this.preAdmissionCancellations.clear();
        this.importerExecutor.shutdownNow();
        this.channel.shutdown();
        final io.grpc.netty.shaded.io.netty.util.concurrent.Future<?> eventLoopTermination =
            this.eventLoopGroup.shutdownGracefully(0, 5, TimeUnit.SECONDS);
        try {
            this.importerExecutor.awaitTermination(5, TimeUnit.SECONDS);
            if (!this.channel.awaitTermination(5, TimeUnit.SECONDS)) {
                this.channel.shutdownNow();
                this.channel.awaitTermination(5, TimeUnit.SECONDS);
            }
            eventLoopTermination.await(5, TimeUnit.SECONDS);
        } catch (final InterruptedException exception) {
            Thread.currentThread().interrupt();
            this.channel.shutdownNow();
        }
    }

    public enum CancellationReservationResult {
        PENDING,
        CANCELLED,
        TERMINAL,
        NOT_FOUND,
    }

    public static final class CancellationReservation {
        private final SteelRemoteNoise owner;
        private final ChunkKey key;
        private volatile CancellationReservationResult result = CancellationReservationResult.PENDING;

        private CancellationReservation(final SteelRemoteNoise owner, final ChunkKey key) {
            this.owner = owner;
            this.key = key;
        }

        public CancellationReservationResult result() {
            return this.result;
        }
    }

    static final class ImportCommitGate {
        private boolean cancelled;
        private boolean committed;

        synchronized boolean isCancelled() {
            return this.cancelled;
        }

        synchronized CancellationResult tryCancel(final java.util.function.BooleanSupplier terminal) {
            if (this.committed || terminal.getAsBoolean()) {
                return CancellationResult.TERMINAL;
            }
            this.cancelled = true;
            return CancellationResult.CANCELLED;
        }

        synchronized <T> T commit(final java.util.function.Supplier<T> action) {
            if (this.cancelled) {
                throw new CancellationException("Steel remote NOISE request was cancelled before commit");
            }
            if (this.committed) {
                throw new IllegalStateException("Steel remote NOISE import was already committed");
            }
            final T result = action.get();
            this.committed = true;
            return result;
        }
    }

    private static final class RequestContext {
        private final ChunkKey key;
        private final ProtoChunk center;
        private final GenerateRequest request;
        private final byte[] canonicalRequestSha256;
        private final CompletableFuture<ChunkAccess> result = new CompletableFuture<>();
        private final ImportCommitGate commitGate = new ImportCommitGate();
        private final AtomicBoolean cancelSent = new AtomicBoolean();
        private final AtomicBoolean finished = new AtomicBoolean();
        private volatile ListenableFuture<GenerateResponse> call;
        private volatile boolean queued;
        private volatile boolean activeSlot;

        private RequestContext(
            final ChunkKey key,
            final ProtoChunk center,
            final GenerateRequest request,
            final byte[] canonicalRequestSha256
        ) {
            this.key = key;
            this.center = center;
            this.request = request;
            this.canonicalRequestSha256 = canonicalRequestSha256;
        }

        private Throwable failure(final Throwable throwable) {
            if (!this.commitGate.isCancelled() || throwable instanceof CancellationException) {
                return throwable;
            }
            final CancellationException cancellation = new CancellationException("Steel remote NOISE request was cancelled");
            cancellation.initCause(throwable);
            return cancellation;
        }
    }

    private static final class ChunkKey {
        private final ServerLevel level;
        private final int chunkX;
        private final int chunkZ;

        private ChunkKey(final ServerLevel level, final int chunkX, final int chunkZ) {
            this.level = level;
            this.chunkX = chunkX;
            this.chunkZ = chunkZ;
        }

        @Override
        public boolean equals(final Object other) {
            return other instanceof ChunkKey key
                && this.level == key.level
                && this.chunkX == key.chunkX
                && this.chunkZ == key.chunkZ;
        }

        @Override
        public int hashCode() {
            int hash = System.identityHashCode(this.level);
            hash = 31 * hash + this.chunkX;
            return 31 * hash + this.chunkZ;
        }
    }

    private record PropertyFingerprint(String name, String value) {
        private static final Comparator<PropertyFingerprint> COMPARATOR = Comparator
            .comparing(PropertyFingerprint::name)
            .thenComparing(PropertyFingerprint::value);
    }

    private record StateFingerprint(String name, List<PropertyFingerprint> properties) {
        private static final Comparator<StateFingerprint> COMPARATOR = (left, right) -> {
            final int name = left.name.compareTo(right.name);
            if (name != 0) {
                return name;
            }
            final int common = Math.min(left.properties.size(), right.properties.size());
            for (int index = 0; index < common; index++) {
                final int property = PropertyFingerprint.COMPARATOR.compare(left.properties.get(index), right.properties.get(index));
                if (property != 0) {
                    return property;
                }
            }
            return Integer.compare(left.properties.size(), right.properties.size());
        };
    }
}
