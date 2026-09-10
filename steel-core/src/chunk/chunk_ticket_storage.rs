//! Authoritative chunk ticket source storage.

use std::cmp::Ordering;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use steel_registry::{REGISTRY, RegistryExt, ticket_type::TicketTypeRef, vanilla_ticket_types};
use steel_utils::saved_data::{SavedDataManager, names as saved_data_names};
use steel_utils::{ChunkPos, Identifier};
use thiserror::Error;

use super::{chunk_ticket::ChunkTicket, chunk_ticket_manager::ChunkTicketLevel};

pub(crate) const PORTAL_TICKET_RADIUS: u8 = 3;
pub(crate) const ENDER_PEARL_TICKET_TIMEOUT_TICKS: i64 =
    vanilla_ticket_types::ENDER_PEARL.timeout();
const ENDER_PEARL_TICKET_RADIUS: u8 = 2;

type StoredTickets = Vec<StoredChunkTicket>;

/// Persistent chunk ticket saved data.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistentChunkTickets {
    #[serde(default)]
    tickets: Vec<PersistentChunkTicket>,
}

// Decode each entry only after the complete TOML document has been parsed.
#[derive(Default, Deserialize)]
struct RawPersistentChunkTickets {
    #[serde(default)]
    tickets: Vec<toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistentChunkTicket {
    #[serde(rename = "type")]
    ticket_type: Identifier,
    chunk_x: i32,
    chunk_z: i32,
    level: u8,
    #[serde(default)]
    ticks_left: i64,
}

/// Invalid data for an individual persisted chunk ticket.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ChunkTicketStorageLoadError {
    #[error("unknown chunk ticket type `{0}`")]
    UnknownTicketType(Identifier),
    #[error("invalid chunk ticket level {level} for type `{ticket_type}`")]
    InvalidTicketLevel { ticket_type: Identifier, level: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredChunkTicket {
    ticket: ChunkTicket,
    timed_generation: Option<u64>,
}

impl StoredChunkTicket {
    const fn new(ticket: ChunkTicket, timed_generation: Option<u64>) -> Self {
        Self {
            ticket,
            timed_generation,
        }
    }

    fn to_persistent(self, pos: ChunkPos) -> Option<PersistentChunkTicket> {
        let ticket_type = self.ticket.ticket_type();
        ticket_type.persist().then(|| PersistentChunkTicket {
            // Saved data owns its registry identifier snapshot.
            ticket_type: ticket_type.key.clone(),
            chunk_x: pos.0.x,
            chunk_z: pos.0.y,
            level: self.ticket.ticket_level().raw(),
            ticks_left: self.ticket.ticks_left(),
        })
    }
}

/// One timed entry observed for the current world-tick expiration pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimedTicketExpiration {
    pos: ChunkPos,
    ticket: ChunkTicket,
    generation: u64,
}

impl TimedTicketExpiration {
    #[must_use]
    pub(crate) const fn pos(self) -> ChunkPos {
        self.pos
    }

    #[must_use]
    pub(crate) const fn can_expire_if_unloaded(self) -> bool {
        self.ticket.ticket_type().can_expire_if_unloaded()
    }
}

#[must_use]
const fn portal_ticket() -> ChunkTicket {
    ChunkTicket::for_full_chunk_radius(&vanilla_ticket_types::PORTAL, PORTAL_TICKET_RADIUS)
}

#[must_use]
const fn ender_pearl_ticket() -> ChunkTicket {
    ChunkTicket::for_full_chunk_radius(
        &vanilla_ticket_types::ENDER_PEARL,
        ENDER_PEARL_TICKET_RADIUS,
    )
}

/// One materialized source level for a propagation domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceLevelUpdate {
    pub(crate) pos: ChunkPos,
    pub(crate) level: Option<ChunkTicketLevel>,
}

/// Source positions dirtied by one or more storage mutations.
///
/// Positions are present only when canonical source membership changed and may
/// repeat when several mutations are merged.
#[must_use = "dirty source positions must be forwarded to the propagation domains"]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceProjectionChanges {
    pub(crate) load_positions: Vec<ChunkPos>,
    pub(crate) simulation_positions: Vec<ChunkPos>,
}

impl SourceProjectionChanges {
    fn for_ticket(pos: ChunkPos, ticket: ChunkTicket) -> Self {
        let mut changes = Self::default();
        if ticket.loading_level().is_some() {
            changes.load_positions.push(pos);
        }
        if ticket.simulation_level().is_some() {
            changes.simulation_positions.push(pos);
        }
        changes
    }

    pub(crate) fn merge(&mut self, mut other: Self) {
        self.load_positions.append(&mut other.load_positions);
        self.simulation_positions
            .append(&mut other.simulation_positions);
    }
}

/// Owns one logical ticket for each canonical type identity and level.
#[derive(Debug, Default)]
pub(crate) struct ChunkTicketStorage {
    tickets: FxHashMap<ChunkPos, StoredTickets>,
    timed_generation: u64,
}

impl ChunkTicketStorage {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Loads usable chunk tickets, logging rejected entries and file errors.
    pub(crate) async fn load(saved_data: &SavedDataManager, world: &Identifier) -> Self {
        let persistent = match saved_data
            .load_or_default::<RawPersistentChunkTickets>(saved_data_names::CHUNK_TICKETS)
            .await
        {
            Ok(persistent) => persistent,
            Err(error) => {
                log::warn!(
                    "Failed to load chunk_tickets.toml for world {world}: {error}; \
                     starting with empty ticket storage"
                );
                return Self::new();
            }
        };
        Self::from_persistent(persistent, world)
    }

    fn from_persistent(persistent: RawPersistentChunkTickets, world: &Identifier) -> Self {
        let mut storage = Self::new();
        for (index, value) in persistent.tickets.into_iter().enumerate() {
            let ticket = match value.try_into::<PersistentChunkTicket>() {
                Ok(ticket) => ticket,
                Err(error) => {
                    log::warn!(
                        "Skipping chunk_tickets.toml tickets[{index}] for world {world}: {error}"
                    );
                    continue;
                }
            };
            if let Err(error) = storage.add_loaded_persistent_ticket(ticket) {
                log::warn!(
                    "Skipping chunk_tickets.toml tickets[{index}] for world {world}: {error}"
                );
            }
        }
        storage
    }

    /// Adds one canonical ticket or refreshes an existing matching type and level.
    pub(crate) fn add_ticket(
        &mut self,
        pos: ChunkPos,
        ticket: ChunkTicket,
    ) -> SourceProjectionChanges {
        let timed_generation = self.generation_for(ticket.ticket_type());
        let tickets = self.tickets.entry(pos).or_default();

        if let Some(stored) = tickets.iter_mut().find(|stored| stored.ticket == ticket) {
            stored.ticket.reset_ticks_left();
            stored.timed_generation = timed_generation;
            return SourceProjectionChanges::default();
        }

        tickets.push(StoredChunkTicket::new(ticket, timed_generation));
        SourceProjectionChanges::for_ticket(pos, ticket)
    }

    /// Removes the canonical ticket matching `ticket`'s type identity and level.
    pub(crate) fn remove_ticket(
        &mut self,
        pos: ChunkPos,
        ticket: ChunkTicket,
    ) -> SourceProjectionChanges {
        let Some(tickets) = self.tickets.get_mut(&pos) else {
            return SourceProjectionChanges::default();
        };
        let Some(index) = tickets.iter().position(|stored| stored.ticket == ticket) else {
            return SourceProjectionChanges::default();
        };

        let removed = tickets.swap_remove(index).ticket;
        if tickets.is_empty() {
            self.tickets.remove(&pos);
        }
        SourceProjectionChanges::for_ticket(pos, removed)
    }

    /// Returns the strongest loading ticket at `pos`.
    #[must_use]
    pub(crate) fn load_source_level(&self, pos: ChunkPos) -> Option<ChunkTicketLevel> {
        self.tickets.get(&pos).and_then(|tickets| {
            tickets
                .iter()
                .filter_map(|stored| stored.ticket.loading_level())
                .min()
        })
    }

    /// Returns the strongest simulation ticket supported by Steel's tracker at `pos`.
    #[must_use]
    pub(crate) fn simulation_source_level(&self, pos: ChunkPos) -> Option<ChunkTicketLevel> {
        self.tickets.get(&pos).and_then(|tickets| {
            tickets
                .iter()
                .filter_map(|stored| stored.ticket.simulation_level())
                .filter(|level| level.is_block_ticking())
                .min()
        })
    }

    #[must_use]
    pub(crate) fn load_source_update(&self, pos: ChunkPos) -> SourceLevelUpdate {
        SourceLevelUpdate {
            pos,
            level: self.load_source_level(pos),
        }
    }

    #[must_use]
    pub(crate) fn simulation_source_update(&self, pos: ChunkPos) -> SourceLevelUpdate {
        SourceLevelUpdate {
            pos,
            level: self.simulation_source_level(pos),
        }
    }

    /// Enumerates the initial loading sources for propagation tracker seeding.
    #[must_use]
    pub(crate) fn initial_load_sources(&self) -> Vec<SourceLevelUpdate> {
        self.initial_sources(Self::load_source_update)
    }

    /// Enumerates the initial simulation sources for propagation tracker seeding.
    #[must_use]
    pub(crate) fn initial_simulation_sources(&self) -> Vec<SourceLevelUpdate> {
        self.initial_sources(Self::simulation_source_update)
    }

    fn initial_sources(
        &self,
        source_update: fn(&Self, ChunkPos) -> SourceLevelUpdate,
    ) -> Vec<SourceLevelUpdate> {
        let mut sources: Vec<_> = self
            .tickets
            .keys()
            .copied()
            .map(|pos| source_update(self, pos))
            .filter(|update| update.level.is_some())
            .collect();
        sources.sort_unstable_by_key(|update| (update.pos.0.x, update.pos.0.y));
        sources
    }

    /// Adds or refreshes Vanilla's post-portal ticket.
    pub(crate) fn add_or_refresh_portal_ticket(
        &mut self,
        pos: ChunkPos,
    ) -> SourceProjectionChanges {
        self.add_ticket(pos, portal_ticket())
    }

    /// Adds or refreshes Vanilla's in-flight ender pearl ticket.
    pub(crate) fn add_or_refresh_ender_pearl_ticket(
        &mut self,
        pos: ChunkPos,
    ) -> SourceProjectionChanges {
        self.add_ticket(pos, ender_pearl_ticket())
    }

    /// Snapshots the exact timed entries eligible for this world-tick pass.
    #[must_use]
    pub(crate) fn timed_ticket_expirations(&self) -> Vec<TimedTicketExpiration> {
        let mut expirations = Vec::new();
        for (&pos, tickets) in &self.tickets {
            expirations.extend(tickets.iter().filter_map(|stored| {
                stored
                    .timed_generation
                    .map(|generation| TimedTicketExpiration {
                        pos,
                        ticket: stored.ticket,
                        generation,
                    })
            }));
        }
        expirations.sort_unstable_by(Self::compare_expirations);
        expirations
    }

    /// Ages unchanged timed entries selected by the current world-tick pass.
    pub(crate) fn tick_timed_tickets(
        &mut self,
        expirations: &[TimedTicketExpiration],
    ) -> SourceProjectionChanges {
        let mut changes = SourceProjectionChanges::default();

        for &expiration in expirations {
            let Some(tickets) = self.tickets.get_mut(&expiration.pos) else {
                continue;
            };
            let Some(index) = tickets.iter().position(|stored| {
                stored.ticket == expiration.ticket
                    && stored.timed_generation == Some(expiration.generation)
            }) else {
                continue;
            };

            tickets[index].ticket.decrease_ticks_left();
            if !tickets[index].ticket.is_timed_out() {
                continue;
            }

            let expired = tickets.swap_remove(index).ticket;
            if tickets.is_empty() {
                self.tickets.remove(&expiration.pos);
            }
            changes.merge(SourceProjectionChanges::for_ticket(expiration.pos, expired));
        }

        changes
    }

    /// Converts every active ticket whose type has Vanilla's persist flag.
    #[must_use]
    pub(crate) fn to_persistent(&self) -> PersistentChunkTickets {
        let mut tickets = Vec::new();
        for (&pos, entries) in &self.tickets {
            tickets.extend(
                entries
                    .iter()
                    .filter_map(|stored| stored.to_persistent(pos)),
            );
        }
        tickets.sort_unstable_by(Self::compare_persistent_tickets);
        PersistentChunkTickets { tickets }
    }

    fn add_loaded_persistent_ticket(
        &mut self,
        persistent: PersistentChunkTicket,
    ) -> Result<(), ChunkTicketStorageLoadError> {
        let PersistentChunkTicket {
            ticket_type,
            chunk_x,
            chunk_z,
            level,
            ticks_left,
        } = persistent;
        let Some(ticket_type_ref) = REGISTRY.ticket_types.by_key(&ticket_type) else {
            return Err(ChunkTicketStorageLoadError::UnknownTicketType(ticket_type));
        };
        let Some(ticket_level) = ChunkTicketLevel::new(level) else {
            return Err(ChunkTicketStorageLoadError::InvalidTicketLevel { ticket_type, level });
        };

        let ticket = ChunkTicket::from_saved(ticket_type_ref, ticket_level, ticks_left);
        self.add_loaded_ticket(ChunkPos::new(chunk_x, chunk_z), ticket);
        Ok(())
    }

    fn add_loaded_ticket(&mut self, pos: ChunkPos, ticket: ChunkTicket) {
        let timed_generation = self.generation_for(ticket.ticket_type());
        let tickets = self.tickets.entry(pos).or_default();
        if let Some(stored) = tickets.iter_mut().find(|stored| stored.ticket == ticket) {
            stored.ticket.reset_ticks_left();
            stored.timed_generation = timed_generation;
            return;
        }

        tickets.push(StoredChunkTicket::new(ticket, timed_generation));
    }

    fn generation_for(&mut self, ticket_type: TicketTypeRef) -> Option<u64> {
        ticket_type
            .has_timeout()
            .then(|| self.allocate_timed_generation())
    }

    fn allocate_timed_generation(&mut self) -> u64 {
        assert_ne!(
            self.timed_generation,
            u64::MAX,
            "timed ticket generation exhausted"
        );
        self.timed_generation += 1;
        self.timed_generation
    }

    fn compare_expirations(
        left: &TimedTicketExpiration,
        right: &TimedTicketExpiration,
    ) -> Ordering {
        (left.pos.0.x, left.pos.0.y)
            .cmp(&(right.pos.0.x, right.pos.0.y))
            .then_with(|| {
                left.ticket
                    .ticket_type()
                    .key
                    .cmp(&right.ticket.ticket_type().key)
            })
            .then_with(|| left.ticket.ticket_level().cmp(&right.ticket.ticket_level()))
    }

    fn compare_persistent_tickets(
        left: &PersistentChunkTicket,
        right: &PersistentChunkTicket,
    ) -> Ordering {
        (left.chunk_x, left.chunk_z)
            .cmp(&(right.chunk_x, right.chunk_z))
            .then_with(|| left.ticket_type.cmp(&right.ticket_type))
            .then_with(|| left.level.cmp(&right.level))
    }

    #[cfg(test)]
    fn ticket_count(&self) -> usize {
        self.tickets.values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests;
