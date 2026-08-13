//! Projectile-weapon item capabilities used by living-entity ammo selection.

use steel_macros::item_behavior;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{REGISTRY, TaggedRegistryExt, vanilla_items};

use crate::behavior::{ItemBehavior, ProjectileWeaponItem};

fn is_arrow(stack: &ItemStack) -> bool {
    REGISTRY.items.is_in_tag(stack.item(), &ItemTag::ARROWS)
}

/// Vanilla bow projectile-selection behavior.
#[item_behavior]
pub struct BowItem;

impl ItemBehavior for BowItem {
    fn as_projectile_weapon(&self) -> Option<&dyn ProjectileWeaponItem> {
        Some(self)
    }
}

impl ProjectileWeaponItem for BowItem {
    fn supports_held_projectile(&self, projectile: &ItemStack) -> bool {
        is_arrow(projectile)
    }
}

/// Vanilla crossbow projectile-selection behavior.
#[item_behavior]
pub struct CrossbowItem;

impl ItemBehavior for CrossbowItem {
    fn as_projectile_weapon(&self) -> Option<&dyn ProjectileWeaponItem> {
        Some(self)
    }
}

impl ProjectileWeaponItem for CrossbowItem {
    fn supports_held_projectile(&self, projectile: &ItemStack) -> bool {
        is_arrow(projectile) || projectile.is(&vanilla_items::FIREWORK_ROCKET)
    }
}
