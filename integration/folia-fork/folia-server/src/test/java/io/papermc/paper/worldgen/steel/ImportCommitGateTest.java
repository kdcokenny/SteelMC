package io.papermc.paper.worldgen.steel;

import java.util.concurrent.CancellationException;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import org.bukkit.support.environment.Normal;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

@Normal
class ImportCommitGateTest {
    @Test
    void cancellationBeforeCommitSuppressesMutation() {
        final SteelRemoteNoise.ImportCommitGate gate = new SteelRemoteNoise.ImportCommitGate();
        final AtomicBoolean mutated = new AtomicBoolean();

        assertEquals(SteelRemoteNoise.CancellationResult.CANCELLED, gate.tryCancel(() -> false));
        assertEquals(SteelRemoteNoise.CancellationResult.CANCELLED, gate.tryCancel(() -> false));
        assertTrue(gate.isCancelled());
        assertThrows(CancellationException.class, () -> gate.commit(() -> {
            mutated.set(true);
            return "unreachable";
        }));
        assertFalse(mutated.get());
    }

    @Test
    void inProgressCommitWinsConcurrentCancellation() throws Exception {
        final SteelRemoteNoise.ImportCommitGate gate = new SteelRemoteNoise.ImportCommitGate();
        final CountDownLatch commitEntered = new CountDownLatch(1);
        final CountDownLatch releaseCommit = new CountDownLatch(1);
        try (var executor = Executors.newThreadPerTaskExecutor(Thread.ofVirtual().factory())) {
            final var commit = executor.submit(() -> gate.commit(() -> {
                commitEntered.countDown();
                try {
                    assertTrue(releaseCommit.await(5, TimeUnit.SECONDS));
                } catch (final InterruptedException exception) {
                    throw new AssertionError(exception);
                }
                return 42;
            }));
            assertTrue(commitEntered.await(5, TimeUnit.SECONDS));
            final CountDownLatch cancellationStarted = new CountDownLatch(1);
            final var cancellation = executor.submit(() -> {
                cancellationStarted.countDown();
                return gate.tryCancel(() -> false);
            });
            assertTrue(cancellationStarted.await(5, TimeUnit.SECONDS));
            assertFalse(cancellation.isDone());
            releaseCommit.countDown();

            assertEquals(42, commit.get(5, TimeUnit.SECONDS));
            assertEquals(SteelRemoteNoise.CancellationResult.TERMINAL, cancellation.get(5, TimeUnit.SECONDS));
            assertFalse(gate.isCancelled());
            assertThrows(IllegalStateException.class, () -> gate.commit(() -> 43));
        }
    }
}
