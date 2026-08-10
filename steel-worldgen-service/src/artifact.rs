//! Deterministic detached chunk artifact encoding.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, anyhow, ensure};
use prost::Message as _;
use rustc_hash::FxHashMap;
use sha2::{Digest as _, Sha256};
use steel_core::chunk::{
    Chunk,
    heightmap::{ChunkHeightmaps, HeightmapType as SteelHeightmapType},
};
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_utils::{BlockStateId, types::Identifier};

use crate::proto::v1::{
    self, BlockProperty, BlockState, ChunkArtifactV1, ChunkSection, Heightmap, PackedPalette,
    PostProcessingSection, Stage,
};

/// Current detached artifact schema version.
pub const ARTIFACT_VERSION: u32 = 1;
/// Maximum uncompressed artifact size accepted by this implementation.
pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_BLOCK_STATE_DICTIONARY: usize = 65_536;
const MAX_BIOME_DICTIONARY: usize = 4_096;

/// Immutable metadata bound into one artifact.
#[derive(Clone)]
pub struct ArtifactContext {
    /// Exact Minecraft data version.
    pub minecraft_version: String,
    /// Canonical semantic request digest.
    pub canonical_request_sha256: [u8; 32],
    /// Exact generator implementation and configuration digest.
    pub generator_sha256: [u8; 32],
    /// Canonical block-state and biome registry digest.
    pub registry_sha256: [u8; 32],
    /// Namespaced loaded dimension key.
    pub dimension_key: String,
    /// Pinned world seed.
    pub seed: i64,
}

/// Encoded protobuf artifact and its content digest.
#[derive(Clone)]
pub struct EncodedArtifact {
    /// Exact uncompressed protobuf bytes.
    pub bytes: Vec<u8>,
    /// SHA-256 of `bytes`.
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CanonicalBlockState {
    name: String,
    properties: Vec<(String, String)>,
}

impl CanonicalBlockState {
    fn from_id(state_id: BlockStateId) -> Result<Self> {
        let block = REGISTRY
            .blocks
            .by_state_id(state_id)
            .ok_or_else(|| anyhow!("unknown block state id {}", state_id.0))?;
        let mut properties = REGISTRY
            .blocks
            .get_properties(state_id)
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        properties.sort_unstable();
        Ok(Self {
            name: block.key.to_string(),
            properties,
        })
    }

    fn into_proto(self) -> BlockState {
        BlockState {
            name: self.name,
            properties: self
                .properties
                .into_iter()
                .map(|(name, value)| BlockProperty { name, value })
                .collect(),
        }
    }
}

/// Snapshots a Steel NOISE chunk into the versioned detached artifact.
pub fn encode_chunk(chunk: &Chunk, context: &ArtifactContext) -> Result<EncodedArtifact> {
    ensure!(chunk.height() > 0, "chunk height must be positive");
    ensure!(
        chunk.height() % 16 == 0 && chunk.min_y() % 16 == 0,
        "chunk world range must be section aligned"
    );

    let section_snapshots = chunk
        .sections()
        .sections
        .iter()
        .map(|holder| {
            let section = holder.read();
            (
                section.states.collect_values(),
                section.biomes.collect_values(),
            )
        })
        .collect::<Vec<_>>();

    let (block_states, state_dictionary) = block_state_dictionary(&section_snapshots)?;
    let (biomes, biome_dictionary) = biome_dictionary(&section_snapshots)?;
    let first_section_y = chunk.min_y().div_euclid(16);
    let sections = section_snapshots
        .into_iter()
        .enumerate()
        .map(|(index, (states, section_biomes))| {
            let state_indices = states
                .into_iter()
                .map(|state| {
                    state_dictionary
                        .get(&state)
                        .copied()
                        .ok_or_else(|| anyhow!("block state disappeared from dictionary"))
                })
                .collect::<Result<Vec<_>>>()?;
            let biome_indices = section_biomes
                .into_iter()
                .map(|biome| {
                    biome_dictionary
                        .get(&biome)
                        .copied()
                        .ok_or_else(|| anyhow!("biome disappeared from dictionary"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ChunkSection {
                section_y: first_section_y + i32::try_from(index)?,
                block_states: Some(local_palette(&state_indices)?),
                biomes: Some(local_palette(&biome_indices)?),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let heightmaps = snapshot_heightmaps(chunk)?;
    let postprocessing = chunk
        .postprocessing
        .lock()
        .iter()
        .enumerate()
        .map(|(index, offsets)| {
            Ok(PostProcessingSection {
                section_y: first_section_y + i32::try_from(index)?,
                packed_offsets: offsets.iter().copied().map(u32::from).collect(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let artifact = ChunkArtifactV1 {
        artifact_version: ARTIFACT_VERSION,
        minecraft_version: context.minecraft_version.clone(),
        canonical_request_sha256: context.canonical_request_sha256.to_vec(),
        generator_sha256: context.generator_sha256.to_vec(),
        registry_sha256: context.registry_sha256.to_vec(),
        dimension_key: context.dimension_key.clone(),
        seed: context.seed,
        chunk_x: chunk.pos().0.x,
        chunk_z: chunk.pos().0.y,
        min_y: chunk.min_y(),
        height: u32::try_from(chunk.height())?,
        completed_stages: vec![Stage::Biomes as i32, Stage::Noise as i32],
        block_states,
        biomes,
        sections,
        heightmaps,
        postprocessing,
    };

    validate_artifact(&artifact)?;
    let bytes = artifact.encode_to_vec();
    ensure!(
        bytes.len() <= MAX_ARTIFACT_BYTES,
        "artifact exceeds size limit"
    );
    let sha256 = Sha256::digest(&bytes).into();
    Ok(EncodedArtifact { bytes, sha256 })
}

fn block_state_dictionary(
    snapshots: &[(Vec<BlockStateId>, Vec<u16>)],
) -> Result<(Vec<BlockState>, FxHashMap<BlockStateId, u32>)> {
    let mut by_description = BTreeMap::<CanonicalBlockState, BlockStateId>::new();
    for state_id in snapshots
        .iter()
        .flat_map(|(states, _)| states.iter())
        .copied()
    {
        let canonical = CanonicalBlockState::from_id(state_id)?;
        if let Some(previous) = by_description.insert(canonical, state_id) {
            ensure!(
                previous == state_id,
                "multiple Steel state ids have one canonical block state"
            );
        }
    }

    let mut dictionary = FxHashMap::default();
    let mut states = Vec::with_capacity(by_description.len());
    for (index, (canonical, state_id)) in by_description.into_iter().enumerate() {
        dictionary.insert(state_id, u32::try_from(index)?);
        states.push(canonical.into_proto());
    }
    Ok((states, dictionary))
}

fn biome_dictionary(
    snapshots: &[(Vec<BlockStateId>, Vec<u16>)],
) -> Result<(Vec<String>, FxHashMap<u16, u32>)> {
    let mut by_name = BTreeMap::<String, u16>::new();
    for biome_id in snapshots
        .iter()
        .flat_map(|(_, biomes)| biomes.iter())
        .copied()
    {
        let biome = REGISTRY
            .biomes
            .by_id(usize::from(biome_id))
            .with_context(|| format!("unknown biome id {biome_id}"))?;
        let name = biome.key.to_string();
        if let Some(previous) = by_name.insert(name, biome_id) {
            ensure!(
                previous == biome_id,
                "multiple Steel biome ids have one key"
            );
        }
    }

    let mut dictionary = FxHashMap::default();
    let mut biomes = Vec::with_capacity(by_name.len());
    for (index, (name, biome_id)) in by_name.into_iter().enumerate() {
        dictionary.insert(biome_id, u32::try_from(index)?);
        biomes.push(name);
    }
    Ok((biomes, dictionary))
}

fn local_palette(global_indices: &[u32]) -> Result<PackedPalette> {
    let entries = global_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ensure!(!entries.is_empty(), "palette cannot be empty");
    let local_by_global = entries
        .iter()
        .enumerate()
        .map(|(local, global)| Ok((*global, u32::try_from(local)?)))
        .collect::<Result<FxHashMap<_, _>>>()?;
    let local_indices = global_indices
        .iter()
        .map(|global| {
            local_by_global
                .get(global)
                .copied()
                .context("palette lookup failed")
        })
        .collect::<Result<Vec<_>>>()?;
    let bits_per_entry = bits_for_len(entries.len());
    Ok(PackedPalette {
        entries,
        bits_per_entry,
        data: pack_lsb(&local_indices, bits_per_entry)?,
    })
}

const fn bits_for_len(len: usize) -> u32 {
    if len <= 1 { 0 } else { (len - 1).bit_width() }
}

/// Packs unsigned palette indices contiguously with the least-significant bit first.
pub fn pack_lsb(values: &[u32], bits_per_entry: u32) -> Result<Vec<u8>> {
    ensure!(bits_per_entry <= 32, "bits per entry exceeds u32 width");
    if bits_per_entry == 0 {
        ensure!(
            values.iter().all(|value| *value == 0),
            "zero-width palette has a nonzero index"
        );
        return Ok(Vec::new());
    }
    let max = u64::from(u32::MAX >> (32 - bits_per_entry));
    let bit_len = values
        .len()
        .checked_mul(usize::try_from(bits_per_entry)?)
        .context("packed palette length overflow")?;
    let mut output = vec![0_u8; bit_len.div_ceil(8)];
    for (entry_index, &value) in values.iter().enumerate() {
        ensure!(
            u64::from(value) <= max,
            "palette index exceeds declared width"
        );
        let start_bit = entry_index * usize::try_from(bits_per_entry)?;
        for bit in 0..bits_per_entry {
            if value & (1_u32 << bit) != 0 {
                let output_bit = start_bit + usize::try_from(bit)?;
                output[output_bit / 8] |= 1_u8 << (output_bit % 8);
            }
        }
    }
    Ok(output)
}

fn snapshot_heightmaps(chunk: &Chunk) -> Result<Vec<Heightmap>> {
    let mut derived = ChunkHeightmaps::empty();
    derived.prime_from_sections(
        SteelHeightmapType::worldgen_types(),
        chunk.min_y(),
        chunk.height(),
        &chunk.sections().sections,
    );
    [
        SteelHeightmapType::WorldSurfaceWg,
        SteelHeightmapType::OceanFloorWg,
    ]
    .into_iter()
    .map(|map_type| {
        let raw = chunk
            .heightmap_raw_data(map_type)
            .with_context(|| format!("NOISE chunk is missing {map_type:?}"))?;
        let derived_raw = derived
            .get(map_type)
            .context("derived NOISE heightmap missing")?
            .raw_data();
        if raw.as_ref() != derived_raw {
            let (index, (&stored, &recomputed)) = raw
                .iter()
                .zip(derived_raw.iter())
                .enumerate()
                .find(|(_, (stored, recomputed))| stored != recomputed)
                .context("heightmap mismatch had no differing column")?;
            return Err(anyhow!(
                "NOISE {map_type:?} does not match block sections at ({}, {}): stored={}, recomputed={}",
                index % 16,
                index / 16,
                stored,
                recomputed
            ));
        }
        Ok(Heightmap {
            r#type: heightmap_type_to_proto(map_type) as i32,
            first_available_relative_to_min_y: raw.iter().copied().map(u32::from).collect(),
        })
    })
    .collect()
}

const fn heightmap_type_to_proto(map_type: SteelHeightmapType) -> v1::HeightmapType {
    match map_type {
        SteelHeightmapType::WorldSurface => v1::HeightmapType::HeightmapWorldSurface,
        SteelHeightmapType::MotionBlocking => v1::HeightmapType::HeightmapMotionBlocking,
        SteelHeightmapType::MotionBlockingNoLeaves => {
            v1::HeightmapType::HeightmapMotionBlockingNoLeaves
        }
        SteelHeightmapType::OceanFloor => v1::HeightmapType::HeightmapOceanFloor,
        SteelHeightmapType::WorldSurfaceWg => v1::HeightmapType::HeightmapWorldSurfaceWg,
        SteelHeightmapType::OceanFloorWg => v1::HeightmapType::HeightmapOceanFloorWg,
    }
}

/// Validates structural and canonical bounds for a V1 artifact.
pub fn validate_artifact(artifact: &ChunkArtifactV1) -> Result<()> {
    ensure!(
        artifact.artifact_version == ARTIFACT_VERSION,
        "unsupported artifact version"
    );
    ensure!(
        artifact.height > 0 && artifact.height <= 4096,
        "invalid artifact height"
    );
    ensure!(
        artifact.height.is_multiple_of(16) && artifact.min_y.rem_euclid(16) == 0,
        "unaligned artifact range"
    );
    ensure!(
        artifact.canonical_request_sha256.len() == 32,
        "invalid request digest length"
    );
    ensure!(
        artifact.generator_sha256.len() == 32,
        "invalid generator digest length"
    );
    ensure!(
        artifact.registry_sha256.len() == 32,
        "invalid registry digest length"
    );
    ensure!(
        artifact.minecraft_version.len() <= 32
            && !artifact.minecraft_version.is_empty()
            && artifact.minecraft_version.is_ascii(),
        "invalid Minecraft version"
    );
    validate_identifier(&artifact.dimension_key, "dimension key")?;
    ensure!(
        artifact.completed_stages == [Stage::Biomes as i32, Stage::Noise as i32],
        "NOISE artifact must contain exactly BIOMES then NOISE"
    );
    validate_dictionaries(artifact)?;
    ensure!(
        artifact.sections.len() == artifact.height as usize / 16,
        "wrong section count"
    );
    validate_heightmaps(artifact)?;
    let mut used_block_states = BTreeSet::new();
    let mut used_biomes = BTreeSet::new();
    for (index, section) in artifact.sections.iter().enumerate() {
        ensure!(
            section.section_y == artifact.min_y.div_euclid(16) + i32::try_from(index)?,
            "noncontiguous sections"
        );
        used_block_states.extend(validate_palette(
            section
                .block_states
                .as_ref()
                .context("missing block palette")?,
            4096,
            artifact.block_states.len(),
        )?);
        used_biomes.extend(validate_palette(
            section.biomes.as_ref().context("missing biome palette")?,
            64,
            artifact.biomes.len(),
        )?);
    }
    ensure!(
        used_block_states.len() == artifact.block_states.len(),
        "block-state dictionary contains an unused entry"
    );
    ensure!(
        used_biomes.len() == artifact.biomes.len(),
        "biome dictionary contains an unused entry"
    );
    validate_postprocessing(artifact)?;
    Ok(())
}

fn validate_heightmaps(artifact: &ChunkArtifactV1) -> Result<()> {
    let heightmap_types = artifact
        .heightmaps
        .iter()
        .map(|map| map.r#type)
        .collect::<Vec<_>>();
    ensure!(
        heightmap_types
            == [
                v1::HeightmapType::HeightmapWorldSurfaceWg as i32,
                v1::HeightmapType::HeightmapOceanFloorWg as i32,
            ],
        "NOISE artifact must contain the two world-generation heightmaps in canonical order"
    );
    ensure!(
        artifact.heightmaps.iter().all(|map| {
            map.first_available_relative_to_min_y.len() == 256
                && map
                    .first_available_relative_to_min_y
                    .iter()
                    .all(|&height| height <= artifact.height)
        }),
        "invalid heightmap data"
    );
    Ok(())
}

fn validate_postprocessing(artifact: &ChunkArtifactV1) -> Result<()> {
    ensure!(
        artifact.postprocessing.len() == artifact.sections.len(),
        "wrong post-processing section count"
    );
    for (index, section) in artifact.postprocessing.iter().enumerate() {
        ensure!(
            section.section_y == artifact.min_y.div_euclid(16) + i32::try_from(index)?,
            "noncontiguous post-processing sections"
        );
        ensure!(
            section.packed_offsets.len() <= 4096
                && section
                    .packed_offsets
                    .iter()
                    .all(|offset| *offset <= 0x0fff),
            "invalid post-processing offset"
        );
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    let Some((namespace, path)) = value.split_once(':') else {
        return Err(anyhow!("{label} is not explicitly namespaced"));
    };
    ensure!(
        value.len() <= 256
            && !namespace.is_empty()
            && !path.is_empty()
            && Identifier::validate(namespace, path),
        "invalid {label}"
    );
    Ok(())
}

fn validate_dictionaries(artifact: &ChunkArtifactV1) -> Result<()> {
    ensure!(
        !artifact.block_states.is_empty()
            && artifact.block_states.len() <= MAX_BLOCK_STATE_DICTIONARY,
        "invalid block-state dictionary size"
    );
    for state in &artifact.block_states {
        validate_identifier(&state.name, "block-state name")?;
        ensure!(
            state
                .properties
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name),
            "block-state properties are not strictly sorted by name"
        );
        for property in &state.properties {
            ensure!(
                !property.name.is_empty()
                    && property.name.len() <= 64
                    && property.name.chars().all(Identifier::valid_char),
                "invalid block-state property name"
            );
            ensure!(
                !property.value.is_empty()
                    && property.value.len() <= 128
                    && property.value.chars().all(Identifier::valid_char),
                "invalid block-state property value"
            );
        }
    }
    ensure!(
        artifact
            .block_states
            .windows(2)
            .all(|pair| canonical_block_state_less(&pair[0], &pair[1])),
        "block-state dictionary is not strictly sorted"
    );
    ensure!(
        !artifact.biomes.is_empty() && artifact.biomes.len() <= MAX_BIOME_DICTIONARY,
        "invalid biome dictionary size"
    );
    for biome in &artifact.biomes {
        validate_identifier(biome, "biome name")?;
    }
    ensure!(
        artifact.biomes.windows(2).all(|pair| pair[0] < pair[1]),
        "biome dictionary is not strictly sorted"
    );
    Ok(())
}

fn canonical_block_state_less(left: &BlockState, right: &BlockState) -> bool {
    left.name
        .cmp(&right.name)
        .then_with(|| {
            left.properties
                .iter()
                .map(|property| (&property.name, &property.value))
                .cmp(
                    right
                        .properties
                        .iter()
                        .map(|property| (&property.name, &property.value)),
                )
        })
        .is_lt()
}

fn validate_palette(
    palette: &PackedPalette,
    volume: usize,
    dictionary_len: usize,
) -> Result<BTreeSet<u32>> {
    ensure!(!palette.entries.is_empty(), "empty local palette");
    ensure!(
        palette.entries.windows(2).all(|pair| pair[0] < pair[1]),
        "local palette is not strictly sorted"
    );
    ensure!(
        palette
            .entries
            .iter()
            .all(|entry| (*entry as usize) < dictionary_len),
        "dictionary index out of range"
    );
    let expected_bits = bits_for_len(palette.entries.len());
    ensure!(
        palette.bits_per_entry == expected_bits,
        "noncanonical palette width"
    );
    let total_bits = volume * expected_bits as usize;
    ensure!(
        palette.data.len() == total_bits.div_ceil(8),
        "wrong packed palette length"
    );
    let trailing_bits = total_bits % 8;
    if trailing_bits != 0 {
        let valid_mask = (1_u8 << trailing_bits) - 1;
        ensure!(
            palette
                .data
                .last()
                .is_some_and(|last| last & !valid_mask == 0),
            "packed palette has nonzero padding bits"
        );
    }
    let mut used_local_entries = BTreeSet::new();
    for index in 0..volume {
        let mut local_index = 0_u32;
        let start_bit = index * expected_bits as usize;
        for bit in 0..expected_bits {
            let packed_bit = start_bit + bit as usize;
            if palette.data[packed_bit / 8] & (1 << (packed_bit % 8)) != 0 {
                local_index |= 1 << bit;
            }
        }
        ensure!(
            (local_index as usize) < palette.entries.len(),
            "packed local palette index out of range"
        );
        used_local_entries.insert(local_index);
    }
    ensure!(
        used_local_entries.len() == palette.entries.len(),
        "local palette contains an unused entry"
    );
    Ok(used_local_entries
        .into_iter()
        .map(|local| palette.entries[local as usize])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_artifact() -> ChunkArtifactV1 {
        let homogeneous = PackedPalette {
            entries: vec![0],
            bits_per_entry: 0,
            data: Vec::new(),
        };
        ChunkArtifactV1 {
            artifact_version: ARTIFACT_VERSION,
            minecraft_version: "26.2".to_owned(),
            canonical_request_sha256: vec![0; 32],
            generator_sha256: vec![1; 32],
            registry_sha256: vec![2; 32],
            dimension_key: "minecraft:overworld".to_owned(),
            seed: 0,
            chunk_x: 0,
            chunk_z: 0,
            min_y: 0,
            height: 16,
            completed_stages: vec![Stage::Biomes as i32, Stage::Noise as i32],
            block_states: vec![BlockState {
                name: "minecraft:air".to_owned(),
                properties: Vec::new(),
            }],
            biomes: vec!["minecraft:plains".to_owned()],
            sections: vec![ChunkSection {
                section_y: 0,
                block_states: Some(homogeneous.clone()),
                biomes: Some(homogeneous),
            }],
            heightmaps: vec![
                Heightmap {
                    r#type: v1::HeightmapType::HeightmapWorldSurfaceWg as i32,
                    first_available_relative_to_min_y: vec![0; 256],
                },
                Heightmap {
                    r#type: v1::HeightmapType::HeightmapOceanFloorWg as i32,
                    first_available_relative_to_min_y: vec![0; 256],
                },
            ],
            postprocessing: vec![PostProcessingSection {
                section_y: 0,
                packed_offsets: Vec::new(),
            }],
        }
    }

    #[test]
    fn minimal_canonical_artifact_is_valid() {
        validate_artifact(&minimal_artifact()).expect("minimal artifact should be valid");
    }

    #[test]
    fn nonzero_packed_padding_bits_are_rejected() {
        let palette = PackedPalette {
            entries: vec![0, 1],
            bits_per_entry: 1,
            data: vec![0b1111_1110],
        };
        let error = validate_palette(&palette, 1, 2).expect_err("padding must be canonical");
        assert!(error.to_string().contains("padding"));
    }

    #[test]
    fn malformed_packed_palette_index_is_rejected() {
        let mut artifact = minimal_artifact();
        artifact.block_states.extend([
            BlockState {
                name: "minecraft:dirt".to_owned(),
                properties: Vec::new(),
            },
            BlockState {
                name: "minecraft:stone".to_owned(),
                properties: Vec::new(),
            },
        ]);
        artifact
            .block_states
            .sort_by(|left, right| left.name.cmp(&right.name));
        artifact.sections[0].block_states = Some(PackedPalette {
            entries: vec![0, 1, 2],
            bits_per_entry: 2,
            data: {
                let mut data = vec![0; 1024];
                data[0] = 3;
                data
            },
        });
        let error = validate_artifact(&artifact).expect_err("local index 3 must be rejected");
        assert!(error.to_string().contains("local palette index"));
    }

    #[test]
    fn noncanonical_dictionary_is_rejected() {
        let mut artifact = minimal_artifact();
        artifact.biomes.push("minecraft:badlands".to_owned());
        let error = validate_artifact(&artifact).expect_err("unsorted dictionary must be rejected");
        assert!(error.to_string().contains("biome dictionary"));
    }

    #[test]
    fn unused_global_dictionary_entry_is_rejected() {
        let mut artifact = minimal_artifact();
        artifact.block_states.push(BlockState {
            name: "minecraft:stone".to_owned(),
            properties: Vec::new(),
        });
        let error = validate_artifact(&artifact).expect_err("unused global state must be rejected");
        assert!(error.to_string().contains("unused entry"));
    }

    #[test]
    fn unused_local_palette_entry_is_rejected() {
        let mut artifact = minimal_artifact();
        artifact.block_states.push(BlockState {
            name: "minecraft:stone".to_owned(),
            properties: Vec::new(),
        });
        artifact.sections[0].block_states = Some(PackedPalette {
            entries: vec![0, 1],
            bits_per_entry: 1,
            data: vec![0; 512],
        });
        let error = validate_artifact(&artifact).expect_err("unused local state must be rejected");
        assert!(error.to_string().contains("local palette"));
    }

    #[test]
    fn out_of_range_postprocessing_offset_is_rejected() {
        let mut artifact = minimal_artifact();
        artifact.postprocessing[0].packed_offsets.push(0x1000);
        let error = validate_artifact(&artifact).expect_err("13-bit offset must be rejected");
        assert!(error.to_string().contains("post-processing offset"));
    }

    #[test]
    fn documented_cross_byte_bitpack_vector() {
        assert_eq!(
            pack_lsb(&[0, 1, 2, 3, 4, 5, 6, 7, 0], 3).expect("documented bitpack vector is valid"),
            [0x88, 0xC6, 0xFA, 0x00]
        );
    }

    #[test]
    fn homogeneous_palette_has_no_data() {
        let palette = local_palette(&[7; 4096]).expect("homogeneous palette is valid");
        assert_eq!(palette.entries, [7]);
        assert_eq!(palette.bits_per_entry, 0);
        assert!(palette.data.is_empty());
    }
}
