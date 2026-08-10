package dev.steelmc.worldgen;

import dev.steelmc.worldgen.protocol.v1.PackedPalette;
import java.util.BitSet;

final class PaletteCodec {
    private PaletteCodec() {
    }

    static BitSet validate(PackedPalette palette, int volume, int dictionarySize) {
        int entries = palette.getEntriesCount();
        require(entries >= 1, "palette is empty");
        for (int index = 0; index < entries; index++) {
            int value = palette.getEntries(index);
            require(value >= 0 && value < dictionarySize, "palette dictionary index out of bounds");
            if (index != 0) {
                require(palette.getEntries(index - 1) < value, "palette is not strictly sorted");
            }
        }
        int expectedBits = entries == 1
            ? 0
            : Integer.SIZE - Integer.numberOfLeadingZeros(entries - 1);
        require(palette.getBitsPerEntry() == expectedBits, "palette width is not canonical");
        long totalBits = (long) volume * expectedBits;
        int expectedBytes = Math.toIntExact(Math.ceilDiv(totalBits, 8));
        require(palette.getData().size() == expectedBytes, "packed palette has wrong length");
        int trailingBits = (int) (totalBits % 8);
        if (trailingBits != 0) {
            int validMask = (1 << trailingBits) - 1;
            int last = Byte.toUnsignedInt(palette.getData().byteAt(expectedBytes - 1));
            require((last & ~validMask) == 0, "packed palette has nonzero padding bits");
        }
        BitSet usedLocalEntries = new BitSet(entries);
        BitSet usedDictionaryEntries = new BitSet(dictionarySize);
        for (int index = 0; index < volume; index++) {
            int local = unpack(palette, index);
            require(local >= 0 && local < entries, "packed palette index out of bounds");
            usedLocalEntries.set(local);
            usedDictionaryEntries.set(palette.getEntries(local));
        }
        require(usedLocalEntries.cardinality() == entries, "palette contains an unused entry");
        return usedDictionaryEntries;
    }

    static int unpack(PackedPalette palette, int index) {
        int width = palette.getBitsPerEntry();
        if (width == 0) {
            return 0;
        }
        int startBit = Math.multiplyExact(index, width);
        int value = 0;
        for (int bit = 0; bit < width; bit++) {
            int sourceBit = startBit + bit;
            int sourceByte = Byte.toUnsignedInt(palette.getData().byteAt(sourceBit >>> 3));
            value |= ((sourceByte >>> (sourceBit & 7)) & 1) << bit;
        }
        return value;
    }

    private static void require(boolean condition, String message) {
        if (!condition) {
            throw new IllegalStateException(message);
        }
    }
}
