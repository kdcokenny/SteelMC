//! Vanilla local-difficulty calculation and world lookup.

use steel_registry::vanilla_world_clocks;
use steel_utils::{BlockPos, ChunkPos, types::Difficulty};

use crate::chunk::DEFAULT_INHABITED_TIME;

use super::{World, environment};

const DIFFICULTY_TIME_GLOBAL_OFFSET: f32 = -72_000.0;
const MAX_DIFFICULTY_TIME_GLOBAL: f32 = 1_440_000.0;
const MAX_DIFFICULTY_TIME_LOCAL: f32 = 3_600_000.0;
const BASE_DIFFICULTY_SCALE: f32 = 0.75;
const MAX_GLOBAL_DIFFICULTY_BONUS: f32 = 0.25;
const NORMAL_LOCAL_DIFFICULTY_SCALE: f32 = 0.75;
const HARD_LOCAL_DIFFICULTY_SCALE: f32 = 1.0;
const MOON_DIFFICULTY_SCALE: f32 = 0.25;
const EASY_LOCAL_DIFFICULTY_SCALE: f32 = 0.5;
const HARD_DIFFICULTY_THRESHOLD: f32 = Difficulty::Hard as u8 as f32;
const SPECIAL_DIFFICULTY_MIN: f32 = 2.0;
const SPECIAL_DIFFICULTY_MAX: f32 = 4.0;
const UNLOADED_CHUNK_MOON_BRIGHTNESS: f32 = 0.0;
const MISSING_OVERWORLD_CLOCK_TIME: i64 = 0;

/// Vanilla's local difficulty at one position and point in time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DifficultyInstance {
    base: Difficulty,
    effective_difficulty: f32,
}

impl DifficultyInstance {
    /// Calculates local difficulty from the world clock, chunk age, and moon brightness.
    #[must_use]
    pub fn new(
        base: Difficulty,
        total_game_time: i64,
        local_game_time: i64,
        moon_brightness: f32,
    ) -> Self {
        Self {
            base,
            effective_difficulty: Self::calculate_difficulty(
                base,
                total_game_time,
                local_game_time,
                moon_brightness,
            ),
        }
    }

    /// Returns the configured world difficulty.
    #[must_use]
    pub const fn difficulty(self) -> Difficulty {
        self.base
    }

    /// Returns the calculated local difficulty.
    #[must_use]
    pub const fn effective_difficulty(self) -> f32 {
        self.effective_difficulty
    }

    /// Returns whether the effective difficulty has reached Vanilla's hard threshold.
    #[must_use]
    pub const fn is_hard(self) -> bool {
        self.effective_difficulty >= HARD_DIFFICULTY_THRESHOLD
    }

    /// Returns whether the effective difficulty is strictly above a threshold.
    #[must_use]
    pub const fn is_harder_than(self, required_difficulty: f32) -> bool {
        self.effective_difficulty > required_difficulty
    }

    /// Returns Vanilla's normalized multiplier for special local-difficulty effects.
    #[must_use]
    pub fn special_multiplier(self) -> f32 {
        if self.effective_difficulty < SPECIAL_DIFFICULTY_MIN {
            0.0
        } else if self.effective_difficulty > SPECIAL_DIFFICULTY_MAX {
            1.0
        } else {
            (self.effective_difficulty - SPECIAL_DIFFICULTY_MIN)
                / (SPECIAL_DIFFICULTY_MAX - SPECIAL_DIFFICULTY_MIN)
        }
    }

    fn calculate_difficulty(
        base: Difficulty,
        total_game_time: i64,
        local_game_time: i64,
        moon_brightness: f32,
    ) -> f32 {
        if base == Difficulty::Peaceful {
            return 0.0;
        }

        let is_hard = base == Difficulty::Hard;
        let global_scale = ((total_game_time as f32 + DIFFICULTY_TIME_GLOBAL_OFFSET)
            / MAX_DIFFICULTY_TIME_GLOBAL)
            .clamp(0.0, 1.0)
            * MAX_GLOBAL_DIFFICULTY_BONUS;
        let mut scale = BASE_DIFFICULTY_SCALE + global_scale;
        let local_time_scale = (local_game_time as f32 / MAX_DIFFICULTY_TIME_LOCAL).clamp(0.0, 1.0)
            * if is_hard {
                HARD_LOCAL_DIFFICULTY_SCALE
            } else {
                NORMAL_LOCAL_DIFFICULTY_SCALE
            };
        let moon_scale = (moon_brightness * MOON_DIFFICULTY_SCALE).clamp(0.0, global_scale);
        let mut local_scale = local_time_scale + moon_scale;
        if base == Difficulty::Easy {
            local_scale *= EASY_LOCAL_DIFFICULTY_SCALE;
        }
        scale += local_scale;

        f32::from(u8::from(base)) * scale
    }
}

impl World {
    /// Returns the local difficulty at a block position without loading its chunk.
    #[must_use]
    pub fn current_difficulty_at(&self, pos: BlockPos) -> DifficultyInstance {
        let mut inhabited_time = DEFAULT_INHABITED_TIME;
        let mut moon_brightness = UNLOADED_CHUNK_MOON_BRIGHTNESS;
        let chunk_pos = ChunkPos::from_block_pos(pos);
        self.chunk_map.with_full_chunk(chunk_pos, |chunk| {
            inhabited_time = chunk.common().inhabited_time();
            moon_brightness = self.moon_brightness_at(pos);
        });
        let overworld_clock_time = match self.clock_total_ticks(&vanilla_world_clocks::OVERWORLD) {
            Some(ticks) => ticks,
            None => MISSING_OVERWORLD_CLOCK_TIME,
        };

        DifficultyInstance::new(
            self.difficulty(),
            overworld_clock_time,
            inhabited_time,
            moon_brightness,
        )
    }

    /// Returns the current moon brightness from this world's environment timelines.
    #[must_use]
    pub fn moon_brightness_at(&self, _pos: BlockPos) -> f32 {
        let level_data = self.level_data.read();
        environment::moon_phase(self.dimension_type, level_data.world_clocks()).brightness()
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::moon_phase::{MOON_PHASE_LENGTH_TICKS, MoonPhase};
    use steel_utils::types::Difficulty;

    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    use super::*;

    const F32_CLOSE_EPSILON: f32 = 0.000_001;
    const BEFORE_GLOBAL_RAMP_TICKS: i64 = 0;
    const GLOBAL_RAMP_START_TICKS: i64 = 72_000;
    const GLOBAL_RAMP_MIDPOINT_TICKS: i64 =
        GLOBAL_RAMP_START_TICKS + MAX_DIFFICULTY_TIME_GLOBAL as i64 / 2;
    const GLOBAL_RAMP_END_TICKS: i64 = GLOBAL_RAMP_START_TICKS + MAX_DIFFICULTY_TIME_GLOBAL as i64;
    const AFTER_GLOBAL_RAMP_TICKS: i64 = GLOBAL_RAMP_END_TICKS + 1;
    const NO_INHABITED_TIME: i64 = DEFAULT_INHABITED_TIME;
    const HALF_LOCAL_RAMP_TICKS: i64 = MAX_DIFFICULTY_TIME_LOCAL as i64 / 2;
    const QUARTER_LOCAL_RAMP_TICKS: i64 = MAX_DIFFICULTY_TIME_LOCAL as i64 / 4;
    const FULL_LOCAL_RAMP_TICKS: i64 = MAX_DIFFICULTY_TIME_LOCAL as i64;
    const AFTER_LOCAL_RAMP_TICKS: i64 = FULL_LOCAL_RAMP_TICKS + 1;
    const BELOW_LOCAL_RAMP_TICKS: i64 = -1;
    const NO_MOON_BRIGHTNESS: f32 = 0.0;
    const FULL_MOON_BRIGHTNESS: f32 = 1.0;
    const BELOW_MINIMUM_MOON_BRIGHTNESS: f32 = -1.0;
    const ABOVE_MAXIMUM_MOON_BRIGHTNESS: f32 = 2.0;
    const PEACEFUL_INITIAL_DIFFICULTY: f32 = 0.0;
    const EASY_INITIAL_DIFFICULTY: f32 = 0.75;
    const NORMAL_INITIAL_DIFFICULTY: f32 = 1.5;
    const HARD_INITIAL_DIFFICULTY: f32 = 2.25;
    const NORMAL_MID_RAMP_DIFFICULTY: f32 = 2.5;
    const NORMAL_NO_MOON_MAX_DIFFICULTY: f32 = 3.5;
    const EASY_MAX_DIFFICULTY: f32 = 1.5;
    const NORMAL_MAX_DIFFICULTY: f32 = 4.0;
    const HARD_MAX_DIFFICULTY: f32 = 6.75;
    const HARD_WITHOUT_GLOBAL_BONUS_DIFFICULTY: f32 = 5.25;
    const NORMAL_MID_GLOBAL_WITHOUT_MOON_DIFFICULTY: f32 = 1.75;
    const NORMAL_MID_GLOBAL_WITH_CLAMPED_MOON_DIFFICULTY: f32 = 2.0;
    const BLOCKS_PER_CHUNK_EDGE: i32 = 16;
    const LAST_BLOCK_OFFSET_IN_CHUNK: i32 = BLOCKS_PER_CHUNK_EDGE - 1;
    const MIN_SPECIAL_MULTIPLIER: f32 = 0.0;
    const MAX_SPECIAL_MULTIPLIER: f32 = 1.0;
    const FULL_MOON_AFTER_GLOBAL_RAMP_TICKS: i64 = GLOBAL_RAMP_END_TICKS + MOON_PHASE_LENGTH_TICKS;

    fn assert_f32_close(left: f32, right: f32) {
        assert!(
            (left - right).abs() < F32_CLOSE_EPSILON,
            "left={left}, right={right}"
        );
    }

    fn effective(
        difficulty: Difficulty,
        total_time: i64,
        inhabited_time: i64,
        moon_brightness: f32,
    ) -> f32 {
        DifficultyInstance::new(difficulty, total_time, inhabited_time, moon_brightness)
            .effective_difficulty()
    }

    #[test]
    fn every_base_difficulty_has_its_vanilla_initial_value() {
        assert_f32_close(
            effective(
                Difficulty::Peaceful,
                BEFORE_GLOBAL_RAMP_TICKS,
                NO_INHABITED_TIME,
                NO_MOON_BRIGHTNESS,
            ),
            PEACEFUL_INITIAL_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Easy,
                BEFORE_GLOBAL_RAMP_TICKS,
                NO_INHABITED_TIME,
                NO_MOON_BRIGHTNESS,
            ),
            EASY_INITIAL_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Normal,
                BEFORE_GLOBAL_RAMP_TICKS,
                NO_INHABITED_TIME,
                NO_MOON_BRIGHTNESS,
            ),
            NORMAL_INITIAL_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Hard,
                BEFORE_GLOBAL_RAMP_TICKS,
                NO_INHABITED_TIME,
                NO_MOON_BRIGHTNESS,
            ),
            HARD_INITIAL_DIFFICULTY,
        );
    }

    #[test]
    fn global_and_local_time_ramps_clamp_at_vanilla_boundaries() {
        assert_f32_close(
            effective(
                Difficulty::Normal,
                GLOBAL_RAMP_START_TICKS,
                BELOW_LOCAL_RAMP_TICKS,
                NO_MOON_BRIGHTNESS,
            ),
            NORMAL_INITIAL_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Normal,
                GLOBAL_RAMP_MIDPOINT_TICKS,
                HALF_LOCAL_RAMP_TICKS,
                NO_MOON_BRIGHTNESS,
            ),
            NORMAL_MID_RAMP_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Normal,
                GLOBAL_RAMP_END_TICKS,
                FULL_LOCAL_RAMP_TICKS,
                NO_MOON_BRIGHTNESS,
            ),
            NORMAL_NO_MOON_MAX_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Normal,
                AFTER_GLOBAL_RAMP_TICKS,
                AFTER_LOCAL_RAMP_TICKS,
                NO_MOON_BRIGHTNESS,
            ),
            NORMAL_NO_MOON_MAX_DIFFICULTY,
        );
    }

    #[test]
    fn local_ramp_and_moon_contribution_scale_by_base_difficulty() {
        assert_f32_close(
            effective(
                Difficulty::Easy,
                GLOBAL_RAMP_END_TICKS,
                FULL_LOCAL_RAMP_TICKS,
                FULL_MOON_BRIGHTNESS,
            ),
            EASY_MAX_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Normal,
                GLOBAL_RAMP_END_TICKS,
                FULL_LOCAL_RAMP_TICKS,
                FULL_MOON_BRIGHTNESS,
            ),
            NORMAL_MAX_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Hard,
                GLOBAL_RAMP_END_TICKS,
                FULL_LOCAL_RAMP_TICKS,
                FULL_MOON_BRIGHTNESS,
            ),
            HARD_MAX_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Hard,
                GLOBAL_RAMP_START_TICKS,
                FULL_LOCAL_RAMP_TICKS,
                FULL_MOON_BRIGHTNESS,
            ),
            HARD_WITHOUT_GLOBAL_BONUS_DIFFICULTY,
        );
    }

    #[test]
    fn moon_contribution_clamps_to_zero_and_global_progress() {
        assert_f32_close(
            effective(
                Difficulty::Normal,
                GLOBAL_RAMP_MIDPOINT_TICKS,
                NO_INHABITED_TIME,
                BELOW_MINIMUM_MOON_BRIGHTNESS,
            ),
            NORMAL_MID_GLOBAL_WITHOUT_MOON_DIFFICULTY,
        );
        assert_f32_close(
            effective(
                Difficulty::Normal,
                GLOBAL_RAMP_MIDPOINT_TICKS,
                NO_INHABITED_TIME,
                ABOVE_MAXIMUM_MOON_BRIGHTNESS,
            ),
            NORMAL_MID_GLOBAL_WITH_CLAMPED_MOON_DIFFICULTY,
        );
    }

    #[test]
    fn special_multiplier_and_hard_checks_keep_exact_boundaries() {
        let below_special = DifficultyInstance::new(
            Difficulty::Normal,
            BEFORE_GLOBAL_RAMP_TICKS,
            NO_INHABITED_TIME,
            NO_MOON_BRIGHTNESS,
        );
        let special_start = DifficultyInstance::new(
            Difficulty::Normal,
            GLOBAL_RAMP_END_TICKS,
            NO_INHABITED_TIME,
            NO_MOON_BRIGHTNESS,
        );
        let special_end = DifficultyInstance::new(
            Difficulty::Normal,
            GLOBAL_RAMP_END_TICKS,
            FULL_LOCAL_RAMP_TICKS,
            FULL_MOON_BRIGHTNESS,
        );
        let above_special = DifficultyInstance::new(
            Difficulty::Hard,
            GLOBAL_RAMP_END_TICKS,
            FULL_LOCAL_RAMP_TICKS,
            FULL_MOON_BRIGHTNESS,
        );
        let hard_boundary = DifficultyInstance::new(
            Difficulty::Hard,
            GLOBAL_RAMP_START_TICKS,
            QUARTER_LOCAL_RAMP_TICKS,
            NO_MOON_BRIGHTNESS,
        );

        assert_f32_close(below_special.special_multiplier(), MIN_SPECIAL_MULTIPLIER);
        assert_f32_close(special_start.special_multiplier(), MIN_SPECIAL_MULTIPLIER);
        assert!(!special_start.is_hard());
        assert!(!special_start.is_harder_than(SPECIAL_DIFFICULTY_MIN));
        assert_f32_close(special_end.special_multiplier(), MAX_SPECIAL_MULTIPLIER);
        assert!(special_end.is_hard());
        assert!(!special_end.is_harder_than(SPECIAL_DIFFICULTY_MAX));
        assert_f32_close(above_special.special_multiplier(), MAX_SPECIAL_MULTIPLIER);
        assert!(above_special.is_harder_than(SPECIAL_DIFFICULTY_MAX));
        assert!(hard_boundary.is_hard());
        assert!(!hard_boundary.is_harder_than(HARD_DIFFICULTY_THRESHOLD));
    }

    #[test]
    fn world_lookup_reads_containing_chunk_clock_and_moon_timeline() {
        let world = fresh_test_world("current_local_difficulty");
        world.set_difficulty(Difficulty::Hard);
        let containing_chunk = ChunkPos::new(2, -3);
        let other_chunk = ChunkPos::new(3, -3);
        let containing_holder = insert_ready_full_chunk(&world, containing_chunk);
        let other_holder = insert_ready_full_chunk(&world, other_chunk);
        let containing_inhabited_time = HALF_LOCAL_RAMP_TICKS;
        let other_inhabited_time = FULL_LOCAL_RAMP_TICKS;
        let Some(containing_full_chunk) = containing_holder.try_full_chunk() else {
            panic!("containing test chunk should be Full");
        };
        containing_full_chunk
            .common()
            .set_inhabited_time(containing_inhabited_time);
        let Some(other_full_chunk) = other_holder.try_full_chunk() else {
            panic!("other test chunk should be Full");
        };
        other_full_chunk
            .common()
            .set_inhabited_time(other_inhabited_time);
        assert_eq!(
            world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, GLOBAL_RAMP_END_TICKS),
            Some(())
        );
        let pos = BlockPos::new(
            containing_chunk.0.x * BLOCKS_PER_CHUNK_EDGE + LAST_BLOCK_OFFSET_IN_CHUNK,
            world.get_min_y(),
            containing_chunk.0.y * BLOCKS_PER_CHUNK_EDGE,
        );

        let actual = world.current_difficulty_at(pos);
        let expected = DifficultyInstance::new(
            Difficulty::Hard,
            GLOBAL_RAMP_END_TICKS,
            containing_inhabited_time,
            MoonPhase::WaxingGibbous.brightness(),
        );

        assert_eq!(actual.difficulty(), Difficulty::Hard);
        assert_f32_close(
            actual.effective_difficulty(),
            expected.effective_difficulty(),
        );
        let Some(containing_full_chunk) = containing_holder.try_full_chunk() else {
            panic!("containing test chunk should remain Full");
        };
        assert_eq!(
            containing_full_chunk.common().inhabited_time(),
            containing_inhabited_time
        );
    }

    #[test]
    fn world_lookup_uses_zero_local_inputs_when_chunk_is_not_loaded() {
        let world = fresh_test_world("unloaded_local_difficulty");
        assert_eq!(
            world.set_clock_total_ticks(
                &vanilla_world_clocks::OVERWORLD,
                FULL_MOON_AFTER_GLOBAL_RAMP_TICKS,
            ),
            Some(())
        );
        let unloaded_pos = BlockPos::new(0, world.get_min_y(), 0);
        assert_f32_close(world.moon_brightness_at(unloaded_pos), FULL_MOON_BRIGHTNESS);

        let actual = world.current_difficulty_at(unloaded_pos);
        let expected = DifficultyInstance::new(
            Difficulty::Normal,
            FULL_MOON_AFTER_GLOBAL_RAMP_TICKS,
            NO_INHABITED_TIME,
            NO_MOON_BRIGHTNESS,
        );

        assert_f32_close(
            actual.effective_difficulty(),
            expected.effective_difficulty(),
        );
    }
}
