//! Client connection policy shared by worker diagnostics.

use std::{env, path::PathBuf};

use anyhow::{Context as _, Result, ensure};
use prost::Message as _;
use sha2::{Digest as _, Sha256};
use tokio::fs::read;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};

use crate::{
    artifact::{ARTIFACT_VERSION, MAX_ARTIFACT_BYTES, validate_artifact},
    config::MINECRAFT_VERSION,
    fingerprint::{
        BUILD_CONFIGURATION, BUILD_TARGET, CARGO_ID, EXTERNAL_BUILD_ID, RUSTC_ID, SOURCE_SHA256,
        canonical_request_sha256,
    },
    proto::v1::{
        Capabilities, ChunkArtifactV1, Compression, GenerateRequest, GenerateResponse, Stage,
        world_gen_service_client::WorldGenServiceClient,
    },
};

/// Connects a diagnostic gRPC channel with optional server-authenticated TLS or mTLS.
///
/// `STEEL_WORLDGEN_CLIENT_CA` and `..._DOMAIN` are required together. Supplying the
/// optional client identity additionally requires both `..._CERT` and `..._KEY`.
pub async fn connect_channel(endpoint: &str) -> Result<Channel> {
    let ca = optional_env("STEEL_WORLDGEN_CLIENT_CA")?;
    let certificate = optional_env("STEEL_WORLDGEN_CLIENT_CERT")?;
    let key = optional_env("STEEL_WORLDGEN_CLIENT_KEY")?;
    let domain = optional_env("STEEL_WORLDGEN_CLIENT_DOMAIN")?;
    ensure!(
        ca.is_some() == domain.is_some(),
        "client TLS requires STEEL_WORLDGEN_CLIENT_CA and STEEL_WORLDGEN_CLIENT_DOMAIN together"
    );
    ensure!(
        certificate.is_some() == key.is_some(),
        "a client TLS identity requires both STEEL_WORLDGEN_CLIENT_CERT and STEEL_WORLDGEN_CLIENT_KEY"
    );
    ensure!(
        certificate.is_none() || ca.is_some(),
        "a client TLS identity requires a configured CA and domain"
    );
    let mut endpoint_config = Endpoint::from_shared(endpoint.to_owned())
        .with_context(|| format!("invalid worker endpoint {endpoint}"))?;
    if let (Some(ca), Some(domain)) = (ca, domain) {
        let ca_bytes = read(PathBuf::from(&ca))
            .await
            .with_context(|| format!("failed to read client CA {ca}"))?;
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca_bytes))
            .domain_name(domain);
        if let (Some(certificate), Some(key)) = (certificate, key) {
            let certificate_bytes = read(PathBuf::from(&certificate))
                .await
                .with_context(|| format!("failed to read client certificate {certificate}"))?;
            let key_bytes = read(PathBuf::from(&key))
                .await
                .with_context(|| format!("failed to read client key {key}"))?;
            tls = tls.identity(Identity::from_pem(certificate_bytes, key_bytes));
        }
        endpoint_config = endpoint_config.tls_config(tls)?;
    }
    endpoint_config
        .connect()
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))
}

/// Connects a generated world-generation client.
pub async fn connect(endpoint: &str) -> Result<WorldGenServiceClient<Channel>> {
    Ok(WorldGenServiceClient::new(connect_channel(endpoint).await?))
}

/// Validates the immutable capability contract used by diagnostic clients.
pub fn validate_capabilities(capabilities: &Capabilities) -> Result<()> {
    ensure!(
        capabilities.protocol_major == 1,
        "unsupported protocol major"
    );
    ensure!(
        capabilities.artifact_versions.contains(&ARTIFACT_VERSION),
        "worker does not support artifact V1"
    );
    ensure!(
        capabilities.minecraft_version == MINECRAFT_VERSION,
        "worker targets a different Minecraft version"
    );
    ensure!(
        !capabilities.profile_id.is_empty()
            && capabilities.profile_id.len() <= 128
            && capabilities.profile_id.is_ascii()
            && !capabilities
                .profile_id
                .bytes()
                .any(|byte| byte.is_ascii_control()),
        "invalid worker profile id"
    );
    ensure!(
        capabilities.generator_sha256.len() == 32
            && capabilities.registry_sha256.len() == 32
            && capabilities.profile_sha256.len() == 32,
        "invalid worker fingerprint length"
    );
    ensure!(
        capabilities.completed_stages == [Stage::Biomes as i32, Stage::Noise as i32],
        "worker advertises an unsupported stage interval"
    );
    ensure!(
        capabilities.compression == [Compression::None as i32],
        "worker advertises unknown or unsupported artifact compression"
    );
    ensure!(
        capabilities.max_request_bytes == 64 * 1024,
        "worker request bound differs from this client"
    );
    ensure!(
        capabilities.max_artifact_bytes as usize == MAX_ARTIFACT_BYTES,
        "worker artifact bound differs from this client"
    );
    ensure!(
        (1..=4096).contains(&capabilities.max_in_flight)
            && (1..=capabilities.max_in_flight).contains(&capabilities.max_in_flight_per_peer),
        "invalid worker global or per-peer concurrency bound"
    );
    ensure!(
        !capabilities.supports_blending
            && !capabilities.supports_retrogen
            && !capabilities.steel_resumable,
        "worker advertises unsupported generation semantics"
    );
    ensure!(
        capabilities.protocol_minor >= 1
            && capabilities.source_sha256 == SOURCE_SHA256
            && capabilities.license_expression == "AGPL-3.0-or-later"
            && capabilities.corresponding_source_url.len() <= 2048
            && (capabilities
                .corresponding_source_url
                .starts_with("https://")
                || capabilities.corresponding_source_url.starts_with("http://")),
        "worker corresponding-source offer is absent or does not match this build"
    );
    ensure!(
        capabilities.external_build_id == EXTERNAL_BUILD_ID
            && capabilities.rustc_id == RUSTC_ID
            && capabilities.cargo_id == CARGO_ID
            && capabilities.build_target == BUILD_TARGET
            && capabilities.build_configuration == BUILD_CONFIGURATION,
        "worker build attestation does not match this diagnostic client"
    );
    Ok(())
}

/// Hashes, decodes, and binds a response artifact to its exact request and capabilities.
pub fn validate_generate_response(
    request: &GenerateRequest,
    capabilities: &Capabilities,
    response: &GenerateResponse,
) -> Result<ChunkArtifactV1> {
    ensure!(
        response.request_id == request.request_id,
        "response request id mismatch"
    );
    let canonical = canonical_request_sha256(request)?;
    ensure!(
        response.canonical_request_sha256 == canonical,
        "response canonical request digest mismatch"
    );
    ensure!(
        response.generator_sha256 == capabilities.generator_sha256
            && response.generator_sha256 == request.expected_generator_sha256,
        "response generator fingerprint mismatch"
    );
    ensure!(
        response.registry_sha256 == capabilities.registry_sha256
            && response.registry_sha256 == request.expected_registry_sha256,
        "response registry fingerprint mismatch"
    );
    ensure!(
        response.artifact_version == ARTIFACT_VERSION,
        "response artifact version mismatch"
    );
    ensure!(
        response.compression == Compression::None as i32,
        "response uses unsupported artifact compression"
    );
    ensure!(
        response.artifact.len() <= MAX_ARTIFACT_BYTES
            && response.uncompressed_size == response.artifact.len() as u64,
        "response artifact size mismatch"
    );
    let artifact_sha256: [u8; 32] = Sha256::digest(&response.artifact).into();
    ensure!(
        response.artifact_sha256 == artifact_sha256,
        "response artifact digest mismatch"
    );

    let artifact = ChunkArtifactV1::decode(response.artifact.as_slice())?;
    validate_artifact(&artifact)?;
    ensure!(
        artifact.minecraft_version == request.minecraft_version
            && artifact.canonical_request_sha256 == canonical
            && artifact.generator_sha256 == response.generator_sha256
            && artifact.registry_sha256 == response.registry_sha256
            && artifact.dimension_key == request.dimension_key
            && artifact.seed == request.seed
            && artifact.chunk_x == request.chunk_x
            && artifact.chunk_z == request.chunk_z
            && artifact.min_y == request.min_y
            && artifact.height == request.height,
        "artifact identity does not match its request"
    );
    Ok(artifact)
}

fn optional_env(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => {
            ensure!(!value.is_empty(), "{name} must not be empty");
            Ok(Some(value))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(anyhow::anyhow!("failed to read {name}: {error}")),
    }
}
