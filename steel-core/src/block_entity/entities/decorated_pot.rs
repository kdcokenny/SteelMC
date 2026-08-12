//! Decorated pot block entity and its single-stack container.

use std::{
    io::Cursor,
    mem,
    str::FromStr as _,
    sync::{Arc, LazyLock, Weak},
};

use simdnbt::FromNbtTag as _;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::{
    BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView, read_compound,
};
use simdnbt::owned::NbtCompound;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, Direction};
use steel_registry::data_components::components::PotDecorations;
use steel_registry::data_components::vanilla_components::{
    BLOCK_ENTITY_DATA, CONTAINER, POT_DECORATIONS,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::{LootContext, LootRandom, LootTableRef};
#[cfg(test)]
use steel_registry::vanilla_blocks;
use steel_registry::{REGISTRY, RegistryExt as _, vanilla_block_entity_types, vanilla_items};
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{
    BlockPos, BlockStateId, DowncastType, DowncastTypeKey, Identifier, locks::SyncMutex,
};

use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::world::World;

const EVENT_POT_WOBBLES: i32 = 1;

/// Vanilla decorated-pot wobble styles and client animation durations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WobbleStyle {
    /// Successful insertion animation.
    Positive,
    /// Failed insertion animation.
    Negative,
}

impl WobbleStyle {
    /// Number of ticks the corresponding client animation lasts.
    #[must_use]
    pub const fn duration(self) -> i32 {
        match self {
            Self::Positive => 7,
            Self::Negative => 10,
        }
    }

    const fn event_data(self) -> i32 {
        match self {
            Self::Positive => 0,
            Self::Negative => 1,
        }
    }

    const fn from_event_data(data: i32) -> Option<Self> {
        match data {
            0 => Some(Self::Positive),
            1 => Some(Self::Negative),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecoratedPotWobble {
    pub started_at_tick: i64,
    pub style: WobbleStyle,
}

/// Independently lockable item and persistent decorated-pot data.
pub(crate) struct DecoratedPotContainer {
    items: [ItemStack; 1],
    decorations: PotDecorations,
    loot_table: Option<Identifier>,
    loot_table_seed: i64,
    wobble: Option<DecoratedPotWobble>,
}

/// Vanilla `DecoratedPotBlockEntity`.
pub struct DecoratedPotBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<DecoratedPotContainer>>,
    container_ref: ContainerRef,
}

// SAFETY: This key is owned by Steel and uniquely identifies `DecoratedPotBlockEntity`.
unsafe impl DowncastType for DecoratedPotBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/decorated_pot");
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable container data used by a decorated-pot block entity.
unsafe impl DowncastType for DecoratedPotContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/decorated_pot");
}

impl DecoratedPotBlockEntity {
    /// Creates a decorated pot with four brick sides and an empty item slot.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::DECORATED_POT,
            level,
            pos,
            state,
        ));
        let container = Arc::new(SyncMutex::new(DecoratedPotContainer {
            items: [ItemStack::empty()],
            decorations: PotDecorations::EMPTY,
            loot_table: None,
            loot_table_seed: 0,
            wobble: None,
        }));
        let shared_container: SharedContainer = container.clone();
        Self {
            container_ref: ContainerRef::owned_by_block_entity(shared_container, Arc::clone(&base)),
            base,
            container,
        }
    }

    /// Returns the pot direction used to orient its ordered sides.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.get_block_state()
            .get_value(&BlockStateProperties::HORIZONTAL_FACING)
    }

    /// Returns a stable snapshot of the four ordered decorations.
    #[must_use]
    pub fn decorations(&self) -> PotDecorations {
        self.container.lock().decorations.clone()
    }

    /// Creates the intact pot stack used by clone/pick and block loot.
    #[must_use]
    pub fn create_decorated_pot_instance(decorations: PotDecorations) -> ItemStack {
        let mut stack = ItemStack::new(&vanilla_items::DECORATED_POT);
        stack.set(POT_DECORATIONS, decorations);
        stack
    }

    /// Applies Vanilla's implicit pot components from a placing stack.
    pub fn apply_components_from_item(&self, stack: &ItemStack) {
        if let Some(data) = stack.get(BLOCK_ENTITY_DATA)
            && data.block_entity_type().key == self.get_type().key
        {
            let nbt = data.data().copy_tag();
            let mut bytes = Vec::new();
            nbt.write(&mut bytes);
            match read_compound(&mut Cursor::new(bytes.as_slice())) {
                Ok(nbt) => self.load_additional(&nbt),
                Err(error) => tracing::warn!(
                    %error,
                    "failed to apply decorated-pot block-entity item data"
                ),
            }
        }

        let decorations = stack
            .get(POT_DECORATIONS)
            .cloned()
            .unwrap_or(PotDecorations::EMPTY);
        let item = stack
            .get(CONTAINER)
            .map_or_else(ItemStack::empty, |contents| {
                contents
                    .items()
                    .first()
                    .and_then(Option::as_ref)
                    .map_or_else(ItemStack::empty, steel_registry::ItemStackTemplate::create)
            });

        {
            let mut container = self.container.lock();
            container.decorations = decorations;
            container.items[0] = item;
        }
        self.set_changed();
    }

    /// Returns the item after lazily filling a saved loot table, if present.
    #[must_use]
    pub fn item(&self) -> ItemStack {
        self.unpack_loot_table();
        let container = self.container.lock();
        container.items[0].copy_with_count(container.items[0].count())
    }

    /// Replaces the single stored stack.
    pub fn set_item(&self, item: ItemStack) {
        self.unpack_loot_table();
        self.container.lock().items[0] = item;
        self.set_changed();
    }

    /// Returns the latest dispatched wobble event.
    #[must_use]
    pub fn wobble_state(&self) -> Option<DecoratedPotWobble> {
        self.container.lock().wobble
    }

    /// Queues the positive or negative wobble block event.
    pub fn wobble(&self, style: WobbleStyle) {
        let Some(world) = self.get_level() else {
            return;
        };
        world.block_event(
            self.get_block_pos(),
            self.get_block_state().get_block(),
            EVENT_POT_WOBBLES,
            style.event_data(),
        );
    }

    /// Returns the container capability used for atomic inventory operations.
    pub(crate) fn inventory_ref(&self) -> ContainerRef {
        self.unpack_loot_table();
        self.container_ref.clone()
    }

    /// Ensures any saved loot table is resolved before the container is locked.
    pub(crate) fn prepare_container_access(&self) {
        self.unpack_loot_table();
    }

    fn unpack_loot_table(&self) {
        if self.get_level().is_none() {
            return;
        }
        let Some((loot_table_key, seed)) = ({
            let mut container = self.container.lock();
            container
                .loot_table
                .take()
                .map(|key| (key, container.loot_table_seed))
        }) else {
            return;
        };

        let Some(loot_table) = REGISTRY.loot_tables.by_key(&loot_table_key) else {
            return;
        };

        if seed == 0 {
            let mut random = rand::rng();
            self.fill_loot_table(loot_table, &mut random);
        } else {
            let mut random = LegacyRandom::from_seed(seed as u64);
            self.fill_loot_table(loot_table, &mut random);
        }
        self.set_changed();
    }

    fn fill_loot_table<R: LootRandom>(&self, loot_table: LootTableRef, random: &mut R) {
        let pos = self.get_block_pos();
        let mut context = LootContext::new(random).with_origin(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 0.5,
            f64::from(pos.z()) + 0.5,
        );
        let mut container = self.container.lock();
        loot_table.fill(&mut context, &mut container.items);
    }
}

impl BlockEntity for DecoratedPotBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn trigger_event(&self, param_a: i32, param_b: i32) -> bool {
        if param_a != EVENT_POT_WOBBLES {
            return false;
        }
        let Some(style) = WobbleStyle::from_event_data(param_b) else {
            return false;
        };
        let Some(world) = self.get_level() else {
            return false;
        };
        self.container.lock().wobble = Some(DecoratedPotWobble {
            started_at_tick: world.game_time(),
            style,
        });
        true
    }

    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        self.unpack_loot_table();
        let item = mem::take(&mut self.container.lock().items[0]);
        let Some(world) = self.get_level() else {
            return;
        };
        world.drop_item_stack(pos, item);
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let decorations = nbt
            .get("sherds")
            .and_then(PotDecorations::from_nbt_tag)
            .unwrap_or(PotDecorations::EMPTY);
        let loot_table = nbt
            .string("LootTable")
            .and_then(|value| Identifier::from_str(value.to_str().as_ref()).ok());
        let loot_table_seed = nbt.long("LootTableSeed").unwrap_or(0);
        let item = if loot_table.is_some() {
            ItemStack::empty()
        } else {
            nbt.compound("item")
                .and_then(|compound| ItemStack::from_borrowed_compound(&compound))
                .unwrap_or_else(ItemStack::empty)
        };

        let mut container = self.container.lock();
        container.decorations = decorations;
        container.loot_table = loot_table;
        container.loot_table_seed = loot_table_seed;
        container.items[0] = item;
        container.wobble = None;
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let container = self.container.lock();
        if container.decorations != PotDecorations::EMPTY {
            nbt.insert("sherds", container.decorations.clone().to_nbt_tag());
        }
        if let Some(loot_table) = &container.loot_table {
            nbt.insert("LootTable", loot_table.to_string());
            if container.loot_table_seed != 0 {
                nbt.insert("LootTableSeed", container.loot_table_seed);
            }
        } else if !container.items[0].is_empty() {
            nbt.insert("item", container.items[0].to_nbt_tag_ref());
        }
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        Some(self.save_custom_only())
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        self.unpack_loot_table();
        Some(self.container_ref.clone())
    }
}

impl Container for DecoratedPotContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_item(&self, slot: usize) -> &ItemStack {
        if slot == 0 {
            &self.items[0]
        } else {
            static EMPTY: LazyLock<ItemStack> = LazyLock::new(ItemStack::empty);
            &EMPTY
        }
    }

    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        if slot == 0 {
            self.items[0] = stack;
        }
    }

    fn remove_item(&mut self, slot: usize, count: i32) -> ItemStack {
        if slot != 0 || count <= 0 {
            return ItemStack::empty();
        }
        self.items[0].split(count)
    }

    fn remove_item_no_update(&mut self, slot: usize) -> ItemStack {
        if slot == 0 {
            mem::take(&mut self.items[0])
        } else {
            ItemStack::empty()
        }
    }

    fn clear_content(&mut self) -> i32 {
        let count = self.items[0].count();
        self.items[0] = ItemStack::empty();
        count
    }

    fn set_changed(&mut self) {}
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound as read_borrowed_compound;
    use steel_registry::data_components::components::{
        BlockEntityData, CustomData, ItemContainerContents,
    };
    use steel_registry::{ItemStackTemplate, init_vanilla_registry, vanilla_loot_tables};

    use super::*;
    use crate::test_support::fresh_test_world;

    fn test_pot(level: Weak<World>) -> DecoratedPotBlockEntity {
        DecoratedPotBlockEntity::new(
            level,
            BlockPos::new(4, 65, -9),
            vanilla_blocks::DECORATED_POT
                .default_state()
                .set_value(&BlockStateProperties::HORIZONTAL_FACING, Direction::West),
        )
    }

    fn load_owned(entity: &DecoratedPotBlockEntity, nbt: &NbtCompound) {
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test NBT should reborrow");
        entity.load_additional(&borrowed);
    }

    fn asymmetric_decorations() -> PotDecorations {
        PotDecorations::from_ordered(&[
            &vanilla_items::ANGLER_POTTERY_SHERD,
            &vanilla_items::BRICK,
            &vanilla_items::ARCHER_POTTERY_SHERD,
            &vanilla_items::ARMS_UP_POTTERY_SHERD,
        ])
        .expect("four extracted pot ingredients should form decorations")
    }

    #[test]
    fn placing_components_and_persistent_nbt_preserve_every_ordered_side_and_item() {
        init_vanilla_registry();
        let decorations = asymmetric_decorations();
        let stored = ItemStack::with_count(&vanilla_items::DIAMOND, 23);
        let contents = ItemContainerContents::new(vec![Some(
            ItemStackTemplate::from_stack(&stored)
                .expect("the non-empty stored stack should form a template"),
        )])
        .expect("one container slot should fit");
        let mut placing_stack = ItemStack::new(&vanilla_items::DECORATED_POT);
        placing_stack.set(POT_DECORATIONS, decorations.clone());
        placing_stack.set(CONTAINER, contents);

        let source = test_pot(Weak::new());
        source.apply_components_from_item(&placing_stack);

        assert_eq!(source.direction(), Direction::West);
        assert_eq!(source.decorations(), decorations);
        assert_eq!(
            source.decorations().ordered()[0],
            &*vanilla_items::ANGLER_POTTERY_SHERD
        );
        assert_eq!(source.decorations().ordered()[1], &*vanilla_items::BRICK);
        assert_eq!(
            source.decorations().ordered()[2],
            &*vanilla_items::ARCHER_POTTERY_SHERD
        );
        assert_eq!(
            source.decorations().ordered()[3],
            &*vanilla_items::ARMS_UP_POTTERY_SHERD
        );
        assert!(ItemStack::is_same_item_same_components(
            &source.item(),
            &stored
        ));
        assert_eq!(source.item().count(), 23);

        let mut saved = NbtCompound::new();
        source.save_additional(&mut saved);
        let update_tag = source
            .get_update_tag()
            .expect("decorated pots require initial client synchronization");
        assert_eq!(update_tag, saved);

        let loaded = test_pot(Weak::new());
        load_owned(&loaded, &saved);
        assert_eq!(loaded.decorations(), decorations);
        assert!(ItemStack::is_same_item_same_components(
            &loaded.item(),
            &stored
        ));
        assert_eq!(loaded.item().count(), 23);
        assert_eq!(loaded.get_update_tag(), Some(saved));
    }

    #[test]
    fn single_item_container_rejects_other_slots_and_splits_exactly() {
        init_vanilla_registry();
        let pot = test_pot(Weak::new());
        let mut container = pot.container.lock();
        container.set_item(1, ItemStack::new(&vanilla_items::DIRT));
        assert!(container.get_item(1).is_empty());

        container.set_item(0, ItemStack::with_count(&vanilla_items::STONE, 8));
        let removed = container.remove_item(0, 3);
        assert!(removed.is(&vanilla_items::STONE));
        assert_eq!(removed.count(), 3);
        assert_eq!(container.get_item(0).count(), 5);
        assert!(container.remove_item(1, 5).is_empty());

        let remainder = container.remove_item_no_update(0);
        assert_eq!(remainder.count(), 5);
        assert!(container.is_empty());
    }

    #[test]
    fn implicit_placing_components_override_custom_data_without_clearing_its_loot_table() {
        init_vanilla_registry();
        let decorations = asymmetric_decorations();
        let stored = ItemStack::with_count(&vanilla_items::DIAMOND, 11);
        let contents = ItemContainerContents::new(vec![Some(
            ItemStackTemplate::from_stack(&stored)
                .expect("the non-empty stored stack should form a template"),
        )])
        .expect("one container slot should fit");
        let mut custom_data = NbtCompound::new();
        custom_data.insert(
            "LootTable",
            vanilla_loot_tables::POTS_TRIAL_CHAMBERS_CORRIDOR
                .key
                .to_string(),
        );
        custom_data.insert("LootTableSeed", 0x0765_4321_i64);
        custom_data.insert("sherds", PotDecorations::EMPTY.to_nbt_tag());

        let mut placing_stack = ItemStack::new(&vanilla_items::DECORATED_POT);
        placing_stack.set(
            BLOCK_ENTITY_DATA,
            BlockEntityData::new(
                &vanilla_block_entity_types::DECORATED_POT,
                CustomData::try_from_compound(custom_data)
                    .expect("decorated-pot custom data should be valid"),
            ),
        );
        placing_stack.set(POT_DECORATIONS, decorations.clone());
        placing_stack.set(CONTAINER, contents);

        let pot = test_pot(Weak::new());
        pot.apply_components_from_item(&placing_stack);
        assert_eq!(pot.decorations(), decorations);
        assert!(ItemStack::is_same_item_same_components(
            &pot.item(),
            &stored
        ));
        assert_eq!(pot.item().count(), 11);

        let mut saved = NbtCompound::new();
        pot.save_additional(&mut saved);
        assert_eq!(saved.long("LootTableSeed"), Some(0x0765_4321));
        assert_eq!(
            saved.string("LootTable").map(ToString::to_string),
            Some(
                vanilla_loot_tables::POTS_TRIAL_CHAMBERS_CORRIDOR
                    .key
                    .to_string()
            )
        );
        assert!(!saved.contains("item"));
    }

    #[test]
    fn seeded_saved_loot_table_unpacks_once_and_persists_the_result() {
        init_vanilla_registry();
        let world = fresh_test_world("decorated_pot_seeded_loot");
        let first = test_pot(Arc::downgrade(&world));
        let second = test_pot(Arc::downgrade(&world));
        let mut loot_nbt = NbtCompound::new();
        loot_nbt.insert(
            "LootTable",
            vanilla_loot_tables::POTS_TRIAL_CHAMBERS_CORRIDOR
                .key
                .to_string(),
        );
        loot_nbt.insert("LootTableSeed", 0x1234_5678_i64);
        load_owned(&first, &loot_nbt);
        load_owned(&second, &loot_nbt);

        let first_item = first.item();
        let second_item = second.item();
        assert!(!first_item.is_empty());
        assert!(ItemStack::is_same_item_same_components(
            &first_item,
            &second_item
        ));
        assert_eq!(first_item.count(), second_item.count());

        let mut saved = NbtCompound::new();
        first.save_additional(&mut saved);
        assert!(!saved.contains("LootTable"));
        assert!(!saved.contains("LootTableSeed"));
        assert!(saved.contains("item"));
        assert!(ItemStack::is_same_item_same_components(
            &first.item(),
            &first_item
        ));
        assert_eq!(first.item().count(), first_item.count());
    }
}
