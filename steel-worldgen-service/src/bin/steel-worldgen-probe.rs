//! One-request correctness probe for a running Steel world-generation worker.

use std::{env, fs::write, process::id as process_id, time::Duration, time::Instant};

use anyhow::{Context as _, Error, Result, ensure};
use serde_json::json;
use steel_worldgen_service::{
    artifact::MAX_ARTIFACT_BYTES,
    client::{connect, connect_channel, validate_capabilities, validate_generate_response},
    fingerprint::canonical_request_sha256,
    proto::v1::{
        CancelRequest, Capabilities, Compression, GenerateRequest, GenerationContext,
        GetCapabilitiesRequest, GetMetricsRequest, Stage,
        world_gen_service_client::WorldGenServiceClient,
    },
};
use tokio::time::sleep;
use tonic::{codec::CompressionEncoding, transport::Channel};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "probe command keeps its single request/evidence sequence visibly linear"
)]
async fn main() -> Result<()> {
    let endpoint =
        env::var("STEEL_WORLDGEN_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:50051".to_owned());
    if env::var("STEEL_WORLDGEN_HEALTH_ONLY").as_deref() == Ok("true") {
        let channel = connect_channel(&endpoint).await?;
        let response = HealthClient::new(channel)
            .check(HealthCheckRequest {
                service: "steel.worldgen.v1.WorldGenService".to_owned(),
            })
            .await?
            .into_inner();
        ensure!(
            response.status == ServingStatus::Serving as i32,
            "worker health service is not serving"
        );
        println!("{{\"ok\":true,\"health\":\"SERVING\"}}");
        return Ok(());
    }
    if env::var("STEEL_WORLDGEN_METRICS_ONLY").as_deref() == Ok("true") {
        let mut client = connect(&endpoint).await?;
        let metrics = client.get_metrics(GetMetricsRequest {}).await?.into_inner();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "ok": true,
                "requests": metrics.requests,
                "succeeded": metrics.succeeded,
                "failed": metrics.failed,
                "cancelled": metrics.cancelled,
                "cache_hits": metrics.cache_hits,
                "in_flight": metrics.in_flight,
            }))?
        );
        return Ok(());
    }
    let chunk_x = parse_coordinate("STEEL_WORLDGEN_CHUNK_X")?;
    let chunk_z = parse_coordinate("STEEL_WORLDGEN_CHUNK_Z")?;
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
    if env::var("STEEL_WORLDGEN_CANCEL_TEST").as_deref() == Ok("true") {
        run_cancellation_test(client, &capabilities).await?;
        return Ok(());
    }

    let request = make_request(
        &capabilities,
        chunk_x,
        chunk_z,
        request_id(chunk_x, chunk_z),
    );
    let started = Instant::now();
    let response = client.generate(request.clone()).await?.into_inner();
    let artifact = validate_generate_response(&request, &capabilities, &response)?;
    if let Ok(path) = env::var("STEEL_WORLDGEN_ARTIFACT_OUT") {
        write(&path, &response.artifact)
            .with_context(|| format!("failed to write artifact fixture to {path}"))?;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "endpoint": endpoint,
            "minecraft_version": capabilities.minecraft_version,
            "steel_version": capabilities.steel_version,
            "profile_id": capabilities.profile_id,
            "profile_sha256": hex::encode(capabilities.profile_sha256),
            "generator_sha256": hex::encode(capabilities.generator_sha256),
            "registry_sha256": hex::encode(capabilities.registry_sha256),
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
            "chunk": [chunk_x, chunk_z],
            "artifact_bytes": response.artifact.len(),
            "artifact_sha256": hex::encode(response.artifact_sha256),
            "block_states": artifact.block_states.len(),
            "block_state_names": artifact.block_states.iter().map(|state| state.name.as_str()).collect::<Vec<_>>(),
            "biomes": artifact.biomes.len(),
            "biome_names": artifact.biomes,
            "sections": artifact.sections.len(),
            "postprocessing_positions": artifact.postprocessing.iter().map(|section| section.packed_offsets.len()).sum::<usize>(),
            "cache_hit": response.cache_hit,
            "client_elapsed_micros": started.elapsed().as_micros(),
            "worker_timings": response.timings.map(|timings| json!({
                "queue_micros": timings.queue_micros,
                "generation_micros": timings.generation_micros,
                "encode_micros": timings.encode_micros,
                "total_micros": timings.total_micros,
            })),
        }))?
    );
    Ok(())
}

fn make_request(
    capabilities: &Capabilities,
    chunk_x: i32,
    chunk_z: i32,
    request_id: [u8; 16],
) -> GenerateRequest {
    GenerateRequest {
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
    }
}

async fn run_cancellation_test(
    mut client: WorldGenServiceClient<Channel>,
    capabilities: &Capabilities,
) -> Result<()> {
    let chunk_x = parse_coordinate_with_default("STEEL_WORLDGEN_CHUNK_X", 12_345)?;
    let chunk_z = parse_coordinate_with_default("STEEL_WORLDGEN_CHUNK_Z", -12_345)?;
    let timeout_ms = env::var("STEEL_WORLDGEN_CANCEL_TEST_TIMEOUT_MS")
        .unwrap_or_else(|_| "30000".to_owned())
        .parse::<u64>()
        .context("STEEL_WORLDGEN_CANCEL_TEST_TIMEOUT_MS must be a u64")?;
    ensure!(
        (1..=600_000).contains(&timeout_ms),
        "cancellation test timeout must be in 1..=600000 ms"
    );
    let test_timeout = Duration::from_millis(timeout_ms);
    let test_deadline = Instant::now() + test_timeout;
    let request = make_request(capabilities, chunk_x, chunk_z, request_id(chunk_x, chunk_z));
    let canonical = canonical_request_sha256(&request)?;
    let mut generation_client = client.clone();
    let generation = tokio::spawn(async move { generation_client.generate(request).await });

    let mut found = false;
    let registration_deadline = test_deadline;
    while Instant::now() < registration_deadline {
        if generation.is_finished() {
            break;
        }
        let remaining = registration_deadline.saturating_duration_since(Instant::now());
        let response = timeout(
            remaining,
            client.cancel(CancelRequest {
                request_id: request_id(chunk_x, chunk_z).to_vec(),
                canonical_request_sha256: canonical.to_vec(),
            }),
        )
        .await
        .context("timed out while registering cancellation")??
        .into_inner();
        if response.found {
            found = true;
            break;
        }
        sleep(Duration::from_millis(1)).await;
    }
    ensure!(
        found,
        "generation finished before cancellation became active"
    );
    let remaining = test_deadline.saturating_duration_since(Instant::now());
    let Err(status) = timeout(remaining, generation)
        .await
        .context("cancelled Generate did not return before the test deadline")?
        .context("cancellation generation task failed")?
    else {
        return Err(Error::msg(
            "cancelled Generate unexpectedly returned an artifact",
        ));
    };
    ensure!(
        status.code() == tonic::Code::Cancelled,
        "Generate did not return CANCELLED"
    );
    let mut physical_work_drained = false;
    let drain_deadline = test_deadline;
    while Instant::now() < drain_deadline {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        let metrics = timeout(remaining, client.get_metrics(GetMetricsRequest {}))
            .await
            .context("timed out while checking physical cancellation drain")??
            .into_inner();
        if metrics.in_flight == 0 {
            physical_work_drained = true;
            break;
        }
        sleep(Duration::from_millis(1)).await;
    }
    ensure!(
        physical_work_drained,
        "cancelled physical work did not drain"
    );
    println!(
        "{}",
        serde_json::to_string(&json!({
            "ok": true,
            "cancelled": true,
            "chunk": [chunk_x, chunk_z],
            "canonical_request_sha256": hex::encode(canonical),
        }))?
    );
    Ok(())
}

fn parse_coordinate(name: &str) -> Result<i32> {
    parse_coordinate_with_default(name, 0)
}

fn parse_coordinate_with_default(name: &str, default: i32) -> Result<i32> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("{name} must be an i32"))
}

fn request_id(chunk_x: i32, chunk_z: i32) -> [u8; 16] {
    let mut id = [0_u8; 16];
    id[..4].copy_from_slice(&process_id().to_be_bytes());
    id[4..8].copy_from_slice(&chunk_x.to_be_bytes());
    id[8..12].copy_from_slice(&chunk_z.to_be_bytes());
    id[12..].copy_from_slice(&0x5357_4731_u32.to_be_bytes());
    id
}
use tokio::time::timeout;
