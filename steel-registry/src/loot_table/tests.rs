use crate::data_components::vanilla_components::{CHICKEN_VARIANT, INSTRUMENT, SHEEP_COLOR};
use crate::data_components::{
    Component, ComponentEntryRef, DataComponentGetter, DataComponentMap, DataComponentType,
    DataComponentValue,
};
use crate::vanilla_instrument_tags::InstrumentTag;
use crate::vanilla_items;
use crate::{
    DyeColor, RegistryReference, init_vanilla_registry, vanilla_chicken_variants,
    vanilla_loot_tables,
};
use steel_utils::DowncastType;

use super::*;
use rand::SeedableRng;

fn test_rng() -> rand::rngs::StdRng {
    rand::rngs::StdRng::seed_from_u64(12345)
}

fn init_test_registries() {
    init_vanilla_registry();
}

const SHEEP_DYE_COLOR_COUNT: usize = 16;
static CHICKEN_LAY_RANDOM_SEQUENCE: Identifier = Identifier::vanilla_static("gameplay/chicken_lay");

#[derive(Default)]
struct EntityComponentFixture {
    components: DataComponentMap,
}

impl EntityComponentFixture {
    fn with<T: Component + DowncastType>(
        mut self,
        component: DataComponentType<T>,
        value: T,
    ) -> Self {
        self.components.set(component, Some(value));
        self
    }
}

impl DataComponentGetter for EntityComponentFixture {
    fn get_data_component(&self, component: ComponentEntryRef) -> Option<DataComponentValue<'_>> {
        self.components.get_data_component(component)
    }
}

fn component_entity(
    components: &EntityComponentFixture,
    sheep_sheared: Option<bool>,
) -> EntityRef<'_> {
    EntityRef {
        entity_type: None,
        flags: EntityRefFlags::default(),
        equipment: None,
        custom_name: None,
        components: Some(components),
        sheep_sheared,
    }
}

fn exact_component_condition(
    serialized: &'static [SerializedEntityDataComponent],
    sheep_sheared: Option<bool>,
) -> LootCondition {
    LootCondition::EntityProperties {
        entity: LootContextEntity::This,
        predicate: EntityPredicate {
            entity_type: None,
            flags: None,
            equipment: None,
            components: Some(EntityExactDataComponentsPredicate::new(serialized)),
            sheep_sheared,
        },
    }
}

static WHITE_SHEEP_COMPONENT: &[SerializedEntityDataComponent] = &[SerializedEntityDataComponent {
    component: "minecraft:sheep/color",
    value: "\"white\"",
}];

static WHITE_SHEEP_AND_TEMPERATE_CHICKEN_COMPONENTS: &[SerializedEntityDataComponent] = &[
    SerializedEntityDataComponent {
        component: "minecraft:sheep/color",
        value: "\"white\"",
    },
    SerializedEntityDataComponent {
        component: "minecraft:chicken/variant",
        value: "\"minecraft:temperate\"",
    },
];

static UNKNOWN_ENTITY_COMPONENT: &[SerializedEntityDataComponent] =
    &[SerializedEntityDataComponent {
        component: "minecraft:not_registered",
        value: "true",
    }];

static INVALID_SHEEP_COLOR: &[SerializedEntityDataComponent] = &[SerializedEntityDataComponent {
    component: "minecraft:sheep/color",
    value: "\"not_a_color\"",
}];

#[test]
fn test_oak_log_loot() {
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_OAK_LOG.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    assert_eq!(items[0].item.key, Identifier::vanilla_static("oak_log"));
}

#[test]
fn set_instrument_selects_from_the_configured_holder_set() {
    init_test_registries();
    let mut rng = test_rng();
    let mut ctx = LootContext::new(&mut rng);
    let mut goat_horn = ItemStack::new(&vanilla_items::GOAT_HORN);
    let function = LootFunction::SetInstrument {
        options: InstrumentOptions::Tag(InstrumentTag::REGULAR_GOAT_HORNS),
    };

    function.apply(&mut goat_horn, &mut ctx);

    let selected = goat_horn
        .get(INSTRUMENT)
        .and_then(|component| component.instrument().as_reference())
        .expect("set_instrument should select a registered instrument");
    assert!(
        REGISTRY
            .instruments
            .is_in_tag(selected, &InstrumentTag::REGULAR_GOAT_HORNS)
    );
}

#[test]
fn test_diamond_ore_loot_no_silk_touch() {
    // Without silk touch, diamond ore should drop diamond (not the ore block)
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_DIAMOND_ORE.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    // Without silk touch enchantment, diamond ore drops diamond
    assert_eq!(items[0].item.key, Identifier::vanilla_static("diamond"));
}

#[test]
fn test_grass_block_loot_no_silk_touch() {
    // Without silk touch, grass block should drop dirt
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_GRASS_BLOCK.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    // Without silk touch, grass block drops dirt
    assert_eq!(items[0].item.key, Identifier::vanilla_static("dirt"));
}

#[test]
fn test_stone_loot_no_silk_touch() {
    // Without silk touch, stone should drop cobblestone
    init_test_registries();
    let mut rng = test_rng();

    let mut ctx = LootContext::new(&mut rng);
    let items = vanilla_loot_tables::BLOCKS_STONE.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].count, 1);
    // Without silk touch, stone drops cobblestone
    assert_eq!(items[0].item.key, Identifier::vanilla_static("cobblestone"));
}

#[test]
fn test_pig_loot_drops_raw_porkchop_when_not_on_fire() {
    init_test_registries();
    let mut rng = test_rng();
    let pig_key = Identifier::vanilla_static("pig");
    let pig = EntityRef {
        entity_type: Some(&pig_key),
        flags: EntityRefFlags::default(),
        equipment: None,
        custom_name: None,
        components: None,
        sheep_sheared: None,
    };

    let mut ctx = LootContext::new(&mut rng).with_this_entity(pig);
    let items = vanilla_loot_tables::ENTITIES_PIG.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.key, Identifier::vanilla_static("porkchop"));
    assert!((1..=3).contains(&items[0].count));
}

#[test]
fn shearing_sheep_table_parses_the_flat_type_specific_sheep_key() {
    init_test_registries();

    let table = &vanilla_loot_tables::SHEARING_SHEEP;
    let pool = table
        .pools
        .first()
        .expect("shearing table should have a pool");
    let LootEntry::Alternatives { children, .. } = pool
        .entries
        .first()
        .expect("shearing pool should start with alternatives")
    else {
        panic!("shearing pool should use an alternatives entry");
    };

    let mut checked = 0;
    for child in *children {
        let LootEntry::LootTableRef { conditions, .. } = child else {
            continue;
        };
        let Some(LootCondition::EntityProperties { predicate, .. }) = conditions.first() else {
            continue;
        };
        assert!(
            predicate
                .components
                .as_ref()
                .is_some_and(EntityExactDataComponentsPredicate::is_valid),
            "branch should match its wool color"
        );
        assert_eq!(
            predicate.sheep_sheared,
            Some(false),
            "sheared must come from the flat minecraft:type_specific/sheep key"
        );
        checked += 1;
    }
    assert_eq!(
        checked, SHEEP_DYE_COLOR_COUNT,
        "all sixteen color branches should carry the sheared predicate"
    );
}

#[test]
fn sheared_predicate_rejects_non_sheep_entities() {
    init_test_registries();
    let mut rng = test_rng();
    let pig_key = Identifier::vanilla_static("pig");
    let pig = EntityRef {
        entity_type: Some(&pig_key),
        flags: EntityRefFlags::default(),
        equipment: None,
        custom_name: None,
        components: None,
        sheep_sheared: None,
    };
    let mut ctx = LootContext::new(&mut rng).with_this_entity(pig);

    let condition = LootCondition::EntityProperties {
        entity: LootContextEntity::This,
        predicate: EntityPredicate {
            entity_type: None,
            flags: None,
            equipment: None,
            components: None,
            sheep_sheared: Some(false),
        },
    };
    assert!(
        !condition.test(&mut ctx),
        "a non-sheep entity must fail a sheared predicate, mirroring SheepPredicate.matches"
    );
}

#[test]
fn sheep_color_and_sheared_predicates_remain_independent() {
    init_test_registries();
    let white = EntityComponentFixture::default().with(SHEEP_COLOR, DyeColor::White);
    let black = EntityComponentFixture::default().with(SHEEP_COLOR, DyeColor::Black);
    let condition = exact_component_condition(WHITE_SHEEP_COMPONENT, Some(false));

    for (fixture, sheared, expected, behavior) in [
        (&white, false, true, "matching color and unsheared"),
        (&white, true, false, "matching color but sheared"),
        (&black, false, false, "wrong color but unsheared"),
    ] {
        let mut rng = test_rng();
        let mut context =
            LootContext::new(&mut rng).with_this_entity(component_entity(fixture, Some(sheared)));
        assert_eq!(condition.test(&mut context), expected, "{behavior}");
    }
}

#[test]
fn exact_entity_components_match_conjunctively_and_reject_missing_values() {
    init_test_registries();
    let complete = EntityComponentFixture::default()
        .with(SHEEP_COLOR, DyeColor::White)
        .with(
            CHICKEN_VARIANT,
            RegistryReference::new(&vanilla_chicken_variants::TEMPERATE),
        );
    let missing_chicken = EntityComponentFixture::default().with(SHEEP_COLOR, DyeColor::White);
    let wrong_chicken = EntityComponentFixture::default()
        .with(SHEEP_COLOR, DyeColor::White)
        .with(
            CHICKEN_VARIANT,
            RegistryReference::new(&vanilla_chicken_variants::WARM),
        );
    let condition = exact_component_condition(WHITE_SHEEP_AND_TEMPERATE_CHICKEN_COMPONENTS, None);

    for (fixture, expected, behavior) in [
        (&complete, true, "all component values match"),
        (&missing_chicken, false, "one required component is absent"),
        (&wrong_chicken, false, "one required component differs"),
    ] {
        let mut rng = test_rng();
        let mut context =
            LootContext::new(&mut rng).with_this_entity(component_entity(fixture, None));
        assert_eq!(condition.test(&mut context), expected, "{behavior}");
    }
}

#[test]
fn invalid_entity_component_keys_and_codec_values_fail_closed() {
    init_test_registries();
    let fixture = EntityComponentFixture::default().with(SHEEP_COLOR, DyeColor::White);

    for (serialized, behavior) in [
        (UNKNOWN_ENTITY_COMPONENT, "unregistered component type"),
        (INVALID_SHEEP_COLOR, "registered codec rejects value"),
    ] {
        let condition = exact_component_condition(serialized, None);
        let mut rng = test_rng();
        let mut context =
            LootContext::new(&mut rng).with_this_entity(component_entity(&fixture, None));
        assert!(!condition.test(&mut context), "{behavior}");
    }
}

#[test]
fn chicken_lay_uses_generic_chicken_variant_component_state() {
    init_test_registries();
    let table = &vanilla_loot_tables::GAMEPLAY_CHICKEN_LAY;

    assert_eq!(table.loot_type, LootType::Gift);
    assert_eq!(
        table.random_sequence.as_ref(),
        Some(&CHICKEN_LAY_RANDOM_SEQUENCE)
    );

    for (variant, expected_item) in [
        (&vanilla_chicken_variants::TEMPERATE, &vanilla_items::EGG),
        (&vanilla_chicken_variants::WARM, &vanilla_items::BROWN_EGG),
        (&vanilla_chicken_variants::COLD, &vanilla_items::BLUE_EGG),
    ] {
        let fixture = EntityComponentFixture::default()
            .with(CHICKEN_VARIANT, RegistryReference::new(variant));
        let mut rng = test_rng();
        let mut context = LootContext::new(&mut rng)
            .with_loot_type(LootType::Gift)
            .with_this_entity(component_entity(&fixture, None));

        let items = table.get_random_items(&mut context);

        assert_eq!(items.len(), 1, "{} variant returns one egg", variant.key);
        assert_eq!(items[0].item.key, expected_item.key, "{} egg", variant.key);
    }
}

#[test]
fn test_pig_loot_smelt_condition_uses_entity_fire_flag() {
    init_test_registries();
    let mut rng = test_rng();
    let pig_key = Identifier::vanilla_static("pig");
    let pig = EntityRef {
        entity_type: Some(&pig_key),
        flags: EntityRefFlags {
            is_on_fire: true,
            ..EntityRefFlags::default()
        },
        equipment: None,
        custom_name: None,
        components: None,
        sheep_sheared: None,
    };

    let mut ctx = LootContext::new(&mut rng).with_this_entity(pig);
    let items = vanilla_loot_tables::ENTITIES_PIG.get_random_items(&mut ctx);

    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].item.key,
        Identifier::vanilla_static("cooked_porkchop")
    );
    assert!((1..=3).contains(&items[0].count));
}

#[test]
fn test_uniform_get_int_reaches_inclusive_max() {
    // Vanilla UniformGenerator.getInt uses Mth.nextInt(rand, min, max), which
    // samples the integer range inclusively; a uniform 1..3 count must yield 3.
    let provider = NumberProvider::Uniform { min: 1.0, max: 3.0 };
    let mut seen = [false; 4];
    for seed in 0u64..1000 {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let value = provider.get_int(&mut rng);
        seen[value as usize] = true;
    }
    assert!(
        seen[1] && seen[2] && seen[3],
        "uniform 1..=3 must produce 1, 2 and 3, saw {seen:?}"
    );
}

#[test]
fn test_explosion_decay_function() {
    // Test the explosion_decay function directly
    init_test_registries();

    // explosion_decay reduces count based on 1/radius probability per item
    let cond_func = ConditionalLootFunction {
        function: LootFunction::ExplosionDecay,
        conditions: &[],
    };

    let mut total_survived = 0;
    let initial_count = 10;

    for seed in 0u64..100 {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut ctx = LootContext::new(&mut rng).with_explosion(4.0);
        let mut item = ItemStack::with_count(&crate::vanilla_items::STONE, initial_count);
        cond_func.function.apply(&mut item, &mut ctx);
        total_survived += item.count;
    }

    // With 10 items each trial, 100 trials = 1000 items total
    // Each has 25% (1/4.0) chance to survive = ~250 expected
    // Allow for variance: 150-350 range
    assert!(
        total_survived > 150 && total_survived < 350,
        "Expected ~250 items with explosion decay (25% of 1000), got {total_survived}"
    );
}

#[test]
fn ominous_bottle_amplifier_function_clamps_to_persistent_range() {
    use crate::data_components::vanilla_components::OMINOUS_BOTTLE_AMPLIFIER;

    init_test_registries();
    for (provided, expected) in [(-3.0, 0), (2.0, 2), (9.0, 4)] {
        let mut rng = test_rng();
        let mut context = LootContext::new(&mut rng);
        let mut item = ItemStack::new(&crate::vanilla_items::OMINOUS_BOTTLE);
        LootFunction::SetOminousBottleAmplifier {
            amplifier: NumberProvider::Constant(provided),
        }
        .apply(&mut item, &mut context);

        assert_eq!(
            item.get(OMINOUS_BOTTLE_AMPLIFIER)
                .map(|amplifier| amplifier.value()),
            Some(expected)
        );
    }
}

#[test]
fn test_survives_explosion_condition() {
    init_test_registries();

    // Test that survives_explosion condition works
    // Gravel has survives_explosion on its alternatives
    let mut survived = 0;
    for seed in 0..100 {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut ctx = LootContext::new(&mut rng).with_explosion(4.0);
        let items = vanilla_loot_tables::BLOCKS_GRAVEL.get_random_items(&mut ctx);
        if !items.is_empty() {
            survived += 1;
        }
    }

    // With radius 4.0, ~25% should survive
    assert!(
        survived > 10 && survived < 50,
        "Expected ~25% survival rate, got {survived}%"
    );
}
