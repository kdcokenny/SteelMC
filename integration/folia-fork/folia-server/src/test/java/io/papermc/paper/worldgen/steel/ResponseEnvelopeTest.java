package io.papermc.paper.worldgen.steel;

import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import dev.steelmc.worldgen.protocol.v1.Capabilities;
import dev.steelmc.worldgen.protocol.v1.Compression;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.GenerateResponse;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.function.UnaryOperator;
import org.bukkit.support.environment.Normal;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

@Normal
class ResponseEnvelopeTest {
    private static final ByteString REQUEST_ID = ByteString.copyFrom(new byte[16]);
    private static final byte[] CANONICAL = filled(1);
    private static final ByteString GENERATOR = ByteString.copyFrom(filled(2));
    private static final ByteString REGISTRY = ByteString.copyFrom(filled(3));
    private static final GenerateRequest REQUEST = GenerateRequest.newBuilder().setRequestId(REQUEST_ID).build();
    private static final Capabilities CAPABILITIES = Capabilities.newBuilder()
        .setGeneratorSha256(GENERATOR)
        .setRegistrySha256(REGISTRY)
        .build();

    @Test
    void validEnvelopeIsParsedOnlyAfterAllIdentityChecks() throws Exception {
        assertEquals(
            0,
            SteelRemoteNoise.validateAndParseResponse(valid(ByteString.EMPTY).build(), REQUEST, CANONICAL, CAPABILITIES)
                .getArtifactVersion()
        );
    }

    @Test
    void rejectsMalformedAndOversizedArtifactsBeforeImport() {
        final ByteString malformed = ByteString.copyFrom(new byte[]{(byte)0x80});
        assertThrows(
            InvalidProtocolBufferException.class,
            () -> SteelRemoteNoise.validateAndParseResponse(valid(malformed).build(), REQUEST, CANONICAL, CAPABILITIES)
        );
        final ByteString oversized = ByteString.copyFrom(new byte[8 * 1024 * 1024 + 1]);
        assertInvalid("artifact exceeds client size limit", ignored -> valid(oversized));
    }

    @Test
    void rejectsEveryResponseEnvelopeIdentityAndEncodingMismatch() {
        assertInvalid("artifact size mismatch", builder -> builder.setUncompressedSize(1));
        assertInvalid("artifact SHA-256 mismatch", builder -> builder.setArtifactSha256(ByteString.copyFrom(filled(9))));
        assertInvalid("response request id mismatch", builder -> builder.setRequestId(ByteString.copyFrom(new byte[]{1})));
        assertInvalid("response canonical request digest mismatch", builder -> builder.setCanonicalRequestSha256(ByteString.copyFrom(filled(9))));
        assertInvalid("response generator fingerprint mismatch", builder -> builder.setGeneratorSha256(ByteString.copyFrom(filled(9))));
        assertInvalid("response registry fingerprint mismatch", builder -> builder.setRegistrySha256(ByteString.copyFrom(filled(9))));
        assertInvalid("response artifact version mismatch", builder -> builder.setArtifactVersion(2));
        assertInvalid("unsupported application compression", builder -> builder.setCompression(Compression.COMPRESSION_ZSTD));
    }

    private static void assertInvalid(
        final String expected,
        final UnaryOperator<GenerateResponse.Builder> mutation
    ) {
        final IllegalStateException error = assertThrows(
            IllegalStateException.class,
            () -> SteelRemoteNoise.validateAndParseResponse(
                mutation.apply(valid(ByteString.EMPTY)).build(), REQUEST, CANONICAL, CAPABILITIES
            )
        );
        assertTrue(error.getMessage().contains(expected), error::getMessage);
    }

    private static GenerateResponse.Builder valid(final ByteString artifact) {
        return GenerateResponse.newBuilder()
            .setRequestId(REQUEST_ID)
            .setCanonicalRequestSha256(ByteString.copyFrom(CANONICAL))
            .setGeneratorSha256(GENERATOR)
            .setRegistrySha256(REGISTRY)
            .setArtifactVersion(1)
            .setCompression(Compression.COMPRESSION_NONE)
            .setUncompressedSize(artifact.size())
            .setArtifactSha256(ByteString.copyFrom(sha256(artifact.toByteArray())))
            .setArtifact(artifact);
    }

    private static byte[] sha256(final byte[] value) {
        try {
            return MessageDigest.getInstance("SHA-256").digest(value);
        } catch (final NoSuchAlgorithmException exception) {
            throw new AssertionError(exception);
        }
    }

    private static byte[] filled(final int value) {
        final byte[] bytes = new byte[32];
        java.util.Arrays.fill(bytes, (byte)value);
        return bytes;
    }
}
