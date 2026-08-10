//! Stable profile and request fingerprints.

use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use sha2::{Digest as _, Sha256};
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_utils::BlockStateId;

use crate::{
    config::Config,
    proto::v1::{GenerateRequest, Stage},
};

const REQUEST_DOMAIN: &[u8; 8] = b"SWGREQ1\0";

/// Canonical source-tree digest embedded by the worker build script.
pub const SOURCE_SHA256: &str = env!("STEEL_WORLDGEN_SOURCE_SHA256");
/// Operator-supplied immutable deployment build identifier, or the local-build marker.
pub const EXTERNAL_BUILD_ID: &str = env!("STEEL_WORLDGEN_EXTERNAL_BUILD_ID");
/// Exact compiler identity used for this worker.
pub const RUSTC_ID: &str = env!("STEEL_WORLDGEN_RUSTC_ID");
/// Exact Cargo frontend identity used for this worker.
pub const CARGO_ID: &str = env!("STEEL_WORLDGEN_CARGO_ID");
/// Rust compilation target triple.
pub const BUILD_TARGET: &str = env!("STEEL_WORLDGEN_TARGET");
/// Resolved profile/code-generation configuration hashed into the generator identity.
pub const BUILD_CONFIGURATION: &str = env!("STEEL_WORLDGEN_BUILD_CONFIGURATION");

/// Digests that bind requests to one exact worker profile.
#[derive(Clone, Copy, Debug)]
pub struct ProfileFingerprints {
    /// Generator implementation, settings, seed, and range digest.
    pub generator: [u8; 32],
    /// Canonical block-state and biome registry digest.
    pub registry: [u8; 32],
    /// Digest combining the named profile with both component digests.
    pub profile: [u8; 32],
}

/// Computes all fingerprints after the global registry has been initialized.
pub fn profile_fingerprints(
    config: &Config,
    min_y: i32,
    height: i32,
) -> Result<ProfileFingerprints> {
    let registry = registry_fingerprint()?;
    let mut generator_hash = Sha256::new();
    put_bytes(&mut generator_hash, b"steel-worldgen-generator-v1");
    put_bytes(&mut generator_hash, env!("CARGO_PKG_VERSION").as_bytes());
    put_bytes(&mut generator_hash, SOURCE_SHA256.as_bytes());
    put_bytes(&mut generator_hash, EXTERNAL_BUILD_ID.as_bytes());
    put_bytes(&mut generator_hash, RUSTC_ID.as_bytes());
    put_bytes(&mut generator_hash, CARGO_ID.as_bytes());
    put_bytes(&mut generator_hash, BUILD_TARGET.as_bytes());
    put_bytes(&mut generator_hash, BUILD_CONFIGURATION.as_bytes());
    put_bytes(
        &mut generator_hash,
        config.generator_id.to_string().as_bytes(),
    );
    put_bytes(
        &mut generator_hash,
        config.dimension_key.to_string().as_bytes(),
    );
    generator_hash.update(config.seed.to_be_bytes());
    generator_hash.update(min_y.to_be_bytes());
    generator_hash.update(height.to_be_bytes());
    let generator: [u8; 32] = generator_hash.finalize().into();

    let mut profile_hash = Sha256::new();
    put_bytes(&mut profile_hash, b"steel-worldgen-profile-v1");
    put_bytes(&mut profile_hash, config.profile_id.as_bytes());
    profile_hash.update(generator);
    profile_hash.update(registry);
    let profile = profile_hash.finalize().into();
    Ok(ProfileFingerprints {
        generator,
        registry,
        profile,
    })
}

/// Computes a canonical fingerprint of every block state and biome key.
pub fn registry_fingerprint() -> Result<[u8; 32]> {
    let mut states = BTreeSet::new();
    for (raw_id, block) in REGISTRY.blocks.state_to_block_lookup.iter().enumerate() {
        let state_id = BlockStateId(u16::try_from(raw_id)?);
        let mut properties = REGISTRY
            .blocks
            .get_properties(state_id)
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        properties.sort_unstable();
        ensure!(
            states.insert((block.key.to_string(), properties)),
            "duplicate canonical block state in registry"
        );
    }

    let mut biomes = BTreeSet::new();
    for id in 0..REGISTRY.biomes.len() {
        let biome = REGISTRY
            .biomes
            .by_id(id)
            .ok_or_else(|| anyhow::anyhow!("biome registry gap at {id}"))?;
        ensure!(
            biomes.insert(biome.key.to_string()),
            "duplicate canonical biome in registry"
        );
    }

    let mut hash = Sha256::new();
    put_bytes(&mut hash, b"steel-worldgen-registry-v2");
    hash.update(u32::try_from(states.len())?.to_be_bytes());
    for (name, properties) in states {
        put_bytes(&mut hash, name.as_bytes());
        hash.update(u32::try_from(properties.len())?.to_be_bytes());
        for (property, value) in properties {
            put_bytes(&mut hash, property.as_bytes());
            put_bytes(&mut hash, value.as_bytes());
        }
    }
    hash.update(u32::try_from(biomes.len())?.to_be_bytes());
    for biome in biomes {
        put_bytes(&mut hash, biome.as_bytes());
    }
    Ok(hash.finalize().into())
}

/// Computes the protocol-defined semantic request key.
pub fn canonical_request_sha256(request: &GenerateRequest) -> Result<[u8; 32]> {
    ensure!(
        request.expected_generator_sha256.len() == 32,
        "generator digest must contain 32 bytes"
    );
    ensure!(
        request.expected_registry_sha256.len() == 32,
        "registry digest must contain 32 bytes"
    );
    ensure!(
        request.first_stage == Stage::Biomes as i32,
        "first stage must be BIOMES"
    );
    ensure!(
        request.last_stage == Stage::Noise as i32,
        "last stage must be NOISE"
    );

    let mut preimage = Vec::with_capacity(192);
    preimage.extend_from_slice(REQUEST_DOMAIN);
    put_u16_bytes(&mut preimage, request.minecraft_version.as_bytes())?;
    put_u16_bytes(&mut preimage, request.dimension_key.as_bytes())?;
    preimage.extend_from_slice(&request.seed.to_be_bytes());
    preimage.extend_from_slice(&request.chunk_x.to_be_bytes());
    preimage.extend_from_slice(&request.chunk_z.to_be_bytes());
    preimage.extend_from_slice(&request.min_y.to_be_bytes());
    preimage.extend_from_slice(&request.height.to_be_bytes());
    preimage.push(u8::try_from(request.first_stage)?);
    preimage.push(u8::try_from(request.last_stage)?);
    preimage.extend_from_slice(&request.expected_generator_sha256);
    preimage.extend_from_slice(&request.expected_registry_sha256);
    preimage.extend_from_slice(&0_u16.to_be_bytes());
    Ok(Sha256::digest(&preimage).into())
}

fn put_bytes(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

fn put_u16_bytes(output: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    output.extend_from_slice(&u16::try_from(value.len())?.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1::{Compression, GenerationContext};

    #[test]
    fn canonical_request_matches_documented_vector() {
        let request = GenerateRequest {
            request_id: vec![0; 16],
            minecraft_version: "26.2".to_owned(),
            profile_id: "ignored-by-canonical-key".to_owned(),
            dimension_key: "minecraft:overworld".to_owned(),
            seed: 13_579,
            chunk_x: 0,
            chunk_z: 0,
            min_y: -64,
            height: 384,
            first_stage: Stage::Biomes as i32,
            last_stage: Stage::Noise as i32,
            expected_generator_sha256: vec![0; 32],
            expected_registry_sha256: vec![0xff; 32],
            accepted_compression: vec![Compression::None as i32],
            generation_context: GenerationContext::Fresh as i32,
        };
        assert_eq!(
            hex::encode(canonical_request_sha256(&request).expect("valid vector")),
            "d63f74fb044c0c93fbd48b1fdca3a4ef20c81d6b6e51b4b78d5cc0462c2c1c68"
        );
    }
}
