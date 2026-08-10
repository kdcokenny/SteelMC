package io.papermc.paper.worldgen.steel;

import ca.spottedleaf.moonrise.patches.chunk_system.scheduling.task.ChunkUpgradeGenericStatusTask;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import org.bukkit.support.environment.Normal;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

@Normal
class AsyncStatusCompletionGateTest {
    @Test
    void cancellationBeforeCompletionSuppressesPublication() {
        final ChunkUpgradeGenericStatusTask.CompletionGate gate = new ChunkUpgradeGenericStatusTask.CompletionGate();

        assertTrue(gate.requestCancellation());
        assertFalse(gate.requestCancellation());
        assertEquals(ChunkUpgradeGenericStatusTask.CompletionGate.CANCELLED, gate.claimCompletion());
        assertEquals(ChunkUpgradeGenericStatusTask.CompletionGate.COMPLETING, gate.claimCompletion());
    }

    @Test
    void completionBeforeCancellationPublishesExactlyOnce() {
        final ChunkUpgradeGenericStatusTask.CompletionGate gate = new ChunkUpgradeGenericStatusTask.CompletionGate();

        assertEquals(ChunkUpgradeGenericStatusTask.CompletionGate.ACTIVE, gate.claimCompletion());
        assertFalse(gate.requestCancellation());
        assertEquals(ChunkUpgradeGenericStatusTask.CompletionGate.COMPLETING, gate.claimCompletion());
    }

    @Test
    void cancellationAndCompletionHaveOneAtomicWinner() throws Exception {
        try (var executor = Executors.newThreadPerTaskExecutor(Thread.ofVirtual().factory())) {
            for (int iteration = 0; iteration < 1_000; iteration++) {
                final ChunkUpgradeGenericStatusTask.CompletionGate gate = new ChunkUpgradeGenericStatusTask.CompletionGate();
                final CountDownLatch start = new CountDownLatch(1);
                final var completion = executor.submit(() -> {
                    assertTrue(start.await(5, TimeUnit.SECONDS));
                    return gate.claimCompletion();
                });
                final var cancellation = executor.submit(() -> {
                    assertTrue(start.await(5, TimeUnit.SECONDS));
                    return gate.requestCancellation();
                });
                start.countDown();
                final int claimed = completion.get(5, TimeUnit.SECONDS);
                final boolean cancelled = cancellation.get(5, TimeUnit.SECONDS);
                assertTrue(
                    cancelled && claimed == ChunkUpgradeGenericStatusTask.CompletionGate.CANCELLED
                        || !cancelled && claimed == ChunkUpgradeGenericStatusTask.CompletionGate.ACTIVE
                );
            }
        }
    }
}
