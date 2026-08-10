//! Concurrent cold/warm load benchmark for the world-generation worker.

use std::{
    env,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use futures::{StreamExt as _, stream};
use serde_json::json;
use steel_worldgen_service::{
    artifact::MAX_ARTIFACT_BYTES,
    client::{connect, validate_capabilities, validate_generate_response},
    proto::v1::{
        Capabilities, Compression, GenerateRequest, GenerationContext, GetCapabilitiesRequest,
        GetMetricsRequest, Stage, world_gen_service_client::WorldGenServiceClient,
    },
};
use tonic::{Request, codec::CompressionEncoding, transport::Channel};

#[derive(Clone, Copy)]
struct Job {
    index: u32,
    x: i32,
    z: i32,
}

struct JobResult {
    elapsed: Duration,
    bytes: usize,
    cache_hit: bool,
    queue_micros: u64,
    generation_micros: u64,
    encode_micros: u64,
}

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "benchmark command keeps one auditable cold/warm measurement sequence"
)]
async fn main() -> Result<()> {
    let endpoint =
        env::var("STEEL_WORLDGEN_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:50051".to_owned());
    let side = parse_usize("STEEL_WORLDGEN_BENCH_SIDE", 10)?;
    let concurrency = parse_usize("STEEL_WORLDGEN_BENCH_CONCURRENCY", 8)?;
    let warm = parse_usize("STEEL_WORLDGEN_BENCH_WARM_PASSES", 1)?;
    let start_x = parse_i32("STEEL_WORLDGEN_BENCH_START_X", 0)?;
    let start_z = parse_i32("STEEL_WORLDGEN_BENCH_START_Z", 0)?;
    ensure!(
        side > 0 && side <= 1024,
        "benchmark side must be in 1..=1024"
    );
    ensure!(
        concurrency > 0 && concurrency <= 4096,
        "benchmark concurrency must be in 1..=4096"
    );

    let mut client = connect(&endpoint)
        .await?
        .send_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Gzip)
        .max_decoding_message_size(MAX_ARTIFACT_BYTES + 64 * 1024);
    let capabilities = client
        .get_capabilities(GetCapabilitiesRequest {})
        .await?
        .into_inner();
    validate_capabilities(&capabilities)?;
    ensure!(
        concurrency <= usize::try_from(capabilities.max_in_flight_per_peer)?,
        "benchmark concurrency exceeds the worker's advertised per-peer generation capacity ({})",
        capabilities.max_in_flight_per_peer
    );
    let metrics_before = client.get_metrics(GetMetricsRequest {}).await?.into_inner();
    ensure!(
        !metrics_before.poisoned,
        "worker is quarantined before benchmark"
    );
    let mut jobs = Vec::with_capacity(side * side);
    for z in 0..side {
        for x in 0..side {
            jobs.push(Job {
                index: u32::try_from(jobs.len())?,
                x: start_x
                    .checked_add(i32::try_from(x)?)
                    .context("benchmark X grid exceeds i32")?,
                z: start_z
                    .checked_add(i32::try_from(z)?)
                    .context("benchmark Z grid exceeds i32")?,
            });
        }
    }

    let mut passes = Vec::with_capacity(warm + 1);
    for pass in 0..=warm {
        passes.push(
            run_pass(
                u32::try_from(pass)?,
                &jobs,
                concurrency,
                &client,
                &capabilities,
            )
            .await?,
        );
    }
    let metrics = client.get_metrics(GetMetricsRequest {}).await?.into_inner();
    let requests = metrics.requests.saturating_sub(metrics_before.requests);
    let succeeded = metrics.succeeded.saturating_sub(metrics_before.succeeded);
    let failed = metrics.failed.saturating_sub(metrics_before.failed);
    let cancelled = metrics.cancelled.saturating_sub(metrics_before.cancelled);
    let cache_hits = metrics.cache_hits.saturating_sub(metrics_before.cache_hits);
    let expected_requests = u64::try_from(
        jobs.len()
            .checked_mul(warm + 1)
            .context("benchmark request count overflow")?,
    )?;
    ensure!(
        requests == expected_requests && succeeded == expected_requests,
        "worker metrics do not match the benchmark request count"
    );
    ensure!(
        failed == 0 && cancelled == 0,
        "worker recorded a benchmark failure or cancellation"
    );
    ensure!(
        metrics.in_flight == 0,
        "worker still reports physical work after the benchmark completed"
    );
    ensure!(
        !metrics.poisoned,
        "worker became quarantined during benchmark"
    );
    ensure!(
        cache_hits
            == u64::try_from(
                jobs.len()
                    .checked_mul(warm)
                    .context("benchmark cache-hit count overflow")?
            )?,
        "worker cache metrics do not match cold/warm pass labels"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "endpoint": endpoint,
            "minecraft_version": capabilities.minecraft_version,
            "steel_version": capabilities.steel_version,
            "profile_sha256": hex::encode(&capabilities.profile_sha256),
            "generator_sha256": hex::encode(&capabilities.generator_sha256),
            "registry_sha256": hex::encode(&capabilities.registry_sha256),
            "build": {
                "source_sha256": capabilities.source_sha256,
                "source_url": capabilities.corresponding_source_url,
                "license": capabilities.license_expression,
                "external_build_id": capabilities.external_build_id,
                "rustc_id": capabilities.rustc_id,
                "cargo_id": capabilities.cargo_id,
                "target": capabilities.build_target,
                "configuration": capabilities.build_configuration,
            },
            "grid": {"start_x": start_x, "start_z": start_z, "side": side},
            "jobs": jobs.len(),
            "concurrency": concurrency,
            "worker_max_in_flight": capabilities.max_in_flight,
            "worker_max_in_flight_per_peer": capabilities.max_in_flight_per_peer,
            "passes": passes,
            "worker_metrics_delta": {
                "requests": requests,
                "succeeded": succeeded,
                "failed": failed,
                "cancelled": cancelled,
                "cache_hits": cache_hits,
                "in_flight_after": metrics.in_flight,
            },
        }))?
    );
    Ok(())
}

async fn run_pass(
    pass: u32,
    jobs: &[Job],
    concurrency: usize,
    client: &WorldGenServiceClient<Channel>,
    capabilities: &Capabilities,
) -> Result<serde_json::Value> {
    let pass_started = Instant::now();
    let results = stream::iter(jobs.iter().copied())
        .map(|job| generate_one(pass, job, client.clone(), capabilities.clone()))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let elapsed = pass_started.elapsed();
    let mut latencies = results
        .iter()
        .map(|result| duration_micros(result.elapsed))
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    let total_bytes = results.iter().map(|result| result.bytes).sum::<usize>();
    let cache_hits = results.iter().filter(|result| result.cache_hit).count();
    if pass == 0 {
        ensure!(
            cache_hits == 0,
            "cold benchmark pass encountered a prewarmed cache entry"
        );
    } else {
        ensure!(
            cache_hits == results.len(),
            "warm benchmark pass missed the worker cache"
        );
    }
    Ok(json!({
        "pass": pass,
        "kind": if pass == 0 { "cold" } else { "warm" },
        "elapsed_micros": duration_micros(elapsed),
        "chunks_per_second": results.len() as f64 / elapsed.as_secs_f64(),
        "latency_micros": {
            "min": latencies.first().copied().unwrap_or_default(),
            "p50": percentile(&latencies, 50),
            "p95": percentile(&latencies, 95),
            "p99": percentile(&latencies, 99),
            "max": latencies.last().copied().unwrap_or_default(),
        },
        "artifact_bytes": total_bytes,
        "cache_hits": cache_hits,
        "worker_stage_micros": {
            "queue_total": results.iter().map(|result| result.queue_micros).sum::<u64>(),
            "generation_total": results.iter().map(|result| result.generation_micros).sum::<u64>(),
            "encode_total": results.iter().map(|result| result.encode_micros).sum::<u64>(),
        },
    }))
}

async fn generate_one(
    pass: u32,
    job: Job,
    mut client: WorldGenServiceClient<Channel>,
    capabilities: Capabilities,
) -> Result<JobResult> {
    let request = GenerateRequest {
        request_id: request_id(pass, job).to_vec(),
        minecraft_version: capabilities.minecraft_version.clone(),
        profile_id: capabilities.profile_id.clone(),
        dimension_key: capabilities.dimension_key.clone(),
        seed: capabilities.seed,
        chunk_x: job.x,
        chunk_z: job.z,
        min_y: capabilities.min_y,
        height: capabilities.height,
        first_stage: Stage::Biomes as i32,
        last_stage: Stage::Noise as i32,
        expected_generator_sha256: capabilities.generator_sha256.clone(),
        expected_registry_sha256: capabilities.registry_sha256.clone(),
        accepted_compression: vec![Compression::None as i32],
        generation_context: GenerationContext::Fresh as i32,
    };
    let mut grpc_request = Request::new(request.clone());
    grpc_request.set_timeout(Duration::from_secs(120));
    let started = Instant::now();
    let response = client.generate(grpc_request).await?.into_inner();
    let elapsed = started.elapsed();
    let _artifact = validate_generate_response(&request, &capabilities, &response)?;
    let timings = response.timings.context("response omitted timings")?;
    Ok(JobResult {
        elapsed,
        bytes: response.artifact.len(),
        cache_hit: response.cache_hit,
        queue_micros: timings.queue_micros,
        generation_micros: timings.generation_micros,
        encode_micros: timings.encode_micros,
    })
}

fn request_id(pass: u32, job: Job) -> [u8; 16] {
    let mut id = [0_u8; 16];
    id[..4].copy_from_slice(&pass.to_be_bytes());
    id[4..8].copy_from_slice(&job.index.to_be_bytes());
    id[8..12].copy_from_slice(&job.x.to_be_bytes());
    id[12..].copy_from_slice(&job.z.to_be_bytes());
    id
}

const fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len() - 1) * percentile / 100;
    sorted[index]
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn parse_i32(name: &str, default: i32) -> Result<i32> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("{name} must be an i32"))
}

fn parse_usize(name: &str, default: usize) -> Result<usize> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("{name} must be an unsigned integer"))
}
