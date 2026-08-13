//! Shared vanilla `Enemy` and `Monster` capabilities.

use steel_protocol::packets::game::SoundSource;
use steel_registry::dimension_type::DimensionTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_game_rules::MOB_DROPS;
use steel_registry::{sound_events, vanilla_items};
use steel_utils::BlockPos;
use steel_utils::random::Random;

use crate::behavior::ITEM_BEHAVIORS;
use crate::chunk::light::LightLayer;
use crate::entity::{EntitySpawnReason, LivingEntity, MobBase, PathfinderMob};
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader, World};

/// No experience reward.
pub const XP_REWARD_NONE: i32 = 0;
/// Vanilla small hostile experience reward.
pub const XP_REWARD_SMALL: i32 = 3;
/// Vanilla default monster experience reward.
pub const XP_REWARD_MEDIUM: i32 = 5;
/// Vanilla large hostile experience reward.
pub const XP_REWARD_LARGE: i32 = 10;
/// Vanilla huge hostile experience reward.
pub const XP_REWARD_HUGE: i32 = 20;
/// Vanilla boss experience reward unit.
pub const XP_REWARD_BOSS: i32 = 50;

const SKY_LIGHT_RANDOM_BOUND: i32 = 32;
const MAX_BLOCK_LIGHT_LEVEL: i32 = 15;
const THUNDER_SKY_DARKENING: u8 = 10;
const BRIGHT_LIGHT_MAGIC_THRESHOLD: f32 = 0.5;
const BRIGHT_LIGHT_NO_ACTION_TIME_INCREASE: i32 = 2;

/// Marker capability matching vanilla `Enemy` class checks.
pub trait Enemy: LivingEntity {}

/// Level inputs required by vanilla monster light spawn rules.
pub trait MonsterSpawnLevel: LevelReader {
    /// Returns vanilla light-layer brightness at `pos`.
    fn brightness(&self, layer: LightLayer, pos: BlockPos) -> u8;

    /// Returns the level's dimension type.
    fn monster_spawn_dimension_type(&self) -> DimensionTypeRef;

    /// Returns whether the level is currently thundering.
    fn is_monster_spawn_thundering(&self) -> bool;
}

impl MonsterSpawnLevel for World {
    fn brightness(&self, layer: LightLayer, pos: BlockPos) -> u8 {
        self.light_value_at(layer, pos)
    }

    fn monster_spawn_dimension_type(&self) -> DimensionTypeRef {
        self.dimension_type
    }

    fn is_monster_spawn_thundering(&self) -> bool {
        self.is_thundering()
    }
}

/// Stateless constructor and static behavior owned by vanilla `Monster`.
#[derive(Debug, Default)]
pub struct MonsterBase;

impl MonsterBase {
    /// Initializes the shared state written by the vanilla `Monster` constructor.
    #[must_use]
    pub fn new(mob_base: &MobBase) -> Self {
        mob_base.set_xp_reward(XP_REWARD_MEDIUM);
        Self
    }

    /// Applies vanilla's light-dependent no-action-time adjustment.
    pub fn update_no_action_time<M: Monster + ?Sized>(monster: &M, light_magic_value: f32) {
        if light_magic_value > BRIGHT_LIGHT_MAGIC_THRESHOLD {
            monster.set_no_action_time(
                monster.no_action_time() + BRIGHT_LIGHT_NO_ACTION_TIME_INCREASE,
            );
        }
    }

    /// Returns vanilla `Monster.getWalkTargetValue` for an explicit level view.
    #[must_use]
    pub fn walk_target_value(level: &dyn LevelReader, pos: BlockPos) -> f32 {
        -level.pathfinding_cost_from_light_levels(pos)
    }

    /// Returns vanilla `Monster.isDarkEnoughToSpawn`.
    pub fn is_dark_enough_to_spawn(
        level: &dyn MonsterSpawnLevel,
        pos: BlockPos,
        random: &mut impl Random,
    ) -> bool {
        if i32::from(level.brightness(LightLayer::Sky, pos))
            > random.next_i32_bounded(SKY_LIGHT_RANDOM_BOUND)
        {
            return false;
        }

        let dimension_type = level.monster_spawn_dimension_type();
        let block_light_limit = dimension_type.monster_spawn_block_light_limit;
        if block_light_limit < MAX_BLOCK_LIGHT_LEVEL
            && i32::from(level.brightness(LightLayer::Block, pos)) > block_light_limit
        {
            return false;
        }

        let sky_darkening = if level.is_monster_spawn_thundering() {
            THUNDER_SKY_DARKENING
        } else {
            0
        };
        let brightness = level.max_local_raw_brightness(pos, sky_darkening);
        i32::from(brightness) <= dimension_type.monster_spawn_light_level.sample(random)
    }

    /// Returns vanilla `Monster.checkMonsterSpawnRules` while preserving the
    /// Mob-owned placement predicate as a caller-supplied decision.
    pub fn check_monster_spawn_rules(
        level: &dyn MonsterSpawnLevel,
        spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        random: &mut impl Random,
        check_mob_spawn_rules: impl FnOnce() -> bool,
    ) -> bool {
        (spawn_reason.ignores_light_requirements()
            || Self::is_dark_enough_to_spawn(level, pos, random))
            && check_mob_spawn_rules()
    }

    /// Returns vanilla `Monster.checkAnyLightMonsterSpawnRules` while keeping
    /// its Mob-owned placement predicate with the caller.
    pub fn check_any_light_monster_spawn_rules(
        check_mob_spawn_rules: impl FnOnce() -> bool,
    ) -> bool {
        check_mob_spawn_rules()
    }

    /// Returns vanilla `Monster.checkSurfaceMonstersSpawnRules`.
    pub fn check_surface_monster_spawn_rules(
        level: &dyn MonsterSpawnLevel,
        spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        random: &mut impl Random,
        check_mob_spawn_rules: impl FnOnce() -> bool,
    ) -> bool {
        Self::check_monster_spawn_rules(level, spawn_reason, pos, random, check_mob_spawn_rules)
            && (spawn_reason.is_spawner() || level.can_see_sky(pos))
    }
}

/// Reusable behavior matching vanilla's `Monster` base class.
pub trait Monster: PathfinderMob + Enemy {
    /// Runs the Monster-owned portion of vanilla `Monster.aiStep`.
    fn ai_step_monster(&self) -> Option<MoveResult> {
        self.update_no_action_time_monster();
        self.default_ai_step()
    }

    /// Runs vanilla `Monster.updateNoActionTime`.
    fn update_no_action_time_monster(&self) {
        MonsterBase::update_no_action_time(self, self.light_level_dependent_magic_value());
    }

    /// Returns vanilla `Monster.getWalkTargetValue`.
    fn monster_walk_target_value(&self, pos: BlockPos) -> f32 {
        self.level().map_or(0.0, |level| {
            MonsterBase::walk_target_value(level.as_ref(), pos)
        })
    }

    /// Returns vanilla `Monster.isPreventingPlayerRest`.
    fn is_preventing_player_rest(&self, _level: &World, _player: &Player) -> bool {
        true
    }

    /// Returns vanilla `Monster.getProjectile`.
    fn monster_projectile(&self, held_weapon: &ItemStack) -> ItemStack {
        let Some(projectile_weapon) = ITEM_BEHAVIORS
            .get_behavior(held_weapon.item())
            .as_projectile_weapon()
        else {
            return ItemStack::empty();
        };

        for slot in [EquipmentSlot::OffHand, EquipmentSlot::MainHand] {
            let mut projectile = ItemStack::empty();
            self.with_equipment_slot(slot, &mut |item_stack| {
                if projectile_weapon.supports_held_projectile(item_stack) {
                    projectile = item_stack.copy_with_count(item_stack.count());
                }
            });
            if !projectile.is_empty() {
                return projectile;
            }
        }

        ItemStack::new(&vanilla_items::ARROW)
    }

    /// Returns vanilla `Monster.getSoundSource`.
    fn monster_sound_source(&self) -> SoundSource {
        SoundSource::Hostile
    }

    /// Returns vanilla `Monster.getSwimSound`.
    fn monster_swim_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_HOSTILE_SWIM
    }

    /// Returns vanilla `Monster.getSwimSplashSound`.
    fn monster_swim_splash_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_HOSTILE_SPLASH
    }

    /// Returns vanilla `Monster.getHurtSound`.
    fn monster_hurt_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_HOSTILE_HURT
    }

    /// Returns vanilla `Monster.getDeathSound`.
    fn monster_death_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_HOSTILE_DEATH
    }

    /// Returns vanilla `Monster.getFallSounds`.
    fn monster_fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
        (
            &sound_events::ENTITY_HOSTILE_SMALL_FALL,
            &sound_events::ENTITY_HOSTILE_BIG_FALL,
        )
    }

    /// Returns vanilla `Monster.shouldDropExperience`.
    fn monster_should_drop_experience(&self) -> bool {
        true
    }

    /// Returns vanilla `Monster.shouldDropLoot`.
    fn monster_should_drop_loot(&self, world: &World) -> bool {
        world.get_game_rule(&MOB_DROPS)
    }
}

#[cfg(test)]
pub(crate) mod tests;
