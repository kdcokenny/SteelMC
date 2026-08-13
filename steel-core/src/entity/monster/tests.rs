use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_protocol::packets::game::SoundSource;
use steel_registry::dimension_type::DimensionTypeRef;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    REGISTRY, init_vanilla_registry, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_damage_types, vanilla_dimension_types, vanilla_entities, vanilla_game_rules,
    vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, BlockStateId};

use super::*;
use crate::behavior::init_behaviors;
use crate::entity::damage::DamageSource;
use crate::entity::entities::PigEntity;
use crate::entity::spawn::entity_type_allowed_in_difficulty;
use crate::entity::{Entity, EntityBase, LivingEntityBase, Mob, SharedEntity};
use crate::test_support::{fresh_test_world, test_world};

const TEST_ENTITY_HEALTH: f32 = 20.0;
const CONFIGURED_ATTACK_DAMAGE: f64 = 4.0;
const DARK_LIGHT_MAGIC_VALUE: f32 = 0.5;
const BRIGHT_LIGHT_MAGIC_VALUE: f32 = 0.51;
const EXPECTED_BRIGHT_LIGHT_IDLE_INCREASE: i32 = 2;
const MIN_LIGHT_LEVEL: u8 = 0;
const MAX_LIGHT_LEVEL: u8 = 15;
const DETERMINISTIC_SPAWN_RANDOM_SEED: u64 = 26_02;
const TEST_MONSTER_ENTITY_ID: i32 = 1;
const TEST_TARGET_ENTITY_ID: i32 = 2;
const TEST_SPAWN_Y: i32 = 64;
const END_BLOCK_LIGHT_ABOVE_LIMIT: u8 = 1;
const CLEAR_SKY_RAW_BRIGHTNESS: u8 = THUNDER_SKY_DARKENING;

pub(crate) struct TestMonster {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    _monster_base: MonsterBase,
    mob_flags: SyncMutex<i8>,
    health: SyncMutex<f32>,
}

impl TestMonster {
    pub(crate) fn detached(id: i32, position: DVec3) -> Self {
        Self::new(id, position, &vanilla_entities::ZOMBIE, Weak::new())
    }

    pub(crate) fn in_world(id: i32, position: DVec3, world: &Arc<World>) -> Self {
        Self::new(
            id,
            position,
            &vanilla_entities::ZOMBIE,
            Arc::downgrade(world),
        )
    }

    fn new(id: i32, position: DVec3, entity_type: EntityTypeRef, world: Weak<World>) -> Self {
        init_vanilla_registry();
        let mob_base = MobBase::new();
        let monster_base = MonsterBase::new(&mob_base);
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            living_base: LivingEntityBase::new(entity_type),
            mob_base,
            _monster_base: monster_base,
            mob_flags: SyncMutex::new(0),
            health: SyncMutex::new(TEST_ENTITY_HEALTH),
        }
    }

    fn equip(&self, slot: EquipmentSlot, stack: ItemStack) {
        let mut stack = Some(stack);
        self.with_equipment_slot_mut(slot, &mut |current| {
            if let Some(stack) = stack.take() {
                *current = stack;
            }
        });
    }
}

crate::entity::impl_test_downcast_type!(TestMonster);

impl Entity for TestMonster {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        LivingEntity::hurt_server(self, world, source, amount)
    }
}

impl LivingEntity for TestMonster {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.health.lock()
    }

    fn set_health(&self, health: f32) {
        *self.health.lock() = health;
    }
}

impl Mob for TestMonster {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_flags(&self) -> i8 {
        *self.mob_flags.lock()
    }

    fn set_mob_flags(&self, flags: i8) {
        *self.mob_flags.lock() = flags;
    }
}

impl PathfinderMob for TestMonster {}
impl Enemy for TestMonster {}
impl Monster for TestMonster {}

struct SpawnRuleLevel {
    dimension_type: DimensionTypeRef,
    sky_light: u8,
    block_light: u8,
    raw_brightness: u8,
    thundering: bool,
    sky_visible: bool,
}

impl SpawnRuleLevel {
    fn new(dimension_type: DimensionTypeRef) -> Self {
        Self {
            dimension_type,
            sky_light: MIN_LIGHT_LEVEL,
            block_light: MIN_LIGHT_LEVEL,
            raw_brightness: MIN_LIGHT_LEVEL,
            thundering: false,
            sky_visible: false,
        }
    }

    fn with_block_light(mut self, block_light: u8) -> Self {
        self.block_light = block_light;
        self
    }

    fn with_raw_brightness(mut self, raw_brightness: u8) -> Self {
        self.raw_brightness = raw_brightness;
        self
    }

    fn with_sky_visible(mut self, sky_visible: bool) -> Self {
        self.sky_visible = sky_visible;
        self
    }

    fn with_thundering(mut self, thundering: bool) -> Self {
        self.thundering = thundering;
        self
    }
}

impl LevelReader for SpawnRuleLevel {
    fn get_block_state(&self, _pos: BlockPos) -> BlockStateId {
        REGISTRY.blocks.get_default_state_id(&vanilla_blocks::AIR)
    }

    fn raw_brightness(&self, _pos: BlockPos, sky_darkening: u8) -> u8 {
        self.raw_brightness.saturating_sub(sky_darkening)
    }

    fn can_see_sky(&self, _pos: BlockPos) -> bool {
        self.sky_visible
    }

    fn min_y(&self) -> i32 {
        self.dimension_type.min_y
    }

    fn height(&self) -> i32 {
        self.dimension_type.height
    }
}

impl MonsterSpawnLevel for SpawnRuleLevel {
    fn brightness(&self, layer: LightLayer, _pos: BlockPos) -> u8 {
        match layer {
            LightLayer::Sky => self.sky_light,
            LightLayer::Block => self.block_light,
        }
    }

    fn monster_spawn_dimension_type(&self) -> DimensionTypeRef {
        self.dimension_type
    }

    fn is_monster_spawn_thundering(&self) -> bool {
        self.thundering
    }
}

#[test]
fn enemy_and_monster_recognition_uses_capabilities() {
    init_vanilla_registry();
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);
    let pig = PigEntity::new(
        &vanilla_entities::PIG,
        TEST_TARGET_ENTITY_ID,
        DVec3::ZERO,
        Weak::new(),
    );

    assert!(monster.is_enemy());
    assert!(monster.is_monster());
    assert!(!pig.is_enemy());
    assert!(!pig.is_monster());
}

#[test]
fn monster_constructor_sets_default_experience_and_attack_damage_is_available() {
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);

    assert_eq!(monster.xp_reward(), XP_REWARD_MEDIUM);
    assert!(
        monster
            .attributes()
            .lock()
            .get_value(vanilla_attributes::ATTACK_DAMAGE)
            .is_some()
    );
}

#[test]
fn monster_sound_defaults_use_the_hostile_category_and_events() {
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);
    let damage_source = DamageSource::environment(&vanilla_damage_types::GENERIC);

    assert_eq!(monster.sound_source(), SoundSource::Hostile);
    assert_eq!(monster.swim_sound(), &sound_events::ENTITY_HOSTILE_SWIM);
    assert_eq!(
        monster.swim_splash_sound(),
        &sound_events::ENTITY_HOSTILE_SPLASH
    );
    assert_eq!(
        monster.hurt_sound(&damage_source),
        Some(&sound_events::ENTITY_HOSTILE_HURT)
    );
    assert_eq!(
        monster.death_sound(),
        Some(&sound_events::ENTITY_HOSTILE_DEATH)
    );
    assert_eq!(
        monster.fall_sounds(),
        (
            &sound_events::ENTITY_HOSTILE_SMALL_FALL,
            &sound_events::ENTITY_HOSTILE_BIG_FALL
        )
    );
    assert!(monster.ambient_sound().is_none());
}

#[test]
fn monster_no_action_time_only_gets_the_extra_in_bright_light() {
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);

    MonsterBase::update_no_action_time(&monster, DARK_LIGHT_MAGIC_VALUE);
    assert_eq!(monster.no_action_time(), 0);

    MonsterBase::update_no_action_time(&monster, BRIGHT_LIGHT_MAGIC_VALUE);
    assert_eq!(
        monster.no_action_time(),
        EXPECTED_BRIGHT_LIGHT_IDLE_INCREASE
    );
}

#[test]
fn monster_walk_target_scoring_prefers_dark_positions() {
    let pos = BlockPos::new(0, TEST_SPAWN_Y, 0);
    let dark =
        SpawnRuleLevel::new(&vanilla_dimension_types::THE_END).with_raw_brightness(MIN_LIGHT_LEVEL);
    let bright =
        SpawnRuleLevel::new(&vanilla_dimension_types::THE_END).with_raw_brightness(MAX_LIGHT_LEVEL);

    assert!(
        MonsterBase::walk_target_value(&dark, pos) > MonsterBase::walk_target_value(&bright, pos)
    );
}

#[test]
fn monster_spawn_light_rules_reject_block_light_and_allow_darkness() {
    let pos = BlockPos::new(0, TEST_SPAWN_Y, 0);
    let dark =
        SpawnRuleLevel::new(&vanilla_dimension_types::THE_END).with_raw_brightness(MAX_LIGHT_LEVEL);
    let bright = SpawnRuleLevel::new(&vanilla_dimension_types::THE_END)
        .with_block_light(END_BLOCK_LIGHT_ABOVE_LIMIT);
    let mut dark_random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);
    let mut bright_random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);

    assert!(MonsterBase::check_monster_spawn_rules(
        &dark,
        EntitySpawnReason::Natural,
        pos,
        &mut dark_random,
        || true,
    ));
    assert!(!MonsterBase::check_monster_spawn_rules(
        &bright,
        EntitySpawnReason::Natural,
        pos,
        &mut bright_random,
        || true,
    ));
}

#[test]
fn thunder_darkening_is_applied_to_monster_spawn_brightness() {
    let pos = BlockPos::new(0, TEST_SPAWN_Y, 0);
    let clear = SpawnRuleLevel::new(&vanilla_dimension_types::OVERWORLD)
        .with_raw_brightness(CLEAR_SKY_RAW_BRIGHTNESS);
    let thundering = SpawnRuleLevel::new(&vanilla_dimension_types::OVERWORLD)
        .with_raw_brightness(CLEAR_SKY_RAW_BRIGHTNESS)
        .with_thundering(true);
    let mut clear_random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);
    let mut thunder_random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);

    assert!(!MonsterBase::is_dark_enough_to_spawn(
        &clear,
        pos,
        &mut clear_random,
    ));
    assert!(MonsterBase::is_dark_enough_to_spawn(
        &thundering,
        pos,
        &mut thunder_random,
    ));
}

#[test]
fn trial_spawner_and_any_light_helpers_preserve_mob_placement_rules() {
    let pos = BlockPos::new(0, TEST_SPAWN_Y, 0);
    let bright =
        SpawnRuleLevel::new(&vanilla_dimension_types::THE_END).with_block_light(MAX_LIGHT_LEVEL);
    let mut random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);

    assert!(MonsterBase::check_monster_spawn_rules(
        &bright,
        EntitySpawnReason::TrialSpawner,
        pos,
        &mut random,
        || true,
    ));
    assert!(MonsterBase::check_any_light_monster_spawn_rules(|| true));
    assert!(!MonsterBase::check_any_light_monster_spawn_rules(|| false));
}

#[test]
fn surface_monster_helper_requires_sky_visibility_outside_spawners() {
    let pos = BlockPos::new(0, TEST_SPAWN_Y, 0);
    let covered =
        SpawnRuleLevel::new(&vanilla_dimension_types::THE_END).with_raw_brightness(MAX_LIGHT_LEVEL);
    let exposed = SpawnRuleLevel::new(&vanilla_dimension_types::THE_END)
        .with_raw_brightness(MAX_LIGHT_LEVEL)
        .with_sky_visible(true);
    let mut covered_random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);
    let mut exposed_random = LegacyRandom::from_seed(DETERMINISTIC_SPAWN_RANDOM_SEED);

    assert!(!MonsterBase::check_surface_monster_spawn_rules(
        &covered,
        EntitySpawnReason::Natural,
        pos,
        &mut covered_random,
        || true,
    ));
    assert!(MonsterBase::check_surface_monster_spawn_rules(
        &exposed,
        EntitySpawnReason::Natural,
        pos,
        &mut exposed_random,
        || true,
    ));
}

#[test]
fn hostile_type_difficulty_gate_rejects_peaceful_before_spawn_predicates() {
    assert!(!entity_type_allowed_in_difficulty(
        &vanilla_entities::ZOMBIE,
        Difficulty::Peaceful,
    ));
    assert!(entity_type_allowed_in_difficulty(
        &vanilla_entities::ZOMBIE,
        Difficulty::Normal,
    ));
}

#[test]
fn monster_projectile_selection_uses_weapon_capability_and_hand_order() {
    init_vanilla_registry();
    init_behaviors();
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);
    monster.equip(
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::ARROW),
    );
    monster.equip(
        EquipmentSlot::OffHand,
        ItemStack::new(&vanilla_items::FIREWORK_ROCKET),
    );

    let bow_projectile = monster.get_projectile(&ItemStack::new(&vanilla_items::BOW));
    assert!(bow_projectile.is(&vanilla_items::ARROW));

    let crossbow_projectile = monster.get_projectile(&ItemStack::new(&vanilla_items::CROSSBOW));
    assert!(crossbow_projectile.is(&vanilla_items::FIREWORK_ROCKET));

    let non_weapon_projectile = monster.get_projectile(&ItemStack::new(&vanilla_items::IRON_SWORD));
    assert!(non_weapon_projectile.is_empty());
}

#[test]
fn monster_projectile_selection_falls_back_to_a_vanilla_arrow() {
    init_vanilla_registry();
    init_behaviors();
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);

    let projectile = monster.get_projectile(&ItemStack::new(&vanilla_items::BOW));

    assert!(projectile.is(&vanilla_items::ARROW));
}

#[test]
fn monster_melee_attack_uses_attack_damage_and_reports_success() {
    init_vanilla_registry();
    init_behaviors();
    let attacker = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);
    attacker
        .attributes()
        .lock()
        .set_base_value(vanilla_attributes::ATTACK_DAMAGE, CONFIGURED_ATTACK_DAMAGE);
    let target = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        TEST_TARGET_ENTITY_ID,
        DVec3::X,
        Weak::new(),
    ));
    let starting_health = target.get_health();
    let target_entity = Arc::<PigEntity>::clone(&target) as SharedEntity;

    assert!(attacker.do_hurt_target(test_world(), &target_entity));
    assert_eq!(
        target.get_health().to_bits(),
        (starting_health - CONFIGURED_ATTACK_DAMAGE as f32).to_bits()
    );
}

#[test]
fn monster_melee_attack_reports_failure_when_damage_is_rejected() {
    init_vanilla_registry();
    init_behaviors();
    let attacker = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);
    let target = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        TEST_TARGET_ENTITY_ID,
        DVec3::X,
        Weak::new(),
    ));
    let starting_health = target.get_health();
    target.set_invulnerable(true);
    let target_entity = Arc::<PigEntity>::clone(&target) as SharedEntity;

    assert!(!attacker.do_hurt_target(test_world(), &target_entity));
    assert_eq!(target.get_health().to_bits(), starting_health.to_bits());
}

#[test]
fn enemy_leash_rejection_runs_through_the_mob_attachment_decision() {
    init_vanilla_registry();
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);
    let holder = PigEntity::new(
        &vanilla_entities::PIG,
        TEST_TARGET_ENTITY_ID,
        DVec3::ZERO,
        Weak::new(),
    );

    assert!(!monster.can_have_a_leash_attached_to(&holder));
}

#[test]
fn monster_drop_rules_follow_mob_drops_and_always_allow_experience() {
    init_vanilla_registry();
    let world = fresh_test_world("monster_drop_rules");
    let monster = TestMonster::detached(TEST_MONSTER_ENTITY_ID, DVec3::ZERO);

    assert!(monster.should_drop_experience());
    assert!(world.set_game_rule(&vanilla_game_rules::MOB_DROPS, false));
    assert!(!monster.should_drop_loot(&world));
    assert!(world.set_game_rule(&vanilla_game_rules::MOB_DROPS, true));
    assert!(monster.should_drop_loot(&world));
}
