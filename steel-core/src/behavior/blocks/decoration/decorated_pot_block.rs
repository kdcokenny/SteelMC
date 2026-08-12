use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{
    BlockStateProperties, BoolProperty, Direction, EnumProperty,
};
use steel_registry::data_components::components::PotDecorations;
use steel_registry::item_stack::ItemStack;
use steel_registry::particle_type::ParticleData;
use steel_registry::sound_types::{self, SoundType};
use steel_registry::vanilla_enchantment_tags::EnchantmentTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, TaggedRegistryExt as _, sound_events, vanilla_block_entity_types,
    vanilla_fluids, vanilla_game_events, vanilla_items, vanilla_particle_types,
};
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, Downcast as _};

use crate::behavior::block::{
    BlockBehavior, BlockEntityCreation, BlockLootContext, schedule_water_tick_if_waterlogged,
};
use crate::behavior::{
    BlockHitResult, BlockPlaceContext, InteractionResult, InventoryAccess, PlacementSource,
};
use crate::block_entity::BLOCK_ENTITIES;
use crate::block_entity::SharedBlockEntity;
use crate::block_entity::entities::{DecoratedPotBlockEntity, DecoratedPotContainer, WobbleStyle};
use crate::entity::ai::path::PathComputationType;
use crate::entity::projectile::Projectile;
use crate::fluid::get_fluid_state;
use crate::inventory::container::{Container as _, calculate_redstone_signal_from_container};
use crate::inventory::lock::ContainerLockGuard;
use crate::player::Player;
use crate::player::player_inventory::PlayerInventory;
use crate::world::game_event::GameEventContext;
use crate::world::{ClipHitResult, LevelReader, ScheduledTickAccess, World};

const FACING: EnumProperty<Direction> = BlockStateProperties::HORIZONTAL_FACING;
const WATERLOGGED: BoolProperty = BlockStateProperties::WATERLOGGED;
const CRACKED: BoolProperty = BlockStateProperties::CRACKED;

/// Vanilla `DecoratedPotBlock` behavior.
#[block_behavior]
pub struct DecoratedPotBlock {
    block: BlockRef,
}

impl DecoratedPotBlock {
    /// Creates behavior for the decorated-pot block.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    #[must_use]
    fn prevents_shattering(tool: &ItemStack) -> bool {
        let Some(enchantments) = tool.get_enchantments() else {
            return false;
        };
        enchantments.iter().any(|(key, _level)| {
            REGISTRY
                .enchantments
                .by_key(key)
                .is_some_and(|enchantment| {
                    REGISTRY.enchantments.is_in_tag(
                        enchantment,
                        &EnchantmentTag::PREVENTS_DECORATED_POT_SHATTERING,
                    )
                })
        })
    }

    fn decorated_pot(world: &dyn LevelReader, pos: BlockPos) -> Option<SharedBlockEntity> {
        let block_entity = world.get_block_entity(pos)?;
        block_entity
            .downcast_ref::<DecoratedPotBlockEntity>()
            .map(|_| ())?;
        Some(block_entity)
    }

    fn insert_item(
        pot: &DecoratedPotBlockEntity,
        player: &Player,
        hand: InteractionHand,
        inv: &InventoryAccess,
    ) -> Option<f32> {
        pot.prepare_container_access();
        let player_ref = inv.container_ref();
        let pot_ref = pot.inventory_ref();
        let player_id = player_ref.container_id();
        let pot_id = pot_ref.container_id();
        let mut guard = ContainerLockGuard::lock_all(&[&player_ref, &pot_ref]);
        let inserted_pitch = {
            let (player_inventory, pot_container) = guard
                .get_two_typed_mut::<PlayerInventory, DecoratedPotContainer>(player_id, pot_id)?;
            let held = player_inventory.get_item_in_hand_mut(hand);
            let stored = pot_container.get_item_mut(0);
            if held.is_empty()
                || (!stored.is_empty()
                    && (!ItemStack::is_same_item_same_components(stored, held)
                        || stored.count() >= stored.max_stack_size()))
            {
                return None;
            }

            let awarded = held.copy_with_count(1);
            if !player.has_infinite_materials() {
                held.shrink(1);
                player_inventory.set_changed();
            }

            if stored.is_empty() {
                *stored = awarded;
            } else {
                stored.grow(1);
            }
            Some(stored.count() as f32 / stored.max_stack_size() as f32)
        };
        let _ = guard.set_changed(pot_id);
        inserted_pitch
    }
}

impl BlockBehavior for DecoratedPotBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(
            self.block
                .default_state()
                .set_value(&FACING, context.horizontal_direction())
                .set_value(
                    &WATERLOGGED,
                    get_fluid_state(context.world, context.place_pos()).fluid_id
                        == &vanilla_fluids::WATER,
                )
                .set_value(&CRACKED, false),
        )
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        _direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        schedule_water_tick_if_waterlogged(state, world, pos);
        state
    }

    fn set_placed_by(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        source: &PlacementSource<'_>,
    ) {
        let Some(block_entity) = Self::decorated_pot(world.as_ref(), pos) else {
            return;
        };
        let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>() else {
            return;
        };
        source.with_item(|stack| pot.apply_components_from_item(stack));
    }

    fn use_item_on(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let Some(block_entity) = Self::decorated_pot(world.as_ref(), pos) else {
            return InteractionResult::Pass;
        };
        let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>() else {
            return InteractionResult::Pass;
        };
        let Some(fullness) = Self::insert_item(pot, player, hand, inv) else {
            return InteractionResult::TryEmptyHandInteraction;
        };

        pot.wobble(WobbleStyle::Positive);
        world.play_block_sound(
            &sound_events::BLOCK_DECORATED_POT_INSERT,
            pos,
            1.0,
            0.7 + 0.5 * fullness,
            None,
        );
        world.send_particles(
            ParticleData::simple(&vanilla_particle_types::DUST_PLUME),
            DVec3::new(
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()) + 1.2,
                f64::from(pos.z()) + 0.5,
            ),
            7,
            DVec3::ZERO,
            0.0,
        );
        world.game_event(
            &vanilla_game_events::BLOCK_CHANGE,
            pos,
            &GameEventContext::new(Some(player), None),
        );
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
        let Some(block_entity) = Self::decorated_pot(world.as_ref(), pos) else {
            return InteractionResult::Pass;
        };
        let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>() else {
            return InteractionResult::Pass;
        };
        world.play_block_sound(
            &sound_events::BLOCK_DECORATED_POT_INSERT_FAIL,
            pos,
            1.0,
            1.0,
            None,
        );
        pot.wobble(WobbleStyle::Negative);
        world.game_event(
            &vanilla_game_events::BLOCK_CHANGE,
            pos,
            &GameEventContext::new(Some(player), None),
        );
        InteractionResult::Success
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::DECORATED_POT,
            level,
            pos,
            state,
        ))
    }

    fn trigger_event(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        param_a: i32,
        param_b: i32,
    ) -> bool {
        world
            .get_block_entity(pos)
            .is_some_and(|block_entity| block_entity.trigger_event(param_a, param_b))
    }

    fn affect_neighbors_after_removal(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, state.get_block());
    }

    fn get_drops(
        &self,
        state: BlockStateId,
        context: &BlockLootContext<'_>,
    ) -> Option<Vec<ItemStack>> {
        let pot = context
            .block_entity()
            .and_then(|entity| entity.downcast_ref::<DecoratedPotBlockEntity>());
        if state.get_value(&CRACKED) {
            return Some(pot.map_or_else(Vec::new, |pot| {
                pot.decorations()
                    .ordered()
                    .into_iter()
                    .map(ItemStack::new)
                    .collect()
            }));
        }

        Some(vec![pot.map_or_else(
            || ItemStack::new(&vanilla_items::DECORATED_POT),
            |pot| DecoratedPotBlockEntity::create_decorated_pot_instance(pot.decorations()),
        )])
    }

    fn player_will_destroy(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> BlockStateId {
        let shatters = {
            let inventory = player.inventory.lock();
            let tool = inventory.get_selected_item();
            tool.item().has_tag(&ItemTag::BREAKS_DECORATED_POTS) && !Self::prevents_shattering(tool)
        };
        if !shatters {
            return state;
        }

        let cracked = state.set_value(&CRACKED, true);
        world.set_block(pos, cracked, UpdateFlags::UPDATE_NONE);
        cracked
    }

    fn on_projectile_hit(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        hit: &ClipHitResult,
        projectile: &dyn Projectile,
    ) {
        if !projectile.projectile_may_interact(world, hit.block_pos) || !projectile.may_break(world)
        {
            return;
        }
        world.set_block(
            hit.block_pos,
            state.set_value(&CRACKED, true),
            UpdateFlags::UPDATE_NONE,
        );
        world.destroy_block_by_entity(hit.block_pos, true, projectile.as_entity_event_source());
    }

    fn get_clone_item_stack(
        &self,
        _block: BlockRef,
        level: &dyn LevelReader,
        pos: BlockPos,
        _state: BlockStateId,
        _include_data: bool,
    ) -> Option<ItemStack> {
        let decorations = Self::decorated_pot(level, pos)
            .and_then(|entity| {
                entity
                    .downcast_ref::<DecoratedPotBlockEntity>()
                    .map(DecoratedPotBlockEntity::decorations)
            })
            .unwrap_or(PotDecorations::EMPTY);
        Some(DecoratedPotBlockEntity::create_decorated_pot_instance(
            decorations,
        ))
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_analog_output_signal(
        &self,
        _state: BlockStateId,
        world: &dyn LevelReader,
        pos: BlockPos,
        _direction: Direction,
    ) -> i32 {
        let Some(block_entity) = Self::decorated_pot(world, pos) else {
            return 0;
        };
        let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>() else {
            return 0;
        };
        pot.prepare_container_access();
        let container_ref = pot.inventory_ref();
        let guard = ContainerLockGuard::lock_all(&[&container_ref]);
        guard
            .get(container_ref.container_id())
            .map_or(0, calculate_redstone_signal_from_container)
    }

    fn get_sound_type(&self, state: BlockStateId) -> SoundType {
        if state.get_value(&CRACKED) {
            sound_types::DECORATED_POT_CRACKED
        } else {
            sound_types::DECORATED_POT
        }
    }
}
