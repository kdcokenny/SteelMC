//! Typed descriptors for the environment attributes Steel evaluates at runtime.

use crate::moon_phase::MoonPhase;

/// A typed Vanilla environment attribute with its fallback value.
#[derive(Clone, Copy, Debug)]
pub struct EnvironmentAttribute<Value> {
    /// The namespaced key used by generated timeline tracks.
    pub key: &'static str,
    /// The value used when no active environment layer overrides the attribute.
    pub default_value: Value,
}

/// Vanilla's non-interpolated moon-phase environment attribute.
pub const MOON_PHASE: EnvironmentAttribute<MoonPhase> = EnvironmentAttribute {
    key: "minecraft:visual/moon_phase",
    default_value: MoonPhase::FullMoon,
};
