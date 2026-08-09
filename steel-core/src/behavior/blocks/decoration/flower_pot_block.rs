use std::sync::Arc;

use steel_macros::block_behavior;
use steel_registry::{
    REGISTRY, blocks::BlockRef, item_stack::ItemStack, items::item::BlockHitResult, vanilla_blocks,
    vanilla_game_events,
};
use steel_utils::{
    BlockPos, BlockStateId, Direction,
    types::{InteractionHand, UpdateFlags},
};

use crate::{
    behavior::{BlockBehavior, BlockPlaceContext, InteractionResult, InventoryAccess, flower_pots},
    entity::ai::path::PathComputationType,
    player::Player,
    world::{ScheduledTickAccess, World, game_event::GameEventContext},
};

/// Vanilla flower-pot interactions and content association.
///
/// `potted` and the reverse insertion mapping both come from extractor-owned
/// `FlowerPotBlock` relationships rather than identifier conventions.
#[block_behavior]
pub struct FlowerPotBlock {
    block: BlockRef,
    #[json_arg(vanilla_blocks)]
    potted: BlockRef,
}

impl FlowerPotBlock {
    /// Creates a flower-pot behavior with its extracted content block.
    #[must_use]
    pub const fn new(block: BlockRef, potted: BlockRef) -> Self {
        Self { block, potted }
    }

    fn is_empty(&self) -> bool {
        self.potted == &vanilla_blocks::AIR
    }

    fn emit_block_change(world: &Arc<World>, pos: BlockPos, player: &Player) {
        world.game_event(
            &vanilla_game_events::BLOCK_CHANGE,
            pos,
            &GameEventContext::new(Some(player), None),
        );
    }
}

impl BlockBehavior for FlowerPotBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn use_item_on(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let potted = inv.with_item(|stack| flower_pots::by_item(stack.item()));
        let Some(potted) = potted else {
            return InteractionResult::TryEmptyHandInteraction;
        };
        if !self.is_empty() {
            return InteractionResult::Consume;
        }

        world.set_block(pos, potted.default_state(), UpdateFlags::UPDATE_ALL);
        Self::emit_block_change(world, pos, player);
        // Steel does not yet expose Vanilla's POT_FLOWER statistic foundation.
        if !player.has_infinite_materials() {
            inv.with_item(|stack| stack.shrink(1));
        }
        InteractionResult::Success
    }

    fn use_without_item(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        if self.is_empty() {
            return InteractionResult::Consume;
        }

        player.add_item_or_drop(ItemStack::new(REGISTRY.items.by_block(self.potted)));
        world.set_block(
            pos,
            vanilla_blocks::FLOWER_POT.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        Self::emit_block_change(world, pos, player);
        InteractionResult::Success
    }

    fn get_clone_item_stack(
        &self,
        _block: BlockRef,
        _state: BlockStateId,
        _include_data: bool,
    ) -> Option<ItemStack> {
        let block = if self.is_empty() {
            self.block
        } else {
            self.potted
        };
        Some(ItemStack::new(REGISTRY.items.by_block(block)))
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        if direction == Direction::Down && !self.can_survive(state, world, pos) {
            vanilla_blocks::AIR.default_state()
        } else {
            state
        }
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }

    fn random_tick(&self, _state: BlockStateId, _world: &Arc<World>, _pos: BlockPos) {
        // Exact potted-eyeblossom transforms require the per-position
        // EYEBLOSSOM_OPEN environment attribute, which Steel does not yet expose.
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::{init_vanilla_registry, vanilla_items};
    use steel_utils::{ChunkPos, types::GameType};
    use uuid::Uuid;

    use crate::{
        behavior::init_behaviors,
        inventory::container::Container,
        test_support::{TestLevel, TestPlayerBuilder, fresh_test_world, insert_ready_full_chunk},
    };

    use super::*;

    #[test]
    fn every_extracted_relationship_round_trips_through_its_block_item() {
        init_vanilla_registry();

        let entries = flower_pots::entries();
        assert_eq!(entries.len(), flower_pots::FLOWER_POT_ENTRY_COUNT);
        assert_eq!(
            entries
                .iter()
                .filter(|(_, content)| *content == &vanilla_blocks::AIR)
                .count(),
            1
        );

        for (pot, content) in entries {
            if content == &vanilla_blocks::AIR {
                assert_eq!(pot, &vanilla_blocks::FLOWER_POT);
                continue;
            }

            let content_item = REGISTRY.items.by_block(content);
            assert_ne!(content_item, &*vanilla_items::AIR, "{}", content.key);
            assert!(
                REGISTRY.items.is_block_item(content_item),
                "{}",
                content.key
            );
            assert_eq!(
                flower_pots::by_item(content_item),
                Some(pot),
                "{}",
                content.key
            );
        }
    }

    #[test]
    fn invalid_items_are_not_pottable() {
        init_vanilla_registry();

        assert_eq!(flower_pots::by_item(&vanilla_items::STICK), None);
        assert_eq!(flower_pots::by_item(&vanilla_items::FLOWER_POT), None);
    }

    fn hit_result(pos: BlockPos) -> BlockHitResult {
        BlockHitResult {
            location: DVec3::new(
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()) + 0.5,
                f64::from(pos.z()) + 0.5,
            ),
            direction: Direction::Up,
            block_pos: pos,
            miss: false,
            inside: false,
            world_border_hit: false,
        }
    }

    #[test]
    fn insertion_consumes_only_survival_items_and_occupied_pots_consume_the_action() {
        init_vanilla_registry();
        init_behaviors();
        let world = fresh_test_world("flower_pot_insertion");
        let pos = BlockPos::new(0, 64, 0);
        insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
        let player =
            TestPlayerBuilder::new(Arc::clone(&world), Uuid::from_u128(1), "FlowerPotTester", 1)
                .build();
        let empty = FlowerPotBlock::new(&vanilla_blocks::FLOWER_POT, &vanilla_blocks::AIR);
        let occupied = FlowerPotBlock::new(&vanilla_blocks::POTTED_POPPY, &vanilla_blocks::POPPY);

        world.set_block(
            pos,
            vanilla_blocks::FLOWER_POT.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        player
            .inventory
            .lock()
            .set_selected_item(ItemStack::with_count(&vanilla_items::POPPY, 2));
        let mut inventory =
            InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
        assert_eq!(
            empty.use_item_on(
                vanilla_blocks::FLOWER_POT.default_state(),
                &world,
                pos,
                &player,
                InteractionHand::MainHand,
                &hit_result(pos),
                &mut inventory,
            ),
            InteractionResult::Success
        );
        assert_eq!(
            world.get_block_state(pos),
            vanilla_blocks::POTTED_POPPY.default_state()
        );
        assert_eq!(player.inventory.lock().get_selected_item().count(), 1);

        player
            .inventory
            .lock()
            .set_selected_item(ItemStack::with_count(&vanilla_items::CACTUS, 2));
        assert_eq!(
            occupied.use_item_on(
                vanilla_blocks::POTTED_POPPY.default_state(),
                &world,
                pos,
                &player,
                InteractionHand::MainHand,
                &hit_result(pos),
                &mut inventory,
            ),
            InteractionResult::Consume
        );
        assert_eq!(player.inventory.lock().get_selected_item().count(), 2);
        assert_eq!(
            world.get_block_state(pos),
            vanilla_blocks::POTTED_POPPY.default_state()
        );

        player.restore_game_modes(GameType::Creative, None);
        world.set_block(
            pos,
            vanilla_blocks::FLOWER_POT.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        player
            .inventory
            .lock()
            .set_selected_item(ItemStack::with_count(&vanilla_items::CACTUS, 2));
        assert_eq!(
            empty.use_item_on(
                vanilla_blocks::FLOWER_POT.default_state(),
                &world,
                pos,
                &player,
                InteractionHand::MainHand,
                &hit_result(pos),
                &mut inventory,
            ),
            InteractionResult::Success
        );
        assert_eq!(
            world.get_block_state(pos),
            vanilla_blocks::POTTED_CACTUS.default_state()
        );
        assert_eq!(player.inventory.lock().get_selected_item().count(), 2);
    }

    #[test]
    fn invalid_item_falls_through_and_removal_returns_the_content() {
        init_vanilla_registry();
        init_behaviors();
        let world = fresh_test_world("flower_pot_removal");
        let pos = BlockPos::new(0, 64, 0);
        insert_ready_full_chunk(&world, ChunkPos::from_block_pos(pos));
        let player =
            TestPlayerBuilder::new(Arc::clone(&world), Uuid::from_u128(2), "FlowerPotTester", 2)
                .build();
        world.set_block(
            pos,
            vanilla_blocks::POTTED_CACTUS.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        player
            .inventory
            .lock()
            .set_selected_item(ItemStack::new(&vanilla_items::STICK));
        let occupied = FlowerPotBlock::new(&vanilla_blocks::POTTED_CACTUS, &vanilla_blocks::CACTUS);
        let mut inventory =
            InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);

        assert_eq!(
            occupied.use_item_on(
                vanilla_blocks::POTTED_CACTUS.default_state(),
                &world,
                pos,
                &player,
                InteractionHand::MainHand,
                &hit_result(pos),
                &mut inventory,
            ),
            InteractionResult::TryEmptyHandInteraction
        );
        assert_eq!(
            occupied.use_without_item(
                vanilla_blocks::POTTED_CACTUS.default_state(),
                &world,
                pos,
                &player,
                &hit_result(pos),
                &mut inventory,
            ),
            InteractionResult::Success
        );
        assert_eq!(
            world.get_block_state(pos),
            vanilla_blocks::FLOWER_POT.default_state()
        );
        let inventory = player.inventory.lock();
        assert_eq!(inventory.get_selected_item().item(), &*vanilla_items::STICK);
        assert!(inventory.contains_stack(&ItemStack::new(&vanilla_items::CACTUS)));
    }

    #[test]
    fn pick_block_returns_the_content_for_occupied_pots() {
        init_vanilla_registry();

        let occupied = FlowerPotBlock::new(&vanilla_blocks::POTTED_CACTUS, &vanilla_blocks::CACTUS);
        let picked = occupied
            .get_clone_item_stack(
                &vanilla_blocks::POTTED_CACTUS,
                vanilla_blocks::POTTED_CACTUS.default_state(),
                false,
            )
            .unwrap_or_else(ItemStack::empty);
        assert_eq!(picked.item(), &*vanilla_items::CACTUS);

        let empty = FlowerPotBlock::new(&vanilla_blocks::FLOWER_POT, &vanilla_blocks::AIR);
        let picked = empty
            .get_clone_item_stack(
                &vanilla_blocks::FLOWER_POT,
                vanilla_blocks::FLOWER_POT.default_state(),
                false,
            )
            .unwrap_or_else(ItemStack::empty);
        assert_eq!(picked.item(), &*vanilla_items::FLOWER_POT);
    }

    #[test]
    fn flower_pots_are_never_pathfindable_and_inherited_survival_keeps_them_supported() {
        init_vanilla_registry();

        let behavior = FlowerPotBlock::new(&vanilla_blocks::FLOWER_POT, &vanilla_blocks::AIR);
        let state = vanilla_blocks::FLOWER_POT.default_state();
        assert!(!behavior.is_pathfindable(state, PathComputationType::Land));
        assert!(!behavior.is_pathfindable(state, PathComputationType::Water));
        assert!(!behavior.is_pathfindable(state, PathComputationType::Air));

        let level = TestLevel::default();
        assert_eq!(
            behavior.update_shape(
                state,
                &level,
                BlockPos::new(0, 64, 0),
                Direction::Down,
                BlockPos::new(0, 63, 0),
                vanilla_blocks::AIR.default_state(),
            ),
            state
        );
    }
}
