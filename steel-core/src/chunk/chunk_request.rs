//! Ticket-owned chunk availability requests.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use rustc_hash::FxHashSet;
use steel_utils::ChunkPos;
use tokio::time::sleep;

use crate::chunk::{
    chunk_holder::ChunkHolder,
    chunk_map::ChunkMap,
    chunk_scheduler::ChunkTicketRevision,
    chunk_ticket_manager::{ChunkTicket, ticket_level_for_status},
    status::ChunkStatus,
};

/// Why a chunk request is holding tickets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkTicketKind {
    /// Player-visible chunk loading.
    Player,
    /// Initial chunks around a joining player's spawn.
    PlayerSpawn,
    /// Candidate chunks loaded while searching for a valid spawn position.
    SpawnSearch,
    /// Candidate chunks loaded by structure location queries.
    StructureLocate,
    /// Chunks loaded by startup pregeneration.
    Pregen,
    /// Generic command-owned chunk request.
    Command,
    /// Chunks loaded while preparing a portal destination.
    Portal,
}

/// Request for a set of chunks at a minimum generation status.
pub struct ChunkRequest {
    /// Minimum chunk status required before the request is ready.
    pub status: ChunkStatus,
    /// Chunk positions requested.
    pub positions: Vec<ChunkPos>,
    /// Ticket owner category.
    pub ticket_kind: ChunkTicketKind,
}

/// Poll result for a [`ChunkRequestHandle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkRequestState {
    /// The request has outstanding chunks.
    Pending {
        /// Number of requested chunks already at the target status.
        ready: usize,
        /// Number of requested chunks after deduplication.
        total: usize,
    },
    /// Every requested chunk is available at the target status.
    Ready,
    /// The request was cancelled and no longer owns tickets.
    Cancelled,
}

/// Chunks passed to request continuations once a request is ready.
pub struct ReadyChunks {
    /// Status all holders have reached.
    pub status: ChunkStatus,
    /// Holders for the requested positions.
    pub holders: Vec<Arc<ChunkHolder>>,
}

// Per-holder baseline distinguishes a new failure from a retry of an old failed attempt.
struct GenerationFailureBaseline {
    holder: Weak<ChunkHolder>,
    revision: u64,
}

struct ChunkRequestInner {
    chunk_map: Arc<ChunkMap>,
    positions: Box<[ChunkPos]>,
    status: ChunkStatus,
    ticket_kind: ChunkTicketKind,
    ticket: ChunkTicket,
    submission_revision: Option<ChunkTicketRevision>,
    generation_failure_baselines: Box<[GenerationFailureBaseline]>,
}

/// Handle for a ticketed chunk request.
///
/// Dropping or cancelling the handle releases its tickets. The handle never
/// creates chunk holders directly; lifecycle publication remains owned by the
/// game-tick boundary.
pub struct ChunkRequestHandle {
    inner: Option<ChunkRequestInner>,
}

impl ChunkRequestHandle {
    pub(crate) fn new(chunk_map: Arc<ChunkMap>, request: ChunkRequest) -> Self {
        let ticket = ChunkTicket::loading(ticket_level_for_status(request.status));
        Self::new_with_ticket(chunk_map, request, ticket)
    }

    fn new_with_ticket(
        chunk_map: Arc<ChunkMap>,
        request: ChunkRequest,
        ticket: ChunkTicket,
    ) -> Self {
        let positions = dedupe_positions(request.positions);
        let generation_failure_baselines = positions
            .iter()
            .map(|pos| {
                chunk_map
                    .chunks
                    .read_sync(pos, |_, holder| GenerationFailureBaseline {
                        holder: Arc::downgrade(holder),
                        revision: holder.generation_failure_revision(),
                    })
                    .unwrap_or_else(|| GenerationFailureBaseline {
                        holder: Weak::new(),
                        revision: 0,
                    })
            })
            .collect();
        let submission_revision = chunk_map.add_chunk_tickets(&positions, ticket);

        Self {
            inner: Some(ChunkRequestInner {
                chunk_map,
                positions,
                status: request.status,
                ticket_kind: request.ticket_kind,
                ticket,
                submission_revision,
                generation_failure_baselines,
            }),
        }
    }

    /// Returns the requested status, if this handle is still active.
    #[must_use]
    pub fn status(&self) -> Option<ChunkStatus> {
        self.inner.as_ref().map(|inner| inner.status)
    }

    /// Returns the ticket kind, if this handle is still active.
    #[must_use]
    pub fn ticket_kind(&self) -> Option<ChunkTicketKind> {
        self.inner.as_ref().map(|inner| inner.ticket_kind)
    }

    /// Returns requested positions after deduplication.
    #[must_use]
    pub fn positions(&self) -> &[ChunkPos] {
        self.inner
            .as_ref()
            .map_or(&[], |inner| inner.positions.as_ref())
    }

    /// Polls request readiness. Chunk holder creation and generation scheduling
    /// are owned by the chunk scheduling epochs.
    #[must_use]
    pub fn poll(&self) -> ChunkRequestState {
        let Some(inner) = &self.inner else {
            return ChunkRequestState::Cancelled;
        };
        if inner.positions.is_empty() {
            return ChunkRequestState::Ready;
        }
        let ticket_revision_committed = inner
            .submission_revision
            .is_none_or(|revision| inner.chunk_map.is_ticket_revision_committed(revision));

        let mut ready = 0;
        for &pos in &inner.positions {
            let Some(holder) = inner
                .chunk_map
                .chunks
                .read_sync(&pos, |_, holder| holder.clone())
            else {
                continue;
            };

            if holder.try_chunk(inner.status).is_some() {
                ready += 1;
            }
        }

        if ticket_revision_committed && ready == inner.positions.len() {
            ChunkRequestState::Ready
        } else {
            ChunkRequestState::Pending {
                ready,
                total: inner.positions.len(),
            }
        }
    }

    /// Returns holders once every requested chunk is at the target status.
    #[must_use]
    pub fn ready_chunks(&self) -> Option<ReadyChunks> {
        let inner = self.inner.as_ref()?;
        if inner
            .submission_revision
            .is_some_and(|revision| !inner.chunk_map.is_ticket_revision_committed(revision))
        {
            return None;
        }
        let mut holders = Vec::with_capacity(inner.positions.len());

        for &pos in &inner.positions {
            let holder = inner
                .chunk_map
                .chunks
                .read_sync(&pos, |_, holder| holder.clone())?;
            {
                let _chunk = holder.try_chunk(inner.status)?;
            }
            holders.push(holder);
        }

        Some(ReadyChunks {
            status: inner.status,
            holders,
        })
    }

    /// Drives offline chunk scheduling until every requested chunk is ready.
    ///
    /// This is intended for headless generation tools that do not have a gameplay
    /// tick boundary. Live servers must continue to publish scheduler epochs from
    /// their normal lifecycle boundary instead. Cancelling this future leaves the
    /// handle active; dropping or explicitly cancelling the handle releases its tickets.
    pub async fn wait_ready_offline(&self) -> Option<ReadyChunks> {
        loop {
            let inner = self.inner.as_ref()?;
            let generation_failed = inner.positions.iter().enumerate().any(|(index, pos)| {
                inner
                    .chunk_map
                    .chunks
                    .read_sync(pos, |_, holder| {
                        let baseline = &inner.generation_failure_baselines[index];
                        let same_holder = baseline
                            .holder
                            .upgrade()
                            .is_some_and(|original| Arc::ptr_eq(&original, holder));
                        let revision = holder.generation_failure_revision();
                        if same_holder {
                            baseline.revision != 0 || revision != baseline.revision
                        } else {
                            revision != 0
                        }
                    })
                    .unwrap_or(false)
            });
            if generation_failed {
                return None;
            }
            if let Some(chunks) = self.ready_chunks() {
                return Some(chunks);
            }

            inner.chunk_map.advance_scheduling();
            sleep(Duration::from_millis(1)).await;
        }
    }

    /// Cancels the request and releases its tickets.
    pub fn cancel(&mut self) {
        self.release_tickets();
    }

    fn release_tickets(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };

        let _ = inner
            .chunk_map
            .remove_chunk_tickets(&inner.positions, inner.ticket);
    }
}

impl Drop for ChunkRequestHandle {
    fn drop(&mut self) {
        self.release_tickets();
    }
}

impl ChunkMap {
    /// Adds tickets for a chunk request and returns a pollable handle.
    ///
    /// The returned handle owns the tickets. Holder creation and generation
    /// scheduling are handled by chunk scheduling epochs.
    #[must_use]
    pub fn request_chunks(self: &Arc<Self>, request: ChunkRequest) -> ChunkRequestHandle {
        ChunkRequestHandle::new(self.clone(), request)
    }

    /// Requests one chunk at `status`.
    #[must_use]
    pub fn request_chunk(
        self: &Arc<Self>,
        pos: ChunkPos,
        status: ChunkStatus,
        ticket_kind: ChunkTicketKind,
    ) -> ChunkRequestHandle {
        self.request_chunks(ChunkRequest {
            status,
            positions: vec![pos],
            ticket_kind,
        })
    }

    /// Requests a square of chunks centered on `center`.
    #[must_use]
    pub fn request_square(
        self: &Arc<Self>,
        center: ChunkPos,
        radius: u8,
        status: ChunkStatus,
        ticket_kind: ChunkTicketKind,
    ) -> ChunkRequestHandle {
        let radius = i32::from(radius);
        let diameter = radius * 2 + 1;
        let capacity = (diameter * diameter) as usize;
        let mut positions = Vec::with_capacity(capacity);

        for dz in -radius..=radius {
            for dx in -radius..=radius {
                positions.push(ChunkPos::new(center.0.x + dx, center.0.y + dz));
            }
        }

        self.request_chunks(ChunkRequest {
            status,
            positions,
            ticket_kind,
        })
    }
}

fn dedupe_positions(positions: Vec<ChunkPos>) -> Box<[ChunkPos]> {
    let mut seen = FxHashSet::default();
    let mut deduped = Vec::with_capacity(positions.len());
    for pos in positions {
        if seen.insert(pos) {
            deduped.push(pos);
        }
    }
    deduped.into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fresh_test_world;
    use std::{sync::mpsc::sync_channel, thread, time::Duration};
    use tokio::{runtime::Builder, time::timeout};

    fn drive_request_until_ready(chunk_map: &Arc<ChunkMap>, request: &ChunkRequestHandle) {
        for _ in 0..10_000 {
            chunk_map.advance_scheduling();
            if request.poll() == ChunkRequestState::Ready {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("chunk request did not become ready");
    }

    #[test]
    fn dedupe_positions_preserves_first_occurrence_order() {
        let positions = dedupe_positions(vec![
            ChunkPos::new(1, 2),
            ChunkPos::new(3, 4),
            ChunkPos::new(1, 2),
        ]);
        assert_eq!(&*positions, &[ChunkPos::new(1, 2), ChunkPos::new(3, 4)]);
    }

    #[test]
    fn wait_ready_offline_succeeds_and_cancelled_handle_returns_none() {
        let world = fresh_test_world("chunk_request_wait_offline");
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("offline wait runtime should start");
        let pos = ChunkPos::new(20, -20);
        let request =
            world
                .chunk_map
                .request_chunk(pos, ChunkStatus::Empty, ChunkTicketKind::Pregen);
        let ready = runtime
            .block_on(async {
                timeout(Duration::from_secs(10), request.wait_ready_offline()).await
            })
            .expect("offline wait timed out")
            .expect("empty generator request unexpectedly failed");
        assert_eq!(ready.status, ChunkStatus::Empty);
        assert_eq!(ready.holders.len(), 1);
        ready.holders[0].record_generation_failure_for_test();
        let quarantined =
            world
                .chunk_map
                .request_chunk(pos, ChunkStatus::Empty, ChunkTicketKind::Pregen);
        assert!(
            runtime.block_on(quarantined.wait_ready_offline()).is_none(),
            "ready data from a failed holder must not bypass quarantine"
        );

        let mut cancelled = world.chunk_map.request_chunk(
            ChunkPos::new(21, -20),
            ChunkStatus::Empty,
            ChunkTicketKind::Pregen,
        );
        cancelled.cancel();
        assert!(runtime.block_on(cancelled.wait_ready_offline()).is_none());
        drop((quarantined, request, cancelled));
        world.chunk_map.stop_generation_refill_loop();
        world.chunk_map.task_tracker.close();
        runtime.block_on(world.chunk_map.task_tracker.wait());
    }

    #[test]
    fn wait_ready_offline_quarantines_failed_holder() {
        let world = fresh_test_world("chunk_request_wait_retry");
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("offline retry runtime should start");
        let (started_sender, started_receiver) = sync_channel(0);
        let (release_sender, release_receiver) = sync_channel(0);
        world.chunk_map.generation_pool.spawn(move || {
            started_sender
                .send(())
                .expect("test should still await the occupied generation pool");
            let _ = release_receiver.recv();
        });
        started_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("failed to occupy the generation pool");

        let pos = ChunkPos::new(22, -20);
        let failed =
            world
                .chunk_map
                .request_chunk(pos, ChunkStatus::Noise, ChunkTicketKind::Pregen);
        let mut holder = None;
        for _ in 0..100 {
            world.chunk_map.advance_scheduling();
            holder = world
                .chunk_map
                .chunks
                .read_sync(&pos, |_, holder| Arc::clone(holder));
            if holder.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let holder = holder.expect("ticket scheduling should create the requested holder");
        holder.cancel_generation_task();
        holder.record_generation_failure_for_test();
        assert!(runtime.block_on(failed.wait_ready_offline()).is_none());
        drop(failed);
        release_sender
            .send(())
            .expect("occupied generation task should still be waiting");

        let retry = world
            .chunk_map
            .request_chunk(pos, ChunkStatus::Noise, ChunkTicketKind::Pregen);
        assert!(
            runtime.block_on(retry.wait_ready_offline()).is_none(),
            "a partially mutated failed holder must remain quarantined"
        );
        assert!(
            world
                .chunk_map
                .chunks
                .read_sync(&pos, |_, current| Arc::ptr_eq(current, &holder))
                .unwrap_or(false)
        );
        drop(retry);
        world.chunk_map.stop_generation_refill_loop();
        world.chunk_map.task_tracker.close();
        runtime.block_on(world.chunk_map.task_tracker.wait());
    }

    #[test]
    fn ready_chunk_still_waits_for_its_ticket_revision_to_commit() {
        let world = fresh_test_world("chunk_request_revision");
        let pos = ChunkPos::new(4, -7);
        let first =
            world
                .chunk_map
                .request_chunk(pos, ChunkStatus::Empty, ChunkTicketKind::Command);
        drive_request_until_ready(&world.chunk_map, &first);

        let second =
            world
                .chunk_map
                .request_chunk(pos, ChunkStatus::Empty, ChunkTicketKind::Command);

        assert_eq!(
            second.poll(),
            ChunkRequestState::Pending { ready: 1, total: 1 }
        );
        assert!(second.ready_chunks().is_none());

        drive_request_until_ready(&world.chunk_map, &second);
        assert!(second.ready_chunks().is_some());
    }
}
