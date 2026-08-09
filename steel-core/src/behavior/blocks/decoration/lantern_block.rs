//! Lantern block behavior.

use std::sync::Arc;

use steel_macros::block_behavior;
use steel_registry::{
    blocks::{
        BlockRef,
        block_state_ext::BlockStateExt,
        properties::{BlockStateProperties, BoolProperty, Direction},
        shapes::SupportType,
    },
    vanilla_block_tags::BlockTag,
    vanilla_blocks,
};
use steel_utils::{BlockPos, BlockStateId};

use crate::{
    behavior::{
        BlockBehavior, BlockPlaceContext,
        block::{PickupResult, pickup_waterlogged_block, schedule_water_tick_if_waterlogged},
        blocks::{WeatherState, WeatheringCopper},
    },
    entity::ai::path::PathComputationType,
    fluid::{FluidStateExt, get_fluid_state},
    player::Player,
    world::{LevelReader, ScheduledTickAccess, World},
};

const HANGING: BoolProperty = BlockStateProperties::HANGING;
const WATERLOGGED: BoolProperty = BlockStateProperties::WATERLOGGED;

/// Behavior shared by ordinary, soul, and waxed copper lanterns.
#[block_behavior]
pub struct LanternBlock {
    block: BlockRef,
}

impl LanternBlock {
    /// Creates a lantern behavior for the given block.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    fn connected_direction(state: BlockStateId) -> Direction {
        if state.get_value(&HANGING) {
            Direction::Down
        } else {
            Direction::Up
        }
    }
}

impl BlockBehavior for LanternBlock {
    fn pickup_block(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        player: Option<&Player>,
    ) -> Option<PickupResult> {
        pickup_waterlogged_block(self, world, pos, state, player)
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
        schedule_water_tick_if_waterlogged(state, world, pos);

        if Self::connected_direction(state).opposite() == direction
            && !self.can_survive(state, world, pos)
        {
            return vanilla_blocks::AIR.default_state();
        }

        state
    }

    fn can_survive(&self, state: BlockStateId, world: &dyn LevelReader, pos: BlockPos) -> bool {
        let support_direction = Self::connected_direction(state).opposite();
        let support_pos = support_direction.relative(pos);
        let support_face = support_direction.opposite();
        let support_state = world.get_block_state(support_pos);

        if support_face == Direction::Down
            && support_state
                .get_block()
                .has_tag(&BlockTag::UNSTABLE_BOTTOM_CENTER)
        {
            return false;
        }

        world.is_face_sturdy_for(
            support_state,
            support_pos,
            support_face,
            SupportType::Center,
        )
    }

    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        let replaced_fluid_state = get_fluid_state(context.world, context.place_pos());

        for direction in context.get_nearest_looking_directions() {
            if !direction.axis().is_vertical() {
                continue;
            }

            let state = self
                .block
                .default_state()
                .set_value(&HANGING, direction == Direction::Up);
            if self.can_survive(state, context.world, context.place_pos()) {
                return Some(state.set_value(&WATERLOGGED, replaced_fluid_state.is_water()));
            }
        }

        None
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }
}

/// Lantern behavior with the shared vanilla copper oxidation algorithm.
#[block_behavior]
pub struct WeatheringLanternBlock {
    lantern: LanternBlock,
    #[json_arg(r#enum = "WeatherState", json = "weather_state")]
    weathering: WeatheringCopper,
}

impl WeatheringLanternBlock {
    /// Creates a weathering copper lantern behavior.
    #[must_use]
    pub const fn new(block: BlockRef, weather_state: WeatherState) -> Self {
        Self {
            lantern: LanternBlock::new(block),
            weathering: WeatheringCopper::new(weather_state),
        }
    }
}

impl BlockBehavior for WeatheringLanternBlock {
    fn pickup_block(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        player: Option<&Player>,
    ) -> Option<PickupResult> {
        self.lantern.pickup_block(world, pos, state, player)
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        neighbor_pos: BlockPos,
        neighbor_state: BlockStateId,
    ) -> BlockStateId {
        self.lantern
            .update_shape(state, world, pos, direction, neighbor_pos, neighbor_state)
    }

    fn can_survive(&self, state: BlockStateId, world: &dyn LevelReader, pos: BlockPos) -> bool {
        self.lantern.can_survive(state, world, pos)
    }

    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        self.lantern.get_state_for_placement(context)
    }

    fn random_tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        self.weathering.change_over_time(state, world, pos);
    }

    fn is_pathfindable(&self, state: BlockStateId, computation_type: PathComputationType) -> bool {
        self.lantern.is_pathfindable(state, computation_type)
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use steel_registry::{
        REGISTRY, blocks::block_state_ext::BlockStateExt, fluid::FluidState, init_vanilla_registry,
        vanilla_fluids,
    };

    use super::*;
    use crate::{
        behavior::{
            BLOCK_BEHAVIORS, init_behaviors,
            waxables::get_waxed_from_normal_variant,
            weathering::{next_copper_stage, previous_copper_stage},
        },
        test_support::TestLevel,
    };

    fn initialize() {
        init_vanilla_registry();
        init_behaviors();
    }

    #[test]
    fn standing_and_hanging_lanterns_require_their_connected_center_support() {
        initialize();
        let behavior = LanternBlock::new(&vanilla_blocks::LANTERN);
        let pos = BlockPos::new(0, 64, 0);
        let standing = vanilla_blocks::LANTERN.default_state();
        let hanging = standing.set_value(&HANGING, true);

        let below_support =
            TestLevel::default().with_block(pos.below(), vanilla_blocks::STONE.default_state());
        assert!(behavior.can_survive(standing, &below_support, pos));
        assert!(!behavior.can_survive(hanging, &below_support, pos));

        let above_support =
            TestLevel::default().with_block(pos.above(), vanilla_blocks::STONE.default_state());
        assert!(!behavior.can_survive(standing, &above_support, pos));
        assert!(behavior.can_survive(hanging, &above_support, pos));

        let unstable_ceiling = TestLevel::default()
            .with_block(pos.above(), vanilla_blocks::OAK_FENCE_GATE.default_state());
        assert!(!behavior.can_survive(hanging, &unstable_ceiling, pos));
    }

    #[test]
    fn removing_the_connected_support_breaks_lantern_and_schedules_retained_water() {
        initialize();
        let behavior = LanternBlock::new(&vanilla_blocks::LANTERN);
        let pos = BlockPos::new(0, 64, 0);
        let wet_standing = vanilla_blocks::LANTERN
            .default_state()
            .set_value(&WATERLOGGED, true);
        let unsupported = TestLevel::default();

        assert!(
            behavior
                .update_shape(
                    wet_standing,
                    &unsupported,
                    pos,
                    Direction::Down,
                    pos.below(),
                    vanilla_blocks::AIR.default_state(),
                )
                .is_air()
        );
        assert_eq!(
            unsupported
                .scheduled_fluid_ticks
                .borrow()
                .iter()
                .map(|tick| (tick.pos, tick.fluid, tick.delay))
                .collect::<Vec<_>>(),
            vec![(pos, &vanilla_fluids::WATER, 5)]
        );
    }

    #[test]
    fn unrelated_neighbor_updates_preserve_a_supported_lantern() {
        initialize();
        let behavior = LanternBlock::new(&vanilla_blocks::LANTERN);
        let pos = BlockPos::new(0, 64, 0);
        let state = vanilla_blocks::LANTERN.default_state();
        let level =
            TestLevel::default().with_block(pos.below(), vanilla_blocks::STONE.default_state());

        assert_eq!(
            behavior.update_shape(
                state,
                &level,
                pos,
                Direction::North,
                pos.north(),
                vanilla_blocks::AIR.default_state(),
            ),
            state
        );
    }

    #[test]
    fn copper_transitions_preserve_hanging_and_waterlogged_properties() {
        initialize();
        let weathering_stages = [
            &vanilla_blocks::COPPER_LANTERN,
            &vanilla_blocks::EXPOSED_COPPER_LANTERN,
            &vanilla_blocks::WEATHERED_COPPER_LANTERN,
            &vanilla_blocks::OXIDIZED_COPPER_LANTERN,
        ];
        let waxed_stages = [
            &vanilla_blocks::WAXED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_EXPOSED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_WEATHERED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_OXIDIZED_COPPER_LANTERN,
        ];

        for (weathering, waxed) in weathering_stages.into_iter().zip(waxed_stages) {
            assert_eq!(get_waxed_from_normal_variant(weathering), Some(waxed));
            assert_transition_preserves_properties(weathering, waxed);
        }

        for stages in weathering_stages.windows(2) {
            assert_eq!(next_copper_stage(stages[0]), Some(stages[1]));
            assert_eq!(previous_copper_stage(stages[1]), Some(stages[0]));
            assert_transition_preserves_properties(stages[0], stages[1]);
            assert_transition_preserves_properties(stages[1], stages[0]);
        }
    }

    fn assert_transition_preserves_properties(source_block: BlockRef, target_block: BlockRef) {
        let source = source_block
            .default_state()
            .set_value(&HANGING, true)
            .set_value(&WATERLOGGED, true);
        let transitioned = REGISTRY
            .blocks
            .copy_matching_properties(source, target_block);

        assert_eq!(transitioned.get_block(), target_block);
        assert!(transitioned.get_value(&HANGING));
        assert!(transitioned.get_value(&WATERLOGGED));
    }

    #[test]
    fn extracted_lantern_states_select_shifted_shapes_and_water_fluid() {
        initialize();
        let standing = vanilla_blocks::LANTERN.default_state();
        let hanging = standing.set_value(&HANGING, true);
        let wet_hanging = hanging.set_value(&WATERLOGGED, true);

        let standing_boxes = standing.get_static_outline_shape().boxes();
        let hanging_boxes = hanging.get_static_outline_shape().boxes();
        assert_eq!(standing_boxes.len(), hanging_boxes.len());
        assert!(
            standing_boxes
                .iter()
                .zip(hanging_boxes)
                .all(
                    |(standing, hanging)| standing.translate(DVec3::new(0.0, 0.0625, 0.0))
                        == *hanging
                )
        );
        assert_eq!(
            wet_hanging.get_fluid_state(),
            FluidState::source(&vanilla_fluids::WATER)
        );
    }

    #[test]
    fn weathering_lanterns_tick_until_oxidized_while_waxed_lanterns_do_not() {
        initialize();
        for block in [
            &vanilla_blocks::COPPER_LANTERN,
            &vanilla_blocks::EXPOSED_COPPER_LANTERN,
            &vanilla_blocks::WEATHERED_COPPER_LANTERN,
        ] {
            assert!(block.default_state().is_randomly_ticking());
        }

        for block in [
            &vanilla_blocks::OXIDIZED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_EXPOSED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_WEATHERED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_OXIDIZED_COPPER_LANTERN,
        ] {
            assert!(!block.default_state().is_randomly_ticking());
        }
    }

    #[test]
    fn every_lantern_family_member_uses_non_pathfindable_behavior() {
        initialize();
        for block in [
            &vanilla_blocks::LANTERN,
            &vanilla_blocks::SOUL_LANTERN,
            &vanilla_blocks::COPPER_LANTERN,
            &vanilla_blocks::EXPOSED_COPPER_LANTERN,
            &vanilla_blocks::WEATHERED_COPPER_LANTERN,
            &vanilla_blocks::OXIDIZED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_EXPOSED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_WEATHERED_COPPER_LANTERN,
            &vanilla_blocks::WAXED_OXIDIZED_COPPER_LANTERN,
        ] {
            let behavior = BLOCK_BEHAVIORS.get_behavior(block);
            for computation_type in [
                PathComputationType::Land,
                PathComputationType::Water,
                PathComputationType::Air,
            ] {
                assert!(!behavior.is_pathfindable(block.default_state(), computation_type));
            }
        }
    }
}
