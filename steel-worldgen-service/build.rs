//! Protobuf compiler setup and reproducible build identity capture.

use std::{
    env,
    error::Error,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest as _, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut prost_config = prost_build::Config::new();
    prost_config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure().compile_with_config(
        prost_config,
        &["proto/steel/worldgen/v1/worldgen.proto"],
        &["proto"],
    )?;

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let workspace = manifest_dir
        .parent()
        .ok_or("service manifest has no workspace parent")?;
    let source_sha256 = source_tree_sha256(workspace)?;
    let rustc_id = command_output(env::var("RUSTC")?, &["--version", "--verbose"])?;
    let cargo_id = command_output(
        env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()),
        &["--version", "--verbose"],
    )?;
    let target = env::var("TARGET")?;
    let build_configuration = format!(
        "profile={};opt={};debug={};debug_assertions={};overflow_checks={};ub_checks={};panic={};relocation={};target_features={};rustflags={}",
        env::var("PROFILE")?,
        env::var("OPT_LEVEL")?,
        env::var("DEBUG")?,
        env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some(),
        env::var_os("CARGO_CFG_OVERFLOW_CHECKS").is_some(),
        env::var_os("CARGO_CFG_UB_CHECKS").is_some(),
        env::var("CARGO_CFG_PANIC").unwrap_or_default(),
        env::var("CARGO_CFG_RELOCATION_MODEL").unwrap_or_default(),
        env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default(),
        env::var("CARGO_ENCODED_RUSTFLAGS")
            .unwrap_or_default()
            .replace('\x1f', " | "),
    );
    let external_build_id = env::var("STEEL_WORLDGEN_BUILD_ID")
        .unwrap_or_else(|_| "local-content-addressed-build".to_owned());
    if external_build_id.is_empty()
        || external_build_id.len() > 256
        || !external_build_id.is_ascii()
        || external_build_id
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err("STEEL_WORLDGEN_BUILD_ID must contain 1..=256 printable ASCII bytes".into());
    }

    println!("cargo:rustc-env=STEEL_WORLDGEN_SOURCE_SHA256={source_sha256}");
    println!("cargo:rustc-env=STEEL_WORLDGEN_RUSTC_ID={rustc_id}");
    println!("cargo:rustc-env=STEEL_WORLDGEN_CARGO_ID={cargo_id}");
    println!("cargo:rustc-env=STEEL_WORLDGEN_TARGET={target}");
    println!("cargo:rustc-env=STEEL_WORLDGEN_BUILD_CONFIGURATION={build_configuration}");
    println!("cargo:rustc-env=STEEL_WORLDGEN_EXTERNAL_BUILD_ID={external_build_id}");
    println!("cargo:rerun-if-env-changed=STEEL_WORLDGEN_BUILD_ID");
    Ok(())
}

fn source_tree_sha256(workspace: &Path) -> Result<String, Box<dyn Error>> {
    let datapack_version_path =
        workspace.join("steel-utils/build_assets/builtin_datapacks/minecraft/.version");
    let datapack_version = fs::read_to_string(&datapack_version_path)?;
    let package_version = env::var("CARGO_PKG_VERSION")?;
    let expected_minecraft = package_version
        .rsplit_once("+mc")
        .map(|(_, minecraft)| minecraft)
        .ok_or("workspace package version has no +mc target")?;
    if datapack_version.trim() != expected_minecraft {
        return Err(format!(
            "builtin datapacks target {} but the service targets {expected_minecraft}",
            datapack_version.trim()
        )
        .into());
    }

    let roots = [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo",
        "rust-toolchain.toml",
        "steel/Cargo.toml",
        "steel/src",
        "steel-login/Cargo.toml",
        "steel-login/src",
        "steel-crypto/Cargo.toml",
        "steel-crypto/src",
        "steel-protocol/Cargo.toml",
        "steel-protocol/src",
        "steel-core/Cargo.toml",
        "steel-core/src",
        "steel-core/build",
        "steel-core/build_assets",
        "steel-macros/Cargo.toml",
        "steel-macros/src",
        "steel-math/Cargo.toml",
        "steel-math/src",
        "steel-registry/Cargo.toml",
        "steel-registry/src",
        "steel-registry/build",
        "steel-registry/build_assets",
        "steel-utils/Cargo.toml",
        "steel-utils/src",
        "steel-utils/build",
        "steel-utils/build_assets",
        "steel-worldgen/Cargo.toml",
        "steel-worldgen/src",
        "steel-worldgen/build",
        "steel-worldgen/build_assets",
        "steel-worldgen-service/Cargo.toml",
        "steel-worldgen-service/build.rs",
        "steel-worldgen-service/proto",
        "steel-worldgen-service/src",
    ];
    let mut files = Vec::new();
    for root in roots {
        let path = workspace.join(root);
        println!("cargo:rerun-if-changed={}", path.display());
        collect_files(&path, &mut files)?;
    }
    files.sort();

    let mut hash = Sha256::new();
    hash.update(b"steel-worldgen-source-v1");
    for path in files {
        let relative = path.strip_prefix(workspace)?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&path)?;
        hash.update((relative.len() as u64).to_be_bytes());
        hash.update(relative.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let mut encoded = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(&mut encoded, "{byte:02x}")?;
    }
    Ok(encoded)
}

fn collect_files(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        output.push(path.to_owned());
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        let name = child.file_name();
        if child.is_file() && name.is_some_and(|name| name == ".asset-extract.lock") {
            continue;
        }
        collect_files(&child, output)?;
    }
    Ok(())
}

fn command_output(command: String, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new(command).args(args).output()?;
    if !output.status.success() {
        return Err("failed to capture compiler identity".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | "))
}
