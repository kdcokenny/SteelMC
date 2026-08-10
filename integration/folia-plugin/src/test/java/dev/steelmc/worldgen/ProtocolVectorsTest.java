package dev.steelmc.worldgen;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import com.google.protobuf.ByteString;
import dev.steelmc.worldgen.protocol.v1.Compression;
import dev.steelmc.worldgen.protocol.v1.GenerateRequest;
import dev.steelmc.worldgen.protocol.v1.GenerationContext;
import dev.steelmc.worldgen.protocol.v1.PackedPalette;
import dev.steelmc.worldgen.protocol.v1.Stage;
import java.util.HexFormat;
import org.junit.jupiter.api.Test;

final class ProtocolVectorsTest {
    @Test
    void canonicalRequestMatchesRustVector() {
        GenerateRequest request = GenerateRequest.newBuilder()
            .setRequestId(ByteString.copyFrom(new byte[16]))
            .setMinecraftVersion("26.2")
            .setProfileId("ignored-by-canonical-key")
            .setDimensionKey("minecraft:overworld")
            .setSeed(13_579)
            .setChunkX(0)
            .setChunkZ(0)
            .setMinY(-64)
            .setHeight(384)
            .setFirstStage(Stage.STAGE_BIOMES)
            .setLastStage(Stage.STAGE_NOISE)
            .setExpectedGeneratorSha256(ByteString.copyFrom(new byte[32]))
            .setExpectedRegistrySha256(ByteString.copyFrom(HexFormat.of().parseHex("ff".repeat(32))))
            .addAcceptedCompression(Compression.COMPRESSION_NONE)
            .setGenerationContext(GenerationContext.GENERATION_CONTEXT_FRESH)
            .build();

        assertEquals(
            "d63f74fb044c0c93fbd48b1fdca3a4ef20c81d6b6e51b4b78d5cc0462c2c1c68",
            HexFormat.of().formatHex(RemoteClient.canonicalRequestSha256(request))
        );
    }

    @Test
    void crossBytePaletteMatchesRustVector() {
        PackedPalette palette = PackedPalette.newBuilder()
            .addAllEntries(java.util.List.of(0, 1, 2, 3, 4, 5, 6, 7))
            .setBitsPerEntry(3)
            .setData(ByteString.copyFrom(new byte[] {(byte) 0x88, (byte) 0xc6, (byte) 0xfa, 0}))
            .build();

        PaletteCodec.validate(palette, 9, 8);
        int[] actual = new int[9];
        for (int index = 0; index < actual.length; index++) {
            actual[index] = PaletteCodec.unpack(palette, index);
        }
        assertArrayEquals(new int[] {0, 1, 2, 3, 4, 5, 6, 7, 0}, actual);
    }

    @Test
    void unusedPaletteEntryIsRejected() {
        PackedPalette palette = PackedPalette.newBuilder()
            .addEntries(0)
            .addEntries(1)
            .setBitsPerEntry(1)
            .setData(ByteString.copyFrom(new byte[] {0}))
            .build();

        assertThrows(IllegalStateException.class, () -> PaletteCodec.validate(palette, 2, 2));
    }

    @Test
    void nonzeroPalettePaddingIsRejected() {
        PackedPalette palette = PackedPalette.newBuilder()
            .addEntries(0)
            .addEntries(1)
            .setBitsPerEntry(1)
            .setData(ByteString.copyFrom(new byte[] {(byte) 0xfe}))
            .build();

        assertThrows(IllegalStateException.class, () -> PaletteCodec.validate(palette, 1, 2));
    }

    @Test
    void malformedPaletteIndexIsRejected() {
        PackedPalette palette = PackedPalette.newBuilder()
            .addEntries(0)
            .addEntries(1)
            .addEntries(2)
            .setBitsPerEntry(2)
            .setData(ByteString.copyFrom(new byte[] {(byte) 0xff}))
            .build();

        assertThrows(IllegalStateException.class, () -> PaletteCodec.validate(palette, 4, 3));
    }
}
