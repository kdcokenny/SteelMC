//! Scoped holder cache for synchronous gameplay chunk lookups.
//!
//! Scopes may only cover intervals where active-map membership is stable. The
//! cache retains holder identity (including map absence), but never generation
//! permission or published status; those remain live per lookup.

use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    num::NonZeroU64,
    ptr,
    rc::Rc,
    sync::Arc,
};

use steel_utils::{BlockPos, BlockStateId, ChunkPos};

use super::{chunk_holder::ChunkHolder, chunk_map::ChunkMap, status::ChunkStatus};
use steel_utils::types::PackedChunkPos;

// Vanilla's ServerChunkCache keeps four recent synchronous lookups. Steel replaces that
// recency list with O(1) hash-indexed slots sized far beyond any per-scope working set:
// block-heavy workloads (explosions, mob AI sweeps, redstone updates) touch tens of chunk
// columns inside one scope, where a small recency array thrashes by design. Slot overwrite
// mirrors vanilla's ring eviction; only capacity, never identity or ordering semantics,
// is observable. This is an internal knob.
const CACHE_SLOT_COUNT: usize = 256;
const CACHE_SLOT_MASK: usize = CACHE_SLOT_COUNT - 1;

/// Spreads packed chunk positions uniformly across the direct-mapped cache slots.
#[inline]
fn cache_slot_index(pos: ChunkPos) -> usize {
    let mixed = u64::from_ne_bytes(PackedChunkPos::from(pos).as_raw().to_ne_bytes())
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((mixed >> 56) as usize) & CACHE_SLOT_MASK
}

/// Lookup statistics collected without synchronization inside one cache scope.
#[derive(Debug, Default)]
pub struct GameplayChunkLookupCacheStats {
    /// Lookups served by a cached active holder.
    pub holder_hits: usize,
    /// Lookups served by a cached active-map absence.
    pub missing_hits: usize,
    /// Cache misses that consulted the active SCC map.
    pub scc_lookups: usize,
    /// Lookups for another chunk map while this scope was active.
    pub foreign_map_bypasses: usize,
    /// Displaced entries: counts every overwrite of an occupied slot, which under the
    /// direct-mapped layout is a hash-slot collision rather than an LRU eviction.
    pub evictions: usize,
}

#[derive(PartialEq, Eq)]
struct CacheOwner(*const ());

impl CacheOwner {
    const fn for_chunk_map(chunk_map: &ChunkMap) -> Self {
        Self(ptr::from_ref(chunk_map).cast())
    }

    #[cfg(test)]
    const fn for_test<T>(owner: &T) -> Self {
        Self(ptr::from_ref(owner).cast())
    }
}

struct CacheEntry {
    pos: ChunkPos,
    holder: Option<Arc<ChunkHolder>>,
}

struct ActiveCache {
    owner: CacheOwner,
    scope_id: Option<NonZeroU64>,
    entries: [Option<CacheEntry>; CACHE_SLOT_COUNT],
    stats: GameplayChunkLookupCacheStats,
}

enum CacheEntryProbe {
    Hit(Option<Arc<ChunkHolder>>),
    Miss,
}

impl ActiveCache {
    fn new(owner: CacheOwner, scope_id: Option<NonZeroU64>) -> Self {
        Self {
            owner,
            scope_id,
            entries: [const { None }; CACHE_SLOT_COUNT],
            stats: GameplayChunkLookupCacheStats::default(),
        }
    }

    #[inline]
    fn lookup(&mut self, pos: ChunkPos) -> CacheEntryProbe {
        let index = cache_slot_index(pos);
        let Some(entry) = self.entries[index].as_ref() else {
            return CacheEntryProbe::Miss;
        };
        if entry.pos != pos {
            return CacheEntryProbe::Miss;
        }
        let holder = entry.holder.as_ref().map(Arc::clone);
        if holder.is_some() {
            self.stats.holder_hits += 1;
        } else {
            self.stats.missing_hits += 1;
        }
        CacheEntryProbe::Hit(holder)
    }

    fn insert(&mut self, pos: ChunkPos, holder: Option<Arc<ChunkHolder>>) {
        let index = cache_slot_index(pos);
        if self.entries[index].is_some() {
            self.stats.evictions += 1;
        }
        self.entries[index] = Some(CacheEntry { pos, holder });
    }
}

thread_local! {
    static ACTIVE_CACHE: RefCell<Option<ActiveCache>> = const { RefCell::new(None) };
    static NEXT_SCOPE_ID: Cell<u64> = const { Cell::new(1) };
}

fn next_scope_id() -> Option<NonZeroU64> {
    NEXT_SCOPE_ID.with(|next| {
        let current = NonZeroU64::new(next.get())?;
        next.set(current.get().checked_add(1).unwrap_or(0));
        Some(current)
    })
}

enum CacheProbe {
    Hit(Option<Arc<ChunkHolder>>),
    Miss,
    Bypass,
}

/// Installs an empty cache until the scope is finished or dropped.
///
/// Nested scopes restore the prior cache, and the owned holder references are
/// released on every exit path, including unwinding. Nested guards must exit in
/// the usual last-in, first-out order.
pub(crate) struct GameplayChunkLookupCacheScope<'map> {
    previous: Option<ActiveCache>,
    active: bool,
    _chunk_map: PhantomData<&'map ChunkMap>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl<'map> GameplayChunkLookupCacheScope<'map> {
    pub(crate) fn enter(chunk_map: &'map ChunkMap) -> Self {
        Self::enter_key(CacheOwner::for_chunk_map(chunk_map))
    }

    #[cfg(test)]
    fn enter_owner<T>(owner: &'map T) -> Self {
        Self::enter_key(CacheOwner::for_test(owner))
    }

    fn enter_key(owner: CacheOwner) -> Self {
        let previous = ACTIVE_CACHE
            .with(|cache| cache.replace(Some(ActiveCache::new(owner, next_scope_id()))));
        Self {
            previous,
            active: true,
            _chunk_map: PhantomData,
            _thread_bound: PhantomData,
        }
    }

    pub(crate) fn finish(mut self) -> GameplayChunkLookupCacheStats {
        self.restore()
            .map_or_else(GameplayChunkLookupCacheStats::default, |cache| cache.stats)
    }

    fn restore(&mut self) -> Option<ActiveCache> {
        if !self.active {
            return None;
        }
        self.active = false;
        ACTIVE_CACHE.with(|cache| cache.replace(self.previous.take()))
    }
}

/// A tiny holder cache for repeated, live Full-chunk block-state reads.
///
/// Entries are usable only inside the exact gameplay lookup-cache scope that populated them. That
/// scope is the proof that active-map membership is stable. The holder's current Full permission
/// and publication are rechecked for every read, and the block section itself is locked only long
/// enough to copy the state. Missing and unpublished chunks are never retained.
pub(crate) struct LocalFullChunkHolderCache {
    scope_id: Option<NonZeroU64>,
    entries: [Option<LocalFullChunkHolderCacheEntry>; CACHE_SLOT_COUNT],
    #[cfg(test)]
    stats: LocalFullChunkHolderCacheStats,
}

struct LocalFullChunkHolderCacheEntry {
    pos: ChunkPos,
    holder: Arc<ChunkHolder>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LocalFullChunkHolderCacheStats {
    pub(crate) holder_hits: usize,
    pub(crate) active_holder_lookups: usize,
    pub(crate) fallback_reads: usize,
}

impl LocalFullChunkHolderCache {
    pub(crate) const fn new() -> Self {
        Self {
            scope_id: None,
            entries: [const { None }; CACHE_SLOT_COUNT],
            #[cfg(test)]
            stats: LocalFullChunkHolderCacheStats {
                holder_hits: 0,
                active_holder_lookups: 0,
                fallback_reads: 0,
            },
        }
    }

    /// Reads the current state without retaining a section guard or a missing chunk result.
    pub(crate) fn block_state(
        &mut self,
        chunk_map: &ChunkMap,
        pos: BlockPos,
    ) -> Option<BlockStateId> {
        let owner = CacheOwner::for_chunk_map(chunk_map);
        let active_scope_id = active_scope_id(owner);
        if active_scope_id != self.scope_id {
            self.entries.fill_with(|| None);
            self.scope_id = active_scope_id;
        }

        let chunk_pos = ChunkPos::from_block_pos(pos);
        let Some(_) = active_scope_id else {
            #[cfg(test)]
            {
                self.stats.fallback_reads += 1;
            }
            return chunk_map.with_full_chunk(chunk_pos, |chunk| chunk.get_block_state(pos));
        };

        let slot_index = cache_slot_index(chunk_pos);
        if let Some(entry) = self.entries[slot_index]
            .as_ref()
            .filter(|entry| entry.pos == chunk_pos)
        {
            let holder = &entry.holder;
            let Some(state) = live_full_block_state(holder, pos) else {
                self.entries[slot_index] = None;
                return None;
            };
            #[cfg(test)]
            {
                self.stats.holder_hits += 1;
            }
            return Some(state);
        }

        #[cfg(test)]
        {
            self.stats.active_holder_lookups += 1;
        }
        let holder = chunk_map.active_full_chunk_holder(chunk_pos)?;
        let state = live_full_block_state(&holder, pos)?;
        self.entries[slot_index] = Some(LocalFullChunkHolderCacheEntry {
            pos: chunk_pos,
            holder,
        });
        Some(state)
    }

    #[cfg(test)]
    pub(crate) const fn stats(&self) -> LocalFullChunkHolderCacheStats {
        self.stats
    }
}

fn active_scope_id(owner: CacheOwner) -> Option<NonZeroU64> {
    ACTIVE_CACHE.with(|cache| {
        let cache = cache.borrow();
        let cache = cache.as_ref()?;
        (cache.owner == owner).then_some(cache.scope_id).flatten()
    })
}

fn live_full_block_state(holder: &ChunkHolder, pos: BlockPos) -> Option<BlockStateId> {
    if holder.is_status_disallowed(ChunkStatus::Full) {
        return None;
    }
    holder
        .try_full_chunk()
        .map(|chunk| chunk.get_block_state(pos))
}

impl Drop for GameplayChunkLookupCacheScope<'_> {
    fn drop(&mut self) {
        drop(self.restore());
    }
}

#[inline]
pub(crate) fn lookup_or_insert_with<F>(
    chunk_map: &ChunkMap,
    pos: ChunkPos,
    load: F,
) -> Option<Arc<ChunkHolder>>
where
    F: FnOnce() -> Option<Arc<ChunkHolder>>,
{
    lookup_or_insert_for_owner(CacheOwner::for_chunk_map(chunk_map), pos, load)
}

#[inline]
fn lookup_or_insert_for_owner<F>(
    owner: CacheOwner,
    pos: ChunkPos,
    load: F,
) -> Option<Arc<ChunkHolder>>
where
    F: FnOnce() -> Option<Arc<ChunkHolder>>,
{
    let probe = ACTIVE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let Some(cache) = cache.as_mut() else {
            return CacheProbe::Bypass;
        };
        if cache.owner != owner {
            cache.stats.foreign_map_bypasses += 1;
            return CacheProbe::Bypass;
        }
        match cache.lookup(pos) {
            CacheEntryProbe::Hit(holder) => return CacheProbe::Hit(holder),
            CacheEntryProbe::Miss => {}
        }
        cache.stats.scc_lookups += 1;
        CacheProbe::Miss
    });

    match probe {
        CacheProbe::Hit(holder) => holder,
        CacheProbe::Bypass => load(),
        CacheProbe::Miss => {
            let holder = load();
            ACTIVE_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                let Some(cache) = cache.as_mut() else {
                    return;
                };
                if cache.owner == owner {
                    cache.insert(pos, holder.as_ref().map(Arc::clone));
                }
            });
            holder
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steel_registry::{blocks::BlockRef, vanilla_blocks};
    use steel_utils::types::UpdateFlags;

    use crate::behavior::init_behaviors;
    use crate::chunk::Chunk;
    use crate::chunk::chunk_ticket_manager::ChunkTicketLevel;
    use crate::chunk::section::{ChunkSection, Sections};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
    use crate::world::World;

    fn holder(pos: ChunkPos) -> Arc<ChunkHolder> {
        Arc::new(ChunkHolder::new(
            pos,
            ChunkTicketLevel::FULL_CHUNK,
            None,
            0,
            16,
        ))
    }

    fn set_test_block(world: &Arc<World>, pos: BlockPos, block: BlockRef) {
        assert!(world.set_block(pos, block.default_state(), UpdateFlags::UPDATE_NONE));
    }
    #[test]
    fn colliding_slots_overwrite_and_force_reload() {
        let owner = 0_u8;
        let scope = GameplayChunkLookupCacheScope::enter_owner(&owner);
        let first = ChunkPos::new(5, 0);

        // Find another chunk position that maps to the same direct-mapped slot.
        let second = (1..=4096)
            .map(|z| ChunkPos::new(5, z))
            .find(|pos| cache_slot_index(*pos) == cache_slot_index(first))
            .expect("a colliding position must exist among 4096 candidates for 256 slots");
        let holder_a = holder(first);
        let holder_b = holder(second);
        let mut loads = 0;

        drop(lookup_or_insert_for_owner(
            CacheOwner::for_test(&owner),
            first,
            || {
                loads += 1;
                Some(Arc::clone(&holder_a))
            },
        ));
        drop(lookup_or_insert_for_owner(
            CacheOwner::for_test(&owner),
            second,
            || {
                loads += 1;
                Some(Arc::clone(&holder_b))
            },
        ));
        // Reloading `first` proves the collision overwrote its slot.
        drop(lookup_or_insert_for_owner(
            CacheOwner::for_test(&owner),
            first,
            || {
                loads += 1;
                Some(Arc::clone(&holder_a))
            },
        ));

        let stats = scope.finish();
        assert_eq!(loads, 3);
        assert_eq!(stats.scc_lookups, 3);
        assert_eq!(stats.evictions, 2);
        assert_eq!(stats.holder_hits, 0);
    }
    #[test]
    fn missing_holder_is_cached_within_scope() {
        let owner = 0_u8;
        let scope = GameplayChunkLookupCacheScope::enter_owner(&owner);
        let pos = ChunkPos::new(3, -7);
        let mut loads = 0;

        assert!(
            lookup_or_insert_for_owner(CacheOwner::for_test(&owner), pos, || {
                loads += 1;
                None
            })
            .is_none()
        );
        assert!(
            lookup_or_insert_for_owner(CacheOwner::for_test(&owner), pos, || {
                panic!("a cached missing holder should not reload")
            })
            .is_none()
        );

        let stats = scope.finish();
        assert_eq!(loads, 1);
        assert_eq!(stats.missing_hits, 1);
        assert_eq!(stats.scc_lookups, 1);
    }

    #[test]
    fn nested_scope_restores_outer_entries_and_releases_holders() {
        let outer_owner = 0_u8;
        let inner_owner = 1_u8;
        let pos = ChunkPos::new(2, 5);
        let holder = holder(pos);
        let outer = GameplayChunkLookupCacheScope::enter_owner(&outer_owner);

        drop(lookup_or_insert_for_owner(
            CacheOwner::for_test(&outer_owner),
            pos,
            || Some(Arc::clone(&holder)),
        ));
        assert_eq!(Arc::strong_count(&holder), 2);

        let inner = GameplayChunkLookupCacheScope::enter_owner(&inner_owner);
        assert!(
            lookup_or_insert_for_owner(
                CacheOwner::for_test(&inner_owner),
                ChunkPos::new(-1, -1),
                || None,
            )
            .is_none()
        );
        let inner_stats = inner.finish();
        assert_eq!(inner_stats.scc_lookups, 1);

        drop(lookup_or_insert_for_owner(
            CacheOwner::for_test(&outer_owner),
            pos,
            || panic!("the outer entry should be restored"),
        ));
        let outer_stats = outer.finish();
        assert_eq!(outer_stats.holder_hits, 1);
        assert_eq!(Arc::strong_count(&holder), 1);
    }

    #[test]
    fn dropping_scope_releases_entries_and_next_scope_starts_empty() {
        let owner = 0_u8;
        let pos = ChunkPos::new(-6, 11);
        let holder = holder(pos);

        {
            let _scope = GameplayChunkLookupCacheScope::enter_owner(&owner);
            drop(lookup_or_insert_for_owner(
                CacheOwner::for_test(&owner),
                pos,
                || Some(Arc::clone(&holder)),
            ));
            assert_eq!(Arc::strong_count(&holder), 2);
        }
        assert_eq!(Arc::strong_count(&holder), 1);

        let scope = GameplayChunkLookupCacheScope::enter_owner(&owner);
        let mut loads = 0;
        drop(lookup_or_insert_for_owner(
            CacheOwner::for_test(&owner),
            pos,
            || {
                loads += 1;
                Some(Arc::clone(&holder))
            },
        ));
        let stats = scope.finish();
        assert_eq!(loads, 1);
        assert_eq!(stats.scc_lookups, 1);
    }

    #[test]
    fn foreign_owner_bypasses_active_cache() {
        let owner = 0_u8;
        let foreign_owner = 1_u8;
        let scope = GameplayChunkLookupCacheScope::enter_owner(&owner);
        let pos = ChunkPos::new(8, 9);
        let holder = holder(pos);
        let mut loads = 0;

        for _ in 0..2 {
            drop(lookup_or_insert_for_owner(
                CacheOwner::for_test(&foreign_owner),
                pos,
                || {
                    loads += 1;
                    Some(Arc::clone(&holder))
                },
            ));
        }

        let stats = scope.finish();
        assert_eq!(loads, 2);
        assert_eq!(stats.foreign_map_bypasses, 2);
        assert_eq!(stats.scc_lookups, 0);
    }

    #[test]
    fn local_full_holder_cache_reuses_one_active_lookup_for_same_chunk() {
        let world = fresh_test_world("local_full_holder_cache_same_chunk");
        init_behaviors();
        let chunk_pos = ChunkPos::new(2, -1);
        insert_ready_full_chunk(&world, chunk_pos);
        let first = BlockPos::new(32, 64, -16);
        let second = BlockPos::new(47, 64, -1);
        set_test_block(&world, first, &vanilla_blocks::STONE);
        set_test_block(&world, second, &vanilla_blocks::DIRT);

        let scope = GameplayChunkLookupCacheScope::enter(&world.chunk_map);
        let mut local = LocalFullChunkHolderCache::new();
        assert_eq!(
            local.block_state(&world.chunk_map, first),
            Some(vanilla_blocks::STONE.default_state())
        );
        assert_eq!(
            local.block_state(&world.chunk_map, second),
            Some(vanilla_blocks::DIRT.default_state())
        );

        assert_eq!(
            local.stats(),
            LocalFullChunkHolderCacheStats {
                holder_hits: 1,
                active_holder_lookups: 1,
                fallback_reads: 0,
            }
        );
        let scope_stats = scope.finish();
        assert_eq!(scope_stats.scc_lookups, 1);
        assert_eq!(scope_stats.holder_hits, 0);
    }

    #[test]
    fn local_full_holder_cache_observes_live_block_mutation() {
        let world = fresh_test_world("local_full_holder_cache_live_mutation");
        init_behaviors();
        let pos = BlockPos::new(3, 64, 7);
        insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
        set_test_block(&world, pos, &vanilla_blocks::STONE);

        let scope = GameplayChunkLookupCacheScope::enter(&world.chunk_map);
        let mut local = LocalFullChunkHolderCache::new();
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::STONE.default_state())
        );
        set_test_block(&world, pos, &vanilla_blocks::DIRT);
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::DIRT.default_state())
        );
        drop(scope);
    }

    #[test]
    fn local_full_holder_cache_rechecks_full_permission() {
        let world = fresh_test_world("local_full_holder_cache_permission");
        init_behaviors();
        let pos = BlockPos::new(3, 64, 7);
        let holder = insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
        set_test_block(&world, pos, &vanilla_blocks::STONE);

        let scope = GameplayChunkLookupCacheScope::enter(&world.chunk_map);
        let mut local = LocalFullChunkHolderCache::new();
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::STONE.default_state())
        );

        holder.update_highest_allowed_status(None);
        assert_eq!(local.block_state(&world.chunk_map, pos), None);

        holder.update_highest_allowed_status(Some(ChunkTicketLevel::FULL_CHUNK));
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::STONE.default_state())
        );
        assert_eq!(local.stats().active_holder_lookups, 2);
        drop(scope);
    }

    #[test]
    fn local_full_holder_cache_does_not_retain_unpublished_data() {
        let world = fresh_test_world("local_full_holder_cache_publication");
        let chunk_pos = ChunkPos::new(-2, 3);
        let pos = BlockPos::new(-31, 64, 49);
        let min_y = world.chunk_map.world_gen_context.min_y();
        let height = world.chunk_map.world_gen_context.height();
        let holder = Arc::new(ChunkHolder::new(
            chunk_pos,
            ChunkTicketLevel::FULL_CHUNK,
            None,
            min_y,
            height,
        ));
        let _ = world
            .chunk_map
            .chunks
            .insert_sync(chunk_pos, Arc::clone(&holder));
        let scope = GameplayChunkLookupCacheScope::enter(&world.chunk_map);
        let mut local = LocalFullChunkHolderCache::new();

        assert_eq!(local.block_state(&world.chunk_map, pos), None);

        let sections = (0..height / 16)
            .map(|_| ChunkSection::new_empty())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let chunk = Chunk::new(
            Sections::from_owned(sections),
            chunk_pos,
            min_y,
            height,
            Arc::downgrade(&world),
        );
        assert!(
            chunk
                .set_block_state_for_generation(
                    ChunkStatus::Empty,
                    pos,
                    vanilla_blocks::STONE.default_state(),
                    UpdateFlags::UPDATE_NONE,
                )
                .is_some()
        );
        drop(chunk.promote_to_full());
        holder.insert_chunk(chunk, ChunkStatus::Full);

        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::STONE.default_state())
        );
        assert_eq!(local.stats().holder_hits, 0);
        assert_eq!(local.stats().active_holder_lookups, 2);
        drop(scope);
    }

    #[test]
    fn local_full_holder_cache_falls_back_without_stable_scope() {
        let world = fresh_test_world("local_full_holder_cache_fallback");
        init_behaviors();
        let pos = BlockPos::new(5, 64, 5);
        insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
        set_test_block(&world, pos, &vanilla_blocks::STONE);
        let mut local = LocalFullChunkHolderCache::new();

        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::STONE.default_state())
        );
        set_test_block(&world, pos, &vanilla_blocks::DIRT);
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::DIRT.default_state())
        );
        assert_eq!(local.stats().fallback_reads, 2);
        assert_eq!(local.stats().holder_hits, 0);
        assert_eq!(local.stats().active_holder_lookups, 0);
    }

    #[test]
    fn local_full_holder_cache_does_not_cross_scope_boundaries() {
        let world = fresh_test_world("local_full_holder_cache_scope_boundary");
        init_behaviors();
        let pos = BlockPos::new(5, 64, 5);
        let chunk_pos = ChunkPos::from_block_pos(pos);
        insert_ready_full_chunk(&world, chunk_pos);
        set_test_block(&world, pos, &vanilla_blocks::STONE);
        let mut local = LocalFullChunkHolderCache::new();

        let first_scope = GameplayChunkLookupCacheScope::enter(&world.chunk_map);
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::STONE.default_state())
        );
        drop(first_scope);

        let Some((_, old_holder)) = world.chunk_map.chunks.remove_sync(&chunk_pos) else {
            panic!("fixture holder should remain active");
        };
        drop(old_holder);
        world.unregister_full_chunk_ticks(chunk_pos);
        insert_ready_full_chunk(&world, chunk_pos);
        set_test_block(&world, pos, &vanilla_blocks::DIRT);

        let second_scope = GameplayChunkLookupCacheScope::enter(&world.chunk_map);
        assert_eq!(
            local.block_state(&world.chunk_map, pos),
            Some(vanilla_blocks::DIRT.default_state())
        );
        assert_eq!(local.stats().active_holder_lookups, 2);
        drop(second_scope);
    }
}
