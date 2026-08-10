//! gRPC service implementation.

use std::{
    collections::VecDeque,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rustc_hash::FxHashMap;
use steel_utils::{ChunkPos, locks::SyncMutex};
use tokio::{sync::Semaphore, task::spawn, time::timeout};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tonic_health::server::HealthReporter;

use crate::{
    artifact::{ARTIFACT_VERSION, ArtifactContext, EncodedArtifact, MAX_ARTIFACT_BYTES},
    config::{Config, MINECRAFT_VERSION},
    engine::{Engine, GenerationError},
    fingerprint::{
        BUILD_CONFIGURATION, BUILD_TARGET, CARGO_ID, EXTERNAL_BUILD_ID, RUSTC_ID, SOURCE_SHA256,
        canonical_request_sha256,
    },
    proto::v1::{
        CancelRequest, CancelResponse, Capabilities, Compression, GenerateRequest,
        GenerateResponse, GenerationContext, GetCapabilitiesRequest, GetMetricsRequest, Metrics,
        Stage, StageTimings,
        world_gen_service_server::{WorldGenService, WorldGenServiceServer},
    },
};

const MAX_REQUEST_BYTES: u32 = 64 * 1024;

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    cache_hits: AtomicU64,
}

struct ActiveRequest {
    canonical_sha256: [u8; 32],
    cancellation: CancellationToken,
    cancellation_recorded: bool,
}

struct ArtifactCache {
    values: FxHashMap<[u8; 32], Arc<EncodedArtifact>>,
    insertion_order: VecDeque<[u8; 32]>,
    total_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl ArtifactCache {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            values: FxHashMap::default(),
            insertion_order: VecDeque::new(),
            total_bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn get(&self, key: &[u8; 32]) -> Option<Arc<EncodedArtifact>> {
        self.values.get(key).cloned()
    }

    fn insert(&mut self, key: [u8; 32], artifact: Arc<EncodedArtifact>) {
        let artifact_bytes = artifact.bytes.len();
        if self.max_entries == 0
            || self.max_bytes == 0
            || artifact_bytes > self.max_bytes
            || self.values.contains_key(&key)
        {
            return;
        }
        while self.values.len() >= self.max_entries
            || self.total_bytes.saturating_add(artifact_bytes) > self.max_bytes
        {
            let Some(oldest) = self.insertion_order.pop_front() else {
                self.values.clear();
                self.total_bytes = 0;
                break;
            };
            if let Some(removed) = self.values.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes.len());
            }
        }
        self.insertion_order.push_back(key);
        self.total_bytes += artifact_bytes;
        self.values.insert(key, artifact);
    }
}

struct PeerAdmission {
    max_in_flight: usize,
    in_flight: FxHashMap<Option<IpAddr>, usize>,
}

struct PeerPermit {
    admission: Arc<SyncMutex<PeerAdmission>>,
    peer: Option<IpAddr>,
}

impl PeerPermit {
    fn try_acquire(admission: Arc<SyncMutex<PeerAdmission>>, peer: Option<IpAddr>) -> Option<Self> {
        let mut state = admission.lock();
        let active = state.in_flight.get(&peer).copied().unwrap_or(0);
        if active >= state.max_in_flight {
            return None;
        }
        state.in_flight.insert(peer, active + 1);
        drop(state);
        Some(Self { admission, peer })
    }
}

impl Drop for PeerPermit {
    fn drop(&mut self) {
        let mut state = self.admission.lock();
        let Some(active) = state.in_flight.get_mut(&self.peer) else {
            return;
        };
        *active -= 1;
        if *active == 0 {
            state.in_flight.remove(&self.peer);
        }
    }
}

struct State {
    config: Config,
    engine: Arc<Engine>,
    capabilities: Capabilities,
    semaphore: Arc<Semaphore>,
    peer_admission: Arc<SyncMutex<PeerAdmission>>,
    active: Arc<SyncMutex<FxHashMap<Vec<u8>, ActiveRequest>>>,
    cache: SyncMutex<ArtifactCache>,
    counters: Arc<Counters>,
    health_reporter: Option<HealthReporter>,
    fatal_shutdown: CancellationToken,
}

impl State {
    async fn fail_stop_engine(&self) {
        self.engine.quarantine_after_failure();
        if let Some(reporter) = &self.health_reporter {
            reporter
                .set_not_serving::<WorldGenServiceServer<Service>>()
                .await;
        }
        self.fatal_shutdown.cancel();
    }
}

/// Cloneable tonic service backed by one fixed [`Engine`].
#[derive(Clone)]
pub struct Service {
    state: Arc<State>,
}

impl Service {
    /// Constructs the bounded service, cache, and capability descriptor.
    pub fn new(config: Config, engine: Arc<Engine>) -> Self {
        Self::new_with_health(config, engine, None, CancellationToken::new())
    }

    /// Constructs the service with a reporter that is marked not-serving after a fatal generation failure.
    pub fn new_with_health(
        config: Config,
        engine: Arc<Engine>,
        health_reporter: Option<HealthReporter>,
        fatal_shutdown: CancellationToken,
    ) -> Self {
        let fingerprints = engine.fingerprints;
        let capabilities = Capabilities {
            protocol_major: 1,
            protocol_minor: 1,
            artifact_versions: vec![ARTIFACT_VERSION],
            minecraft_version: MINECRAFT_VERSION.to_owned(),
            steel_version: env!("CARGO_PKG_VERSION").to_owned(),
            profile_id: config.profile_id.clone(),
            dimension_key: config.dimension_key.to_string(),
            seed: config.seed,
            min_y: engine.min_y,
            height: engine.height as u32,
            generator_sha256: fingerprints.generator.to_vec(),
            registry_sha256: fingerprints.registry.to_vec(),
            profile_sha256: fingerprints.profile.to_vec(),
            completed_stages: vec![Stage::Biomes as i32, Stage::Noise as i32],
            compression: vec![Compression::None as i32],
            max_request_bytes: MAX_REQUEST_BYTES,
            max_artifact_bytes: MAX_ARTIFACT_BYTES as u32,
            max_in_flight: config.max_in_flight as u32,
            supports_blending: false,
            supports_retrogen: false,
            steel_resumable: false,
            corresponding_source_url: config.corresponding_source_url.clone(),
            source_sha256: SOURCE_SHA256.to_owned(),
            license_expression: "AGPL-3.0-or-later".to_owned(),
            external_build_id: EXTERNAL_BUILD_ID.to_owned(),
            rustc_id: RUSTC_ID.to_owned(),
            cargo_id: CARGO_ID.to_owned(),
            build_target: BUILD_TARGET.to_owned(),
            build_configuration: BUILD_CONFIGURATION.to_owned(),
            max_in_flight_per_peer: config.max_in_flight_per_peer as u32,
        };
        Self {
            state: Arc::new(State {
                semaphore: Arc::new(Semaphore::new(config.max_in_flight)),
                peer_admission: Arc::new(SyncMutex::new(PeerAdmission {
                    max_in_flight: config.max_in_flight_per_peer,
                    in_flight: FxHashMap::default(),
                })),
                active: Arc::new(SyncMutex::new(FxHashMap::default())),
                cache: SyncMutex::new(ArtifactCache::new(
                    config.max_cache_entries,
                    config.max_cache_bytes,
                )),
                counters: Arc::new(Counters::default()),
                health_reporter,
                fatal_shutdown,
                config,
                engine,
                capabilities,
            }),
        }
    }
}

struct ActiveGuard {
    request_id: Vec<u8>,
    active: Arc<SyncMutex<FxHashMap<Vec<u8>, ActiveRequest>>>,
    counters: Arc<Counters>,
    completed: bool,
}

impl ActiveGuard {
    fn register(
        request_id: Vec<u8>,
        canonical_sha256: [u8; 32],
        active: Arc<SyncMutex<FxHashMap<Vec<u8>, ActiveRequest>>>,
        counters: Arc<Counters>,
    ) -> Result<(Self, CancellationToken), Status> {
        let cancellation = CancellationToken::new();
        let mut active_requests = active.lock();
        if active_requests.contains_key(&request_id) {
            return Err(Status::already_exists("request_id is already active"));
        }
        active_requests.insert(
            request_id.clone(),
            ActiveRequest {
                canonical_sha256,
                cancellation: cancellation.clone(),
                cancellation_recorded: false,
            },
        );
        drop(active_requests);
        Ok((
            Self {
                request_id,
                active,
                counters,
                completed: false,
            },
            cancellation,
        ))
    }

    fn complete(&mut self) -> bool {
        if self.completed {
            return false;
        }
        let explicitly_cancelled = self
            .active
            .lock()
            .remove(&self.request_id)
            .is_some_and(|request| request.cancellation_recorded);
        self.completed = true;
        explicitly_cancelled
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let cancellation_recorded =
            if let Some(active) = self.active.lock().remove(&self.request_id) {
                active.cancellation.cancel();
                active.cancellation_recorded
            } else {
                false
            };
        if !cancellation_recorded {
            self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[tonic::async_trait]
impl WorldGenService for Service {
    async fn get_capabilities(
        &self,
        _request: Request<GetCapabilitiesRequest>,
    ) -> Result<Response<Capabilities>, Status> {
        Ok(Response::new(self.state.capabilities.clone()))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the RPC keeps validation, bounded execution, metrics, and response publication visible in order"
    )]
    async fn generate(
        &self,
        request: Request<GenerateRequest>,
    ) -> Result<Response<GenerateResponse>, Status> {
        self.state.counters.requests.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let peer = request.remote_addr().map(|address| address.ip());
        let request = request.into_inner();
        let canonical_sha256 = match self.validate_request(&request) {
            Ok(hash) => hash,
            Err(status) => {
                self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
                return Err(status);
            }
        };

        if self.state.engine.is_poisoned() {
            self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
            return Err(Status::unavailable(
                "worker is quarantined after a generation failure and must be restarted",
            ));
        }

        let (mut active_guard, cancellation) = match ActiveGuard::register(
            request.request_id.clone(),
            canonical_sha256,
            Arc::clone(&self.state.active),
            Arc::clone(&self.state.counters),
        ) {
            Ok(active) => active,
            Err(status) => {
                self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
                return Err(status);
            }
        };

        let Ok(request_permit) = Arc::clone(&self.state.semaphore).try_acquire_owned() else {
            if active_guard.complete() {
                return Err(Status::cancelled("request was cancelled"));
            }
            self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
            return Err(Status::resource_exhausted(
                "worker has reached its configured in-flight request limit",
            ));
        };
        let Some(peer_permit) =
            PeerPermit::try_acquire(Arc::clone(&self.state.peer_admission), peer)
        else {
            if active_guard.complete() {
                return Err(Status::cancelled("request was cancelled"));
            }
            self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
            return Err(Status::resource_exhausted(
                "network peer has reached its configured in-flight request limit",
            ));
        };

        if let Some(artifact) = self.state.cache.lock().get(&canonical_sha256) {
            let publication = self.state.engine.publish_if_healthy(|| {
                if active_guard.complete() {
                    return Err(Status::cancelled("request was cancelled"));
                }
                self.state
                    .counters
                    .cache_hits
                    .fetch_add(1, Ordering::Relaxed);
                self.state
                    .counters
                    .succeeded
                    .fetch_add(1, Ordering::Relaxed);
                Ok(Response::new(response_from_artifact(
                    &request,
                    canonical_sha256,
                    &self.state.engine,
                    artifact.as_ref(),
                    true,
                    StageTimings {
                        total_micros: elapsed_micros(started),
                        ..StageTimings::default()
                    },
                )))
            });
            return publication.unwrap_or_else(|| {
                if active_guard.complete() {
                    return Err(Status::cancelled("request was cancelled"));
                }
                self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
                Err(Status::unavailable(
                    "worker is quarantined after a generation failure and must be restarted",
                ))
            });
        }
        let request_timeout = Duration::from_millis(self.state.config.request_timeout_ms);
        let state = Arc::clone(&self.state);
        let chunk_pos = ChunkPos::new(request.chunk_x, request.chunk_z);
        let work_cancellation = cancellation.clone();
        let mut work = spawn(async move {
            // A cancelled or timed-out RPC detaches this task. Keeping the permit here
            // bounds physical work until synchronous Steel generation really stops.
            let _request_permit = request_permit;
            let _peer_permit = peer_permit;
            let context = ArtifactContext {
                minecraft_version: MINECRAFT_VERSION.to_owned(),
                canonical_request_sha256: canonical_sha256,
                generator_sha256: state.engine.fingerprints.generator,
                registry_sha256: state.engine.fingerprints.registry,
                dimension_key: state.config.dimension_key.to_string(),
                seed: state.config.seed,
            };
            match state
                .engine
                .generate_noise(chunk_pos, context, work_cancellation)
                .await
            {
                Ok(result) => Ok(result),
                Err(GenerationError::Cancelled(reason)) => {
                    tracing::debug!(reason, "cancelled Steel generation drained");
                    Err(Status::cancelled(reason))
                }
                Err(GenerationError::Failed(error)) => {
                    state.fail_stop_engine().await;
                    tracing::error!(?error, "Steel generation failed; worker quarantined");
                    Err(Status::internal("Steel chunk generation failed"))
                }
            }
        });

        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(Status::cancelled("request was cancelled")),
            result = timeout(request_timeout, &mut work) => match result {
                Err(_) => {
                    cancellation.cancel();
                    Err(Status::deadline_exceeded("worker request timeout"))
                },
                Ok(Err(error)) => {
                    self.state.fail_stop_engine().await;
                    tracing::error!(?error, "worker generation task panicked; worker quarantined");
                    Err(Status::internal("worker generation task failed"))
                }
                Ok(Ok(result)) => result,
            }
        };
        let result = match outcome {
            Ok(value) => value,
            Err(status) => {
                if active_guard.complete() {
                    return Err(Status::cancelled("request was cancelled"));
                }
                if status.code() != tonic::Code::Cancelled {
                    self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
                }
                return Err(status);
            }
        };
        let queue_micros = duration_micros(result.queue);
        let generation_micros = duration_micros(result.generation);
        let encode_micros = duration_micros(result.encode);
        let artifact = Arc::new(result.artifact);
        let timings = StageTimings {
            queue_micros,
            generation_micros,
            encode_micros,
            total_micros: elapsed_micros(started),
        };
        self.state
            .engine
            .publish_if_healthy(|| {
                if active_guard.complete() {
                    return Err(Status::cancelled("request was cancelled"));
                }
                self.state
                    .cache
                    .lock()
                    .insert(canonical_sha256, Arc::clone(&artifact));
                self.state
                    .counters
                    .succeeded
                    .fetch_add(1, Ordering::Relaxed);
                Ok(Response::new(response_from_artifact(
                    &request,
                    canonical_sha256,
                    &self.state.engine,
                    artifact.as_ref(),
                    false,
                    timings,
                )))
            })
            .unwrap_or_else(|| {
                if active_guard.complete() {
                    return Err(Status::cancelled("request was cancelled"));
                }
                self.state.counters.failed.fetch_add(1, Ordering::Relaxed);
                Err(Status::unavailable(
                    "worker is quarantined after a generation failure and must be restarted",
                ))
            })
    }

    async fn cancel(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        let request = request.into_inner();
        if request.request_id.len() != 16 || request.canonical_request_sha256.len() != 32 {
            return Err(Status::invalid_argument(
                "request id and canonical digest have invalid lengths",
            ));
        }
        let mut active = self.state.active.lock();
        let Some(entry) = active.get_mut(&request.request_id) else {
            return Ok(Response::new(CancelResponse { found: false }));
        };
        if entry.canonical_sha256.as_slice() != request.canonical_request_sha256 {
            return Err(Status::failed_precondition(
                "canonical request digest does not match active request",
            ));
        }
        entry.cancellation.cancel();
        if !entry.cancellation_recorded {
            entry.cancellation_recorded = true;
            self.state
                .counters
                .cancelled
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(Response::new(CancelResponse { found: true }))
    }

    async fn get_metrics(
        &self,
        _request: Request<GetMetricsRequest>,
    ) -> Result<Response<Metrics>, Status> {
        let counters = &self.state.counters;
        Ok(Response::new(Metrics {
            requests: counters.requests.load(Ordering::Relaxed),
            succeeded: counters.succeeded.load(Ordering::Relaxed),
            failed: counters.failed.load(Ordering::Relaxed),
            cancelled: counters.cancelled.load(Ordering::Relaxed),
            cache_hits: counters.cache_hits.load(Ordering::Relaxed),
            in_flight: u64::try_from(
                self.state.config.max_in_flight - self.state.semaphore.available_permits(),
            )
            .unwrap_or(u64::MAX),
            poisoned: self.state.engine.is_poisoned(),
        }))
    }
}

impl Service {
    fn validate_request(&self, request: &GenerateRequest) -> Result<[u8; 32], Status> {
        if request.request_id.len() != 16 {
            return Err(Status::invalid_argument(
                "request_id must contain exactly 16 bytes",
            ));
        }
        if request.minecraft_version != MINECRAFT_VERSION
            || request.profile_id != self.state.config.profile_id
            || request.dimension_key != self.state.config.dimension_key.to_string()
            || request.seed != self.state.config.seed
            || request.min_y != self.state.engine.min_y
            || request.height != self.state.engine.height as u32
        {
            return Err(Status::failed_precondition(
                "request does not match the pinned worker profile",
            ));
        }
        if request.expected_generator_sha256 != self.state.engine.fingerprints.generator
            || request.expected_registry_sha256 != self.state.engine.fingerprints.registry
        {
            return Err(Status::failed_precondition(
                "request fingerprint does not match the worker",
            ));
        }
        if request.first_stage != Stage::Biomes as i32 || request.last_stage != Stage::Noise as i32
        {
            return Err(Status::failed_precondition(
                "worker only supports BIOMES through NOISE",
            ));
        }
        if request.generation_context != GenerationContext::Fresh as i32 {
            return Err(Status::failed_precondition(
                "worker only supports fresh generation; blending and retrogen are rejected",
            ));
        }
        if request.accepted_compression != [Compression::None as i32] {
            return Err(Status::failed_precondition(
                "worker only supports uncompressed artifact payloads",
            ));
        }
        let pos = ChunkPos::new(request.chunk_x, request.chunk_z);
        if !is_valid_request_chunk_position(pos.0.x, pos.0.y) {
            return Err(Status::invalid_argument(
                "chunk position is outside Minecraft's valid range",
            ));
        }
        canonical_request_sha256(request)
            .map_err(|error| Status::invalid_argument(error.to_string()))
    }
}

fn response_from_artifact(
    request: &GenerateRequest,
    canonical_sha256: [u8; 32],
    engine: &Engine,
    artifact: &EncodedArtifact,
    cache_hit: bool,
    timings: StageTimings,
) -> GenerateResponse {
    GenerateResponse {
        request_id: request.request_id.clone(),
        canonical_request_sha256: canonical_sha256.to_vec(),
        generator_sha256: engine.fingerprints.generator.to_vec(),
        registry_sha256: engine.fingerprints.registry.to_vec(),
        artifact_version: ARTIFACT_VERSION,
        compression: Compression::None as i32,
        uncompressed_size: artifact.bytes.len() as u64,
        artifact_sha256: artifact.sha256.to_vec(),
        artifact: artifact.bytes.clone(),
        cache_hit,
        timings: Some(timings),
    }
}

const fn is_valid_request_chunk_position(x: i32, z: i32) -> bool {
    x != i32::MIN && z != i32::MIN && ChunkPos::is_valid(x, z)
}

fn elapsed_micros(start: Instant) -> u64 {
    duration_micros(start.elapsed())
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1::ChunkArtifactV1;
    use std::thread;

    fn artifact(bytes: usize) -> Arc<EncodedArtifact> {
        Arc::new(EncodedArtifact {
            bytes: vec![0; bytes],
            sha256: [0; 32],
        })
    }

    #[test]
    fn minimum_i32_chunk_coordinate_is_rejected_without_overflow() {
        assert!(!is_valid_request_chunk_position(i32::MIN, 0));
        assert!(!is_valid_request_chunk_position(0, i32::MIN));
        assert!(is_valid_request_chunk_position(0, 0));
    }

    #[test]
    fn cache_evicts_by_encoded_byte_weight() {
        let mut cache = ArtifactCache::new(10, 10);
        cache.insert([1; 32], artifact(6));
        cache.insert([2; 32], artifact(6));

        assert!(cache.get(&[1; 32]).is_none());
        assert!(cache.get(&[2; 32]).is_some());
        assert_eq!(cache.total_bytes, 6);
    }

    #[test]
    fn peer_admission_is_bounded_and_released_per_address() {
        let admission = Arc::new(SyncMutex::new(PeerAdmission {
            max_in_flight: 1,
            in_flight: FxHashMap::default(),
        }));
        let first = PeerPermit::try_acquire(Arc::clone(&admission), None)
            .expect("first internal peer request should be admitted");
        assert!(PeerPermit::try_acquire(Arc::clone(&admission), None).is_none());
        let address = Some(IpAddr::from([127, 0, 0, 1]));
        let network = PeerPermit::try_acquire(Arc::clone(&admission), address)
            .expect("a different network peer should have independent admission");
        drop(first);
        assert!(PeerPermit::try_acquire(Arc::clone(&admission), None).is_some());
        drop(network);
    }

    #[test]
    fn artifact_larger_than_cache_is_not_retained() {
        let mut cache = ArtifactCache::new(10, 5);
        cache.insert([1; 32], artifact(6));

        assert!(cache.get(&[1; 32]).is_none());
        assert_eq!(cache.total_bytes, 0);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end service scenario deliberately shares its expensive real worker fixture"
    )]
    fn real_service_generation_cache_identity_admission_and_cancel() -> anyhow::Result<()> {
        use std::{net::SocketAddr, str::FromStr as _};

        use prost::Message as _;
        use steel_core::bootstrap;
        use steel_utils::Identifier;
        use tokio::runtime::Builder;
        use tonic::Code;

        use crate::{
            client::validate_generate_response,
            proto::v1::{GenerationContext, GetMetricsRequest},
        };

        bootstrap::init_globals().map_err(anyhow::Error::msg)?;
        let runtime = Arc::new(Builder::new_multi_thread().enable_all().build()?);
        let config = Config {
            bind: SocketAddr::from_str("127.0.0.1:0")?,
            profile_id: "service-test".to_owned(),
            dimension_key: Identifier::from_str("minecraft:overworld")
                .map_err(anyhow::Error::msg)?,
            generator_id: Identifier::from_str("minecraft:overworld")
                .map_err(anyhow::Error::msg)?,
            seed: 13_579,
            max_in_flight: 1,
            max_in_flight_per_peer: 1,
            generation_threads: 1,
            request_timeout_ms: 30_000,
            max_cache_entries: 8,
            max_cache_bytes: MAX_ARTIFACT_BYTES * 2,
            corresponding_source_url: "https://example.invalid/steel-source".to_owned(),
            tls_certificate: None,
            tls_private_key: None,
            tls_client_ca: None,
        };
        let engine = runtime.block_on(Engine::new(&config, Arc::clone(&runtime)))?;
        let engine = Arc::new(engine);
        let service = Service::new(config, Arc::clone(&engine));
        let capabilities = service.state.capabilities.clone();
        let request = |request_id: [u8; 16], chunk_x: i32, chunk_z: i32| GenerateRequest {
            request_id: request_id.to_vec(),
            minecraft_version: capabilities.minecraft_version.clone(),
            profile_id: capabilities.profile_id.clone(),
            dimension_key: capabilities.dimension_key.clone(),
            seed: capabilities.seed,
            chunk_x,
            chunk_z,
            min_y: capabilities.min_y,
            height: capabilities.height,
            first_stage: Stage::Biomes as i32,
            last_stage: Stage::Noise as i32,
            expected_generator_sha256: capabilities.generator_sha256.clone(),
            expected_registry_sha256: capabilities.registry_sha256.clone(),
            accepted_compression: vec![Compression::None as i32],
            generation_context: GenerationContext::Fresh as i32,
        };

        let mut invalid = request([0; 16], 0, 0);
        invalid.request_id.pop();
        assert_eq!(
            service
                .validate_request(&invalid)
                .expect_err("short request ID must fail")
                .code(),
            Code::InvalidArgument
        );
        let invalid_mutators: [fn(&mut GenerateRequest); 4] = [
            |request: &mut GenerateRequest| request.minecraft_version = "1.21.1".to_owned(),
            |request: &mut GenerateRequest| request.first_stage = Stage::Surface as i32,
            |request: &mut GenerateRequest| {
                request.generation_context = GenerationContext::Blending as i32;
            },
            |request: &mut GenerateRequest| {
                request.accepted_compression = vec![Compression::Zstd as i32];
            },
        ];
        for mutate in invalid_mutators {
            let mut invalid = request([0; 16], 0, 0);
            mutate(&mut invalid);
            assert_eq!(
                service
                    .validate_request(&invalid)
                    .expect_err("unsupported request must fail")
                    .code(),
                Code::FailedPrecondition
            );
        }
        let invalid_position = request([0; 16], i32::MIN, 0);
        assert_eq!(
            service
                .validate_request(&invalid_position)
                .expect_err("minimum chunk coordinate must fail")
                .code(),
            Code::InvalidArgument
        );

        let cold_request = request([1; 16], -6, 2);
        let cold = runtime
            .block_on(service.generate(Request::new(cold_request.clone())))?
            .into_inner();
        assert!(!cold.cache_hit);
        validate_generate_response(&cold_request, &capabilities, &cold)?;
        let mut generated = ChunkArtifactV1::decode(cold.artifact.as_slice())?;
        let mut golden = ChunkArtifactV1::decode(
            &include_bytes!("../test_assets/noise-v1-overworld-seed-13579-x-6-z2.pb")[..],
        )?;
        generated.generator_sha256.clear();
        generated.canonical_request_sha256.clear();
        golden.generator_sha256.clear();
        golden.canonical_request_sha256.clear();
        assert_eq!(
            generated, golden,
            "current Steel generation/encoding differs semantically from the release golden"
        );

        let warm_request = request([2; 16], -6, 2);
        let warm = runtime
            .block_on(service.generate(Request::new(warm_request.clone())))?
            .into_inner();
        assert!(warm.cache_hit);
        validate_generate_response(&warm_request, &capabilities, &warm)?;
        assert_eq!(cold.artifact, warm.artifact);

        let duplicate_id = vec![3; 16];
        let (mut existing, _) = ActiveGuard::register(
            duplicate_id.clone(),
            [9; 32],
            Arc::clone(&service.state.active),
            Arc::clone(&service.state.counters),
        )?;
        let duplicate = runtime
            .block_on(service.generate(Request::new(request([3; 16], -6, 2))))
            .expect_err("an active request ID must be rejected even for a cache hit");
        assert_eq!(duplicate.code(), Code::AlreadyExists);
        existing.complete();

        let held_permit = runtime.block_on(Arc::clone(&service.state.semaphore).acquire_owned())?;
        let overloaded_id = [4; 16];
        let overloaded = runtime
            .block_on(service.generate(Request::new(request(overloaded_id, 400, 400))))
            .expect_err("admission must reject work when every physical permit is held");
        assert_eq!(overloaded.code(), Code::ResourceExhausted);
        assert!(
            !service
                .state
                .active
                .lock()
                .contains_key(overloaded_id.as_slice())
        );
        drop(held_permit);

        let cancel_id = vec![5; 16];
        let cancel_hash = [7; 32];
        let (mut active, cancellation) = ActiveGuard::register(
            cancel_id.clone(),
            cancel_hash,
            Arc::clone(&service.state.active),
            Arc::clone(&service.state.counters),
        )?;
        let cancel_request = CancelRequest {
            request_id: cancel_id,
            canonical_request_sha256: cancel_hash.to_vec(),
        };
        assert!(
            runtime
                .block_on(service.cancel(Request::new(cancel_request.clone())))?
                .into_inner()
                .found
        );
        assert!(
            runtime
                .block_on(service.cancel(Request::new(cancel_request)))?
                .into_inner()
                .found
        );
        assert!(cancellation.is_cancelled());
        assert!(
            active.complete(),
            "explicit cancellation must win publication"
        );

        let generation_guard = runtime.block_on(engine.hold_generation_lock_for_test());
        let queued_request = request([6; 16], 401, 401);
        let queued_service = service.clone();
        let queued = runtime
            .spawn(async move { queued_service.generate(Request::new(queued_request)).await });
        let queued_id = vec![6; 16];
        for _ in 0..100 {
            if service.state.active.lock().contains_key(&queued_id) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(service.state.active.lock().contains_key(&queued_id));
        let queued_hash = service
            .state
            .active
            .lock()
            .get(&queued_id)
            .expect("queued request disappeared")
            .canonical_sha256;
        let cancelled = runtime
            .block_on(service.cancel(Request::new(CancelRequest {
                request_id: queued_id.clone(),
                canonical_request_sha256: queued_hash.to_vec(),
            })))?
            .into_inner();
        assert!(cancelled.found);
        drop(generation_guard);
        let queued_error = runtime
            .block_on(queued)?
            .expect_err("cancelled queued generation must not publish");
        assert_eq!(queued_error.code(), Code::Cancelled);
        assert!(!service.state.active.lock().contains_key(&queued_id));
        for _ in 0..1000 {
            if service.state.semaphore.available_permits() == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            service.state.semaphore.available_permits(),
            1,
            "physical admission did not drain after queued cancellation"
        );

        let retry_request = request([7; 16], 401, 401);
        let retry = runtime
            .block_on(service.generate(Request::new(retry_request.clone())))?
            .into_inner();
        validate_generate_response(&retry_request, &capabilities, &retry)?;
        assert!(!retry.cache_hit);

        let metrics = runtime
            .block_on(service.get_metrics(Request::new(GetMetricsRequest {})))?
            .into_inner();
        assert_eq!(metrics.succeeded, 3);
        assert_eq!(metrics.cache_hits, 1);
        assert_eq!(metrics.cancelled, 2);
        assert_eq!(metrics.in_flight, 0);
        assert!(!metrics.poisoned);
        assert_eq!(metrics.failed, 2);

        runtime.block_on(service.state.fail_stop_engine());
        assert!(service.state.fatal_shutdown.is_cancelled());
        let quarantined = runtime
            .block_on(service.generate(Request::new(request([8; 16], 500, 500))))
            .expect_err("quarantined worker must reject later generation");
        assert_eq!(quarantined.code(), Code::Unavailable);
        let metrics = runtime
            .block_on(service.get_metrics(Request::new(GetMetricsRequest {})))?
            .into_inner();
        assert!(metrics.poisoned);
        assert_eq!(metrics.failed, 3);

        engine.stop();
        drop(service);
        drop(engine);
        drop(runtime);
        Ok(())
    }
}
