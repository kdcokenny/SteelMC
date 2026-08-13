//! Persistent named random sequences shared by the worlds in a Steel domain.

use std::{io, path::Path};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use steel_utils::{
    Identifier,
    locks::SyncMutex,
    random::xoroshiro::Xoroshiro,
    saved_data::{SavedDataManager, names as saved_data_names},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RandomSequenceData {
    id: Identifier,
    source: [i64; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RandomSequencesData {
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: Vec<RandomSequenceData>,
}

impl Default for RandomSequencesData {
    fn default() -> Self {
        Self {
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            sequences: Vec::new(),
        }
    }
}

struct RandomSequencesState {
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: FxHashMap<Identifier, Xoroshiro>,
    generation: u64,
    saved_generation: u64,
}

impl Default for RandomSequencesState {
    fn default() -> Self {
        Self {
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            sequences: FxHashMap::default(),
            generation: 0,
            saved_generation: 0,
        }
    }
}

impl RandomSequencesState {
    fn from_data(data: RandomSequencesData) -> io::Result<Self> {
        let mut sequences = FxHashMap::default();
        for sequence in data.sequences {
            if sequences.contains_key(&sequence.id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("duplicate random sequence {}", sequence.id),
                ));
            }
            sequences.insert(
                sequence.id,
                Xoroshiro::from_state(sequence.source[0] as u64, sequence.source[1] as u64),
            );
        }
        Ok(Self {
            salt: data.salt,
            include_world_seed: data.include_world_seed,
            include_sequence_id: data.include_sequence_id,
            sequences,
            generation: 0,
            saved_generation: 0,
        })
    }

    fn snapshot(&self) -> RandomSequencesData {
        let mut sequences: Vec<_> = self
            .sequences
            .iter()
            .map(|(id, random)| {
                let (seed_lo, seed_hi) = random.state();
                RandomSequenceData {
                    id: id.clone(),
                    source: [seed_lo as i64, seed_hi as i64],
                }
            })
            .collect();
        sequences.sort_by(|left, right| {
            left.id
                .namespace
                .cmp(&right.id.namespace)
                .then_with(|| left.id.path.cmp(&right.id.path))
        });
        RandomSequencesData {
            salt: self.salt,
            include_world_seed: self.include_world_seed,
            include_sequence_id: self.include_sequence_id,
            sequences,
        }
    }
}

/// Vanilla `RandomSequences`, scoped to one Steel domain's world set.
pub(crate) struct RandomSequences {
    world_seed: i64,
    storage: SavedDataManager,
    state: SyncMutex<RandomSequencesState>,
}

impl RandomSequences {
    pub(crate) async fn load(world_dir: Option<&Path>, world_seed: i64) -> io::Result<Self> {
        let storage = SavedDataManager::new(world_dir);
        let data = storage
            .load_or_default(saved_data_names::RANDOM_SEQUENCES)
            .await?;
        Ok(Self {
            world_seed,
            storage,
            state: SyncMutex::new(RandomSequencesState::from_data(data)?),
        })
    }

    #[cfg(test)]
    fn ephemeral(world_seed: i64) -> Self {
        Self {
            world_seed,
            storage: SavedDataManager::new(None),
            state: SyncMutex::new(RandomSequencesState::default()),
        }
    }

    pub(crate) fn with_sequence<T>(
        &self,
        key: &Identifier,
        action: impl FnOnce(&mut Xoroshiro) -> T,
    ) -> T {
        let mut state = self.state.lock();
        let salt = state.salt;
        let include_world_seed = state.include_world_seed;
        let include_sequence_id = state.include_sequence_id;
        let (result, changed) = {
            let sequence = state.sequences.entry(key.clone()).or_insert_with(|| {
                self.create_sequence(key, salt, include_world_seed, include_sequence_id)
            });
            let initial_state = sequence.state();
            let result = action(sequence);
            (result, sequence.state() != initial_state)
        };
        if changed {
            state.generation = state.generation.wrapping_add(1);
        }
        result
    }

    fn create_sequence(
        &self,
        key: &Identifier,
        salt: i32,
        include_world_seed: bool,
        include_sequence_id: bool,
    ) -> Xoroshiro {
        let seed = (if include_world_seed {
            self.world_seed
        } else {
            0
        }) ^ i64::from(salt);
        if !include_sequence_id {
            return Xoroshiro::from_seed(seed as u64);
        }

        let digest = md5::compute(key.to_string().as_bytes());
        let key_hash = [
            u64::from_be_bytes([
                digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6],
                digest[7],
            ]),
            u64::from_be_bytes([
                digest[8], digest[9], digest[10], digest[11], digest[12], digest[13], digest[14],
                digest[15],
            ]),
        ];
        Xoroshiro::from_seed_and_key_hash(seed as u64, key_hash)
    }

    pub(crate) async fn save(&self) -> io::Result<bool> {
        let (generation, snapshot) = {
            let state = self.state.lock();
            if state.generation == state.saved_generation {
                return Ok(false);
            }
            (state.generation, state.snapshot())
        };

        self.storage
            .save(saved_data_names::RANDOM_SEQUENCES, &snapshot)
            .await?;
        let mut state = self.state.lock();
        if state.saved_generation < generation {
            state.saved_generation = generation;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use steel_utils::random::Random;
    use tokio::fs::remove_dir_all;

    use super::*;

    const TEST_WORLD_SEED: i64 = 42;

    fn temp_world_dir(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        temp_dir().join(format!("steel-random-sequences-{test_name}-{unique}"))
    }

    #[tokio::test]
    async fn saved_sequence_resumes_from_the_exact_next_value() {
        const WORLD_SEED: i64 = 0x1234_5678;

        let path = temp_world_dir("resume");
        let key = Identifier::vanilla_static("pots/trial_chambers/corridor");
        let Ok(sequences) = RandomSequences::load(Some(&path), WORLD_SEED).await else {
            panic!("random sequences should load");
        };

        sequences.with_sequence(&key, Random::next_i64);
        assert!(matches!(sequences.save().await, Ok(true)));
        let expected_next = sequences.with_sequence(&key, Random::next_i64);

        let Ok(loaded) = RandomSequences::load(Some(&path), WORLD_SEED).await else {
            panic!("saved random sequences should reload");
        };
        assert_eq!(loaded.with_sequence(&key, Random::next_i64), expected_next);

        if let Err(error) = remove_dir_all(&path).await {
            panic!("random sequence test data should be removed: {error}");
        }
    }

    #[tokio::test]
    async fn obtaining_a_named_sequence_without_using_randomness_does_not_dirty_it() {
        let sequences = RandomSequences::ephemeral(TEST_WORLD_SEED);
        let key = Identifier::vanilla_static("unused");

        sequences.with_sequence(&key, |_| {});

        assert!(matches!(sequences.save().await, Ok(false)));
    }

    #[test]
    fn sequence_id_changes_the_vanilla_xoroshiro_stream() {
        let sequences = RandomSequences::ephemeral(TEST_WORLD_SEED);
        let first = sequences.with_sequence(&Identifier::vanilla_static("first"), Random::next_i64);
        let second =
            sequences.with_sequence(&Identifier::vanilla_static("second"), Random::next_i64);
        assert_ne!(first, second);
    }

    #[test]
    fn named_sequence_matches_vanilla_java_vector() {
        const EXPECTED: [i64; 5] = [
            4_298_899_086_323_700_842,
            -4_353_401_517_908_995_432,
            -742_493_435_360_048_728,
            8_066_653_164_040_059_132,
            4_985_134_824_357_493_253,
        ];

        let sequences = RandomSequences::ephemeral(TEST_WORLD_SEED);
        let key = Identifier::vanilla_static("pots/trial_chambers/corridor");
        for expected in EXPECTED {
            assert_eq!(sequences.with_sequence(&key, Random::next_i64), expected);
        }
    }
}
