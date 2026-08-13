use steel_registry::entity_type::EntityTypeRef;
use steel_utils::types::Difficulty;

/// Returns the difficulty gate at the start of vanilla
/// `SpawnPlacements.checkSpawnRules`.
#[must_use]
pub fn entity_type_allowed_in_difficulty(
    entity_type: EntityTypeRef,
    difficulty: Difficulty,
) -> bool {
    entity_type.allowed_in_peaceful || difficulty != Difficulty::Peaceful
}

/// Vanilla `EntitySpawnReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitySpawnReason {
    /// Natural world spawning.
    Natural,
    /// Mob spawning during chunk generation.
    ChunkGeneration,
    /// A standard mob spawner.
    Spawner,
    /// Structure-owned spawning.
    Structure,
    /// Offspring created by breeding.
    Breeding,
    /// A mob summoned by another mob.
    MobSummoned,
    /// A jockey or other mounted spawn.
    Jockey,
    /// Event-owned spawning.
    Event,
    /// Conversion from another entity type.
    Conversion,
    /// Reinforcement spawning.
    Reinforcement,
    /// Triggered spawning.
    Triggered,
    /// An entity released from a bucket.
    Bucket,
    /// Spawning caused by item use.
    SpawnItemUse,
    /// Spawning caused by a command.
    Command,
    /// Spawning caused by a dispenser.
    Dispenser,
    /// Patrol spawning.
    Patrol,
    /// A trial spawner.
    TrialSpawner,
    /// Loading a persisted entity.
    Load,
    /// Arrival after dimension travel.
    DimensionTravel,
}

impl EntitySpawnReason {
    #[must_use]
    /// Returns vanilla `EntitySpawnReason.isSpawner`.
    pub const fn is_spawner(self) -> bool {
        matches!(self, Self::Spawner | Self::TrialSpawner)
    }

    #[must_use]
    /// Returns vanilla `EntitySpawnReason.ignoresLightRequirements`.
    pub const fn ignores_light_requirements(self) -> bool {
        matches!(self, Self::TrialSpawner)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpawnGroupData {
    AgeableMob(AgeableMobGroupData),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgeableMobGroupData {
    group_size: i32,
    should_spawn_baby: bool,
    baby_spawn_chance: f32,
}

impl AgeableMobGroupData {
    pub const DEFAULT_BABY_SPAWN_CHANCE: f32 = 0.05;

    #[must_use]
    pub const fn new(should_spawn_baby: bool, baby_spawn_chance: f32) -> Self {
        Self {
            group_size: 0,
            should_spawn_baby,
            baby_spawn_chance,
        }
    }

    #[must_use]
    pub const fn with_should_spawn_baby(should_spawn_baby: bool) -> Self {
        Self::new(should_spawn_baby, Self::DEFAULT_BABY_SPAWN_CHANCE)
    }

    #[must_use]
    pub const fn with_baby_spawn_chance(baby_spawn_chance: f32) -> Self {
        Self::new(true, baby_spawn_chance)
    }

    #[must_use]
    pub const fn group_size(self) -> i32 {
        self.group_size
    }

    #[must_use]
    pub const fn should_spawn_baby(self) -> bool {
        self.should_spawn_baby
    }

    #[must_use]
    pub const fn baby_spawn_chance(self) -> f32 {
        self.baby_spawn_chance
    }

    pub const fn increase_group_size_by_one(&mut self) {
        self.group_size += 1;
    }

    #[must_use]
    pub const fn needs_baby_spawn_roll(self) -> bool {
        self.should_spawn_baby && self.group_size > 0
    }

    pub fn finalize_ageable_spawn(&mut self, baby_roll: impl FnOnce() -> f32) -> bool {
        let spawn_baby = self.needs_baby_spawn_roll() && baby_roll() <= self.baby_spawn_chance;
        self.increase_group_size_by_one();
        spawn_baby
    }
}

#[cfg(test)]
mod tests {
    use super::AgeableMobGroupData;

    #[test]
    fn ageable_group_data_increments_before_later_baby_rolls_can_apply() {
        let mut group_data = AgeableMobGroupData::with_should_spawn_baby(true);

        assert!(!group_data.finalize_ageable_spawn(|| {
            panic!("first group member should not roll for baby spawn")
        }));
        assert_eq!(group_data.group_size(), 1);

        assert!(group_data.finalize_ageable_spawn(|| 0.05));
        assert_eq!(group_data.group_size(), 2);
    }

    #[test]
    fn ageable_group_data_can_disable_baby_spawns() {
        let mut group_data = AgeableMobGroupData::with_should_spawn_baby(false);

        assert!(
            !group_data
                .finalize_ageable_spawn(|| { panic!("disabled baby spawning should not roll") })
        );
        assert!(
            !group_data
                .finalize_ageable_spawn(|| { panic!("disabled baby spawning should not roll") })
        );
        assert_eq!(group_data.group_size(), 2);
    }
}
