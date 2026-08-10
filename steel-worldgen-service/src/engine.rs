//! Headless Steel generation engine.

use std::{
    result::Result as StdResult,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use rayon::ThreadPoolBuilder;
use steel_core::{
    chunk::{chunk_request::ChunkTicketKind, status::ChunkStatus},
    level_data::WorldGenerationSettings,
    world::{World, WorldConfig, WorldStorageConfig},
    worldgen::WorldGeneratorRegistry,
};
use steel_utils::{
    ChunkPos,
    locks::{AsyncMutex, SyncRwLock},
    types::{Difficulty, GameType},
};
#[cfg(test)]
use tokio::sync::MutexGuard;
use tokio::{runtime::Runtime, task::spawn_blocking};
use tokio_util::sync::CancellationToken;
use toml::map::Map;

use crate::{
    artifact::{ArtifactContext, EncodedArtifact, encode_chunk},
    config::Config,
    fingerprint::{ProfileFingerprints, profile_fingerprints},
};

/// Timed result of one headless generation request.
pub struct GenerationResult {
    /// Encoded detached artifact.
    pub artifact: EncodedArtifact,
    /// Time spent waiting to enter the single headless scheduling epoch.
    pub queue: Duration,
    /// Time spent waiting for Steel to reach NOISE.
    pub generation: Duration,
    /// Time spent canonicalizing and encoding the artifact.
    pub encode: Duration,
}

/// A cooperative cancellation or a fatal failure of the shared headless world.
#[derive(Debug)]
pub enum GenerationError {
    /// No synchronous stage failed; publication was cancelled at a checkpoint.
    Cancelled(&'static str),
    /// The world may be partially mutated and must never be reused.
    Failed(anyhow::Error),
}

/// One fixed-seed, fixed-generator headless Steel world.
pub struct Engine {
    world: Arc<World>,
    // The headless scheduler exposes one global epoch publication boundary per world.
    generation_lock: AsyncMutex<()>,
    poisoned: AtomicBool,
    publication_gate: SyncRwLock<()>,
    /// Minimum block Y accepted by the profile.
    pub min_y: i32,
    /// Total profile world height.
    pub height: i32,
    /// Digests clients must match exactly.
    pub fingerprints: ProfileFingerprints,
}

impl Engine {
    /// Creates the in-memory worker world and its dedicated generation pool.
    pub async fn new(config: &Config, chunk_runtime: Arc<Runtime>) -> Result<Self> {
        let generation_pool = Arc::new(
            ThreadPoolBuilder::new()
                .num_threads(config.generation_threads)
                .stack_size(8 * 1024 * 1024)
                .thread_name(|index| format!("steel-worldgen-{index}"))
                .build()
                .context("failed to create generation thread pool")?,
        );
        let generator_registry =
            WorldGeneratorRegistry::new_with_builtins().map_err(anyhow::Error::msg)?;
        let generator_config = toml::Value::Table(Map::new());
        let validated = generator_registry
            .validate_config(&config.generator_id, &generator_config)
            .map_err(anyhow::Error::msg)?;
        let output = generator_registry
            .create(None, &validated, config.seed, Arc::clone(&generation_pool))
            .map_err(anyhow::Error::msg)?;
        let min_y = output.dimension_type.min_y;
        let height = output.dimension_type.height;
        ensure!(
            height > 0 && height <= 4096 && height % 16 == 0 && min_y % 16 == 0,
            "generator returned an invalid world range"
        );
        let generation_settings = WorldGenerationSettings::from_generator_config(
            config.generator_id.clone(),
            &output.config,
            output.dimension_type.key.clone(),
            min_y,
            height,
        );
        let world = World::new_with_config(
            chunk_runtime,
            config.dimension_key.clone(),
            output.dimension_type,
            config.seed,
            WorldConfig {
                storage: WorldStorageConfig::RamOnly,
                level_data_path: None,
                generator: Arc::new(output.generator),
                generation_settings,
                view_distance: 2,
                simulation_distance: 2,
                max_chained_neighbor_updates: 1_000_000,
                compression: None,
                is_flat: output.is_flat,
                sea_level: output.sea_level,
                default_gamemode: GameType::Survival,
                difficulty: Difficulty::Normal,
                generation_status_ceiling: Some(ChunkStatus::Noise),
            },
            generation_pool,
        )
        .await
        .context("failed to create headless world")?;
        let fingerprints = profile_fingerprints(config, min_y, height)?;
        Ok(Self {
            world,
            generation_lock: AsyncMutex::new(()),
            poisoned: AtomicBool::new(false),
            publication_gate: SyncRwLock::new(()),
            min_y,
            height,
            fingerprints,
        })
    }

    /// Generates a fresh center chunk through NOISE and snapshots it.
    pub async fn generate_noise(
        &self,
        pos: ChunkPos,
        artifact_context: ArtifactContext,
        cancellation: CancellationToken,
    ) -> StdResult<GenerationResult, GenerationError> {
        if pos.0.x == i32::MIN || pos.0.y == i32::MIN || !ChunkPos::is_valid(pos.0.x, pos.0.y) {
            return Err(GenerationError::Failed(anyhow::anyhow!(
                "chunk position is outside Minecraft's valid range"
            )));
        }
        if self.is_poisoned() {
            return Err(GenerationError::Failed(anyhow::anyhow!(
                "generation engine is quarantined after an earlier failure"
            )));
        }

        // request_chunk and advance_scheduling form one headless scheduling epoch.
        // Interleaving epochs can publish a generation task before its holder exists.
        let queue_started = Instant::now();
        let generation_guard = tokio::select! {
            guard = self.generation_lock.lock() => guard,
            () = cancellation.cancelled() => {
                return Err(GenerationError::Cancelled(
                    "generation was cancelled before it entered the scheduling epoch",
                ));
            }
        };
        if self.is_poisoned() {
            return Err(GenerationError::Failed(anyhow::anyhow!(
                "generation engine is quarantined after an earlier failure"
            )));
        }
        if cancellation.is_cancelled() {
            return Err(GenerationError::Cancelled(
                "generation was cancelled before it entered the scheduling epoch",
            ));
        }

        let queue = queue_started.elapsed();
        let generation_started = Instant::now();
        let request =
            self.world
                .chunk_map
                .request_chunk(pos, ChunkStatus::Noise, ChunkTicketKind::Pregen);
        let Some(ready) = request.wait_ready_offline().await else {
            self.quarantine();
            return Err(GenerationError::Failed(anyhow::anyhow!(
                "Steel chunk generation failed"
            )));
        };
        let generation = generation_started.elapsed();
        if cancellation.is_cancelled() {
            return Err(GenerationError::Cancelled(
                "generation was cancelled while Steel completed its synchronous stage",
            ));
        }

        let result = async {
            if ready.holders.len() != 1 {
                return Err(anyhow::anyhow!(
                    "single chunk request returned the wrong holder count"
                ));
            }
            let holder = Arc::clone(&ready.holders[0]);
            if holder.published_status() != Some(ChunkStatus::Noise) {
                return Err(anyhow::anyhow!(
                    "headless generation exceeded the NOISE status ceiling: {:?}",
                    holder.published_status()
                ));
            }
            let encode_started = Instant::now();
            let artifact = spawn_blocking(move || {
                let chunk = holder
                    .try_chunk(ChunkStatus::Noise)
                    .context("ready holder lost its NOISE chunk")?;
                encode_chunk(chunk, &artifact_context)
            })
            .await
            .context("artifact encoder task failed")??;
            let encode = encode_started.elapsed();
            drop(request);
            Ok(GenerationResult {
                artifact,
                queue,
                generation,
                encode,
            })
        }
        .await;
        if result.is_err() {
            // This store occurs while the global generation guard is still held. Any queued
            // request re-checks it after acquiring the guard and cannot observe this world.
            self.quarantine();
        }
        drop(generation_guard);
        result.map_err(GenerationError::Failed)
    }

    /// Whether a fatal generation/encoding error quarantined this engine.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// Runs a response-publication closure atomically before any fatal quarantine.
    pub fn publish_if_healthy<T>(&self, publish: impl FnOnce() -> T) -> Option<T> {
        let _publication = self.publication_gate.read();
        (!self.is_poisoned()).then(publish)
    }

    pub(crate) fn quarantine_after_failure(&self) {
        self.quarantine();
    }

    fn quarantine(&self) {
        let _publication = self.publication_gate.write();
        self.poisoned.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) async fn hold_generation_lock_for_test(&self) -> MutexGuard<'_, ()> {
        self.generation_lock.lock().await
    }

    /// Stops the world generation-refill task before runtime shutdown.
    pub fn stop(&self) {
        self.world.chunk_map.stop_generation_refill_loop();
    }
}
