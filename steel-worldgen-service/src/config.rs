//! Environment-backed worker configuration.

use std::{env, net::SocketAddr, path::PathBuf, str::FromStr as _};

use anyhow::{Context as _, Result, bail, ensure};
use steel_utils::Identifier;

/// Exact Minecraft version implemented by this worker build.
pub const MINECRAFT_VERSION: &str = "26.2";

/// Validated fixed-profile worker configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// Listener address; loopback by default.
    pub bind: SocketAddr,
    /// Operator-defined immutable profile name.
    pub profile_id: String,
    /// Loaded dimension key presented on the wire.
    pub dimension_key: Identifier,
    /// Built-in Steel generator identifier.
    pub generator_id: Identifier,
    /// Pinned world seed.
    pub seed: i64,
    /// Maximum admitted concurrent generation jobs.
    pub max_in_flight: usize,
    /// Maximum admitted concurrent generation jobs from one network peer.
    pub max_in_flight_per_peer: usize,
    /// Rayon world-generation worker count.
    pub generation_threads: usize,
    /// Server-side request deadline in milliseconds.
    pub request_timeout_ms: u64,
    /// Maximum number of complete artifacts retained in memory.
    pub max_cache_entries: usize,
    /// Maximum total encoded artifact bytes retained in memory.
    pub max_cache_bytes: usize,
    /// Public no-charge location for the exact corresponding source.
    pub corresponding_source_url: String,
    /// PEM server certificate chain for direct mutual TLS.
    pub tls_certificate: Option<PathBuf>,
    /// PEM server private key for direct mutual TLS.
    pub tls_private_key: Option<PathBuf>,
    /// PEM CA roots used to authenticate worker clients.
    pub tls_client_ca: Option<PathBuf>,
}

impl Config {
    /// Parses and validates `STEEL_WORLDGEN_*` environment variables.
    #[expect(
        clippy::too_many_lines,
        reason = "keeping all startup-only environment validation in one linear parser is easier to audit"
    )]
    pub fn from_env() -> Result<Self> {
        let bind: SocketAddr = env_or("STEEL_WORLDGEN_BIND", "127.0.0.1:50051")?
            .parse()
            .context("STEEL_WORLDGEN_BIND must be a socket address")?;
        let profile_id = env_or("STEEL_WORLDGEN_PROFILE_ID", "default")?;
        ensure!(
            !profile_id.is_empty()
                && profile_id.len() <= 128
                && profile_id.is_ascii()
                && !profile_id.bytes().any(|byte| byte.is_ascii_control()),
            "profile id must contain 1..=128 printable ASCII bytes"
        );
        let generator_id = parse_identifier(
            "STEEL_WORLDGEN_GENERATOR",
            &env_or("STEEL_WORLDGEN_GENERATOR", "minecraft:overworld")?,
        )?;
        let dimension_key = parse_identifier(
            "STEEL_WORLDGEN_DIMENSION",
            &env_or("STEEL_WORLDGEN_DIMENSION", &generator_id.to_string())?,
        )?;
        let seed = env::var("STEEL_WORLDGEN_SEED")
            .context("STEEL_WORLDGEN_SEED is required")?
            .parse()
            .context("STEEL_WORLDGEN_SEED must be an i64")?;
        let generation_threads = parse_usize("STEEL_WORLDGEN_THREADS", 1)?;
        ensure!(
            generation_threads == 1,
            "headless generation currently requires STEEL_WORLDGEN_THREADS=1 for deterministic output; scale with worker processes"
        );
        let max_in_flight = parse_usize("STEEL_WORLDGEN_MAX_IN_FLIGHT", generation_threads)?;
        ensure!(
            max_in_flight > 0 && max_in_flight <= 4096,
            "max in flight must be in 1..=4096"
        );
        let max_in_flight_per_peer =
            parse_usize("STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER", max_in_flight)?;
        ensure!(
            max_in_flight_per_peer > 0 && max_in_flight_per_peer <= max_in_flight,
            "max in flight per peer must be in 1..=STEEL_WORLDGEN_MAX_IN_FLIGHT"
        );
        let request_timeout_ms = parse_u64("STEEL_WORLDGEN_REQUEST_TIMEOUT_MS", 30_000)?;
        ensure!(
            (1..=600_000).contains(&request_timeout_ms),
            "request timeout must be in 1..=600000 ms"
        );
        let max_cache_entries = parse_usize("STEEL_WORLDGEN_MAX_CACHE_ENTRIES", 1024)?;
        ensure!(
            max_cache_entries <= 1_000_000,
            "max cache entries must not exceed 1000000"
        );
        let max_cache_bytes = parse_usize("STEEL_WORLDGEN_MAX_CACHE_BYTES", 256 * 1024 * 1024)?;
        ensure!(
            max_cache_bytes <= 64_usize.saturating_mul(1024 * 1024 * 1024),
            "max cache bytes must not exceed 64 GiB"
        );
        let source_url_was_configured = env::var_os("STEEL_WORLDGEN_SOURCE_URL").is_some();
        let corresponding_source_url = env_or(
            "STEEL_WORLDGEN_SOURCE_URL",
            "https://github.com/Steel-Foundation/SteelMC",
        )?;
        ensure!(
            corresponding_source_url.len() <= 2048
                && corresponding_source_url.is_ascii()
                && !corresponding_source_url
                    .bytes()
                    .any(|byte| byte.is_ascii_control())
                && (corresponding_source_url.starts_with("https://")
                    || corresponding_source_url.starts_with("http://")),
            "STEEL_WORLDGEN_SOURCE_URL must be a printable HTTP(S) URL no longer than 2048 bytes"
        );
        let tls_certificate = optional_path("STEEL_WORLDGEN_TLS_CERT")?;
        let tls_private_key = optional_path("STEEL_WORLDGEN_TLS_KEY")?;
        let tls_client_ca = optional_path("STEEL_WORLDGEN_TLS_CLIENT_CA")?;
        let configured_tls_files = [
            tls_certificate.is_some(),
            tls_private_key.is_some(),
            tls_client_ca.is_some(),
        ]
        .into_iter()
        .filter(|configured| *configured)
        .count();
        ensure!(
            configured_tls_files == 0 || configured_tls_files == 3,
            "TLS requires STEEL_WORLDGEN_TLS_CERT, STEEL_WORLDGEN_TLS_KEY, and STEEL_WORLDGEN_TLS_CLIENT_CA together"
        );
        let allow_insecure_remote = parse_bool("STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE", false)?;
        ensure!(
            configured_tls_files == 3 || bind.ip().is_loopback() || allow_insecure_remote,
            "plaintext workers may only bind loopback unless STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE=true is explicitly set"
        );
        ensure!(
            bind.ip().is_loopback() || source_url_was_configured,
            "non-loopback workers require STEEL_WORLDGEN_SOURCE_URL for the AGPL corresponding-source offer"
        );
        Ok(Self {
            bind,
            profile_id,
            dimension_key,
            generator_id,
            seed,
            max_in_flight,
            max_in_flight_per_peer,
            generation_threads,
            request_timeout_ms,
            max_cache_entries,
            max_cache_bytes,
            corresponding_source_url,
            tls_certificate,
            tls_private_key,
            tls_client_ca,
        })
    }
}

fn env_or(name: &str, default: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(error) => bail!("failed to read {name}: {error}"),
    }
}

fn optional_path(name: &str) -> Result<Option<PathBuf>> {
    match env::var(name) {
        Ok(value) => {
            ensure!(!value.is_empty(), "{name} must not be empty");
            Ok(Some(PathBuf::from(value)))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => bail!("failed to read {name}: {error}"),
    }
}

fn parse_bool(name: &str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(value) if value == "true" => Ok(true),
        Ok(value) if value == "false" => Ok(false),
        Ok(_) => bail!("{name} must be true or false"),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => bail!("failed to read {name}: {error}"),
    }
}

fn parse_identifier(name: &str, value: &str) -> Result<Identifier> {
    Identifier::from_str(value).map_err(|error| anyhow::anyhow!("{name} is invalid: {error}"))
}

fn parse_usize(name: &str, default: usize) -> Result<usize> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be an unsigned integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => bail!("failed to read {name}: {error}"),
    }
}

fn parse_u64(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be an unsigned integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => bail!("failed to read {name}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::{env, process::Command};

    use super::Config;

    const CHILD_MARKER: &str = "STEEL_CONFIG_TEST_CHILD";
    const EXPECTED_ERROR: &str = "STEEL_CONFIG_TEST_EXPECT_ERROR";

    #[test]
    fn from_env_child() {
        if env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        let result = Config::from_env();
        if let Ok(expected) = env::var(EXPECTED_ERROR) {
            let error = result.expect_err("configuration unexpectedly succeeded");
            assert!(
                format!("{error:#}").contains(&expected),
                "configuration error did not contain {expected:?}: {error:#}"
            );
        } else {
            result.expect("configuration unexpectedly failed");
        }
    }

    fn run_case(name: &str, values: &[(&str, &str)], expected_error: Option<&str>) {
        let executable = env::current_exe().expect("test executable should have a path");
        let mut command = Command::new(executable);
        command
            .arg("--exact")
            .arg("config::tests::from_env_child")
            .arg("--nocapture")
            .env_clear()
            .env(CHILD_MARKER, "true")
            .env("STEEL_WORLDGEN_SEED", "0");
        for &(key, value) in values {
            command.env(key, value);
        }
        if let Some(expected) = expected_error {
            command.env(EXPECTED_ERROR, expected);
        }
        let output = command.output().expect("configuration child should run");
        assert!(
            output.status.success(),
            "configuration case {name:?} failed:
stdout:
{}
stderr:
{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn from_env_enforces_remote_security_policy() {
        run_case("loopback plaintext", &[], None);
        run_case(
            "remote plaintext refused",
            &[
                ("STEEL_WORLDGEN_BIND", "0.0.0.0:50051"),
                ("STEEL_WORLDGEN_SOURCE_URL", "https://example.test/source"),
            ],
            Some("plaintext workers may only bind loopback"),
        );
        run_case(
            "remote plaintext needs source offer",
            &[
                ("STEEL_WORLDGEN_BIND", "0.0.0.0:50051"),
                ("STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE", "true"),
            ],
            Some("non-loopback workers require STEEL_WORLDGEN_SOURCE_URL"),
        );
        run_case(
            "explicit remote plaintext",
            &[
                ("STEEL_WORLDGEN_BIND", "0.0.0.0:50051"),
                ("STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE", "true"),
                ("STEEL_WORLDGEN_SOURCE_URL", "https://example.test/source"),
            ],
            None,
        );
        for (name, tls) in [
            (
                "certificate only",
                vec![("STEEL_WORLDGEN_TLS_CERT", "/cert")],
            ),
            ("key only", vec![("STEEL_WORLDGEN_TLS_KEY", "/key")]),
            ("ca only", vec![("STEEL_WORLDGEN_TLS_CLIENT_CA", "/ca")]),
            (
                "certificate and key",
                vec![
                    ("STEEL_WORLDGEN_TLS_CERT", "/cert"),
                    ("STEEL_WORLDGEN_TLS_KEY", "/key"),
                ],
            ),
            (
                "certificate and ca",
                vec![
                    ("STEEL_WORLDGEN_TLS_CERT", "/cert"),
                    ("STEEL_WORLDGEN_TLS_CLIENT_CA", "/ca"),
                ],
            ),
            (
                "key and ca",
                vec![
                    ("STEEL_WORLDGEN_TLS_KEY", "/key"),
                    ("STEEL_WORLDGEN_TLS_CLIENT_CA", "/ca"),
                ],
            ),
        ] {
            run_case(name, &tls, Some("TLS requires"));
        }
        run_case(
            "complete remote TLS",
            &[
                ("STEEL_WORLDGEN_BIND", "0.0.0.0:50051"),
                ("STEEL_WORLDGEN_SOURCE_URL", "https://example.test/source"),
                ("STEEL_WORLDGEN_TLS_CERT", "/cert"),
                ("STEEL_WORLDGEN_TLS_KEY", "/key"),
                ("STEEL_WORLDGEN_TLS_CLIENT_CA", "/ca"),
            ],
            None,
        );
        run_case(
            "invalid insecure flag",
            &[("STEEL_WORLDGEN_ALLOW_INSECURE_REMOTE", "yes")],
            Some("must be true or false"),
        );
        for (name, source) in [
            ("non-http source", "ftp://example.test/source"),
            (
                "source control character",
                "https://example.test/source
",
            ),
        ] {
            run_case(
                name,
                &[("STEEL_WORLDGEN_SOURCE_URL", source)],
                Some("must be a printable HTTP(S) URL"),
            );
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "table-driven subprocess cases keep environment isolation checks together"
    )]
    fn from_env_enforces_resource_and_string_bounds() {
        for (name, values) in [
            (
                "minimum bounds",
                vec![
                    ("STEEL_WORLDGEN_THREADS", "1"),
                    ("STEEL_WORLDGEN_MAX_IN_FLIGHT", "1"),
                    ("STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER", "1"),
                    ("STEEL_WORLDGEN_REQUEST_TIMEOUT_MS", "1"),
                    ("STEEL_WORLDGEN_MAX_CACHE_ENTRIES", "0"),
                    ("STEEL_WORLDGEN_MAX_CACHE_BYTES", "0"),
                    ("STEEL_WORLDGEN_PROFILE_ID", "x"),
                ],
            ),
            (
                "maximum bounds",
                vec![
                    ("STEEL_WORLDGEN_THREADS", "1"),
                    ("STEEL_WORLDGEN_MAX_IN_FLIGHT", "4096"),
                    ("STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER", "4096"),
                    ("STEEL_WORLDGEN_REQUEST_TIMEOUT_MS", "600000"),
                    ("STEEL_WORLDGEN_MAX_CACHE_ENTRIES", "1000000"),
                    ("STEEL_WORLDGEN_MAX_CACHE_BYTES", "68719476736"),
                    (
                        "STEEL_WORLDGEN_PROFILE_ID",
                        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                    ),
                ],
            ),
        ] {
            run_case(name, &values, None);
        }
        for (name, values, error) in [
            (
                "zero threads",
                vec![("STEEL_WORLDGEN_THREADS", "0")],
                "STEEL_WORLDGEN_THREADS=1",
            ),
            (
                "multiple threads",
                vec![("STEEL_WORLDGEN_THREADS", "2")],
                "STEEL_WORLDGEN_THREADS=1",
            ),
            (
                "zero global admission",
                vec![("STEEL_WORLDGEN_MAX_IN_FLIGHT", "0")],
                "max in flight must be",
            ),
            (
                "global admission too large",
                vec![("STEEL_WORLDGEN_MAX_IN_FLIGHT", "4097")],
                "max in flight must be",
            ),
            (
                "zero peer admission",
                vec![("STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER", "0")],
                "max in flight per peer",
            ),
            (
                "peer admission exceeds global",
                vec![
                    ("STEEL_WORLDGEN_MAX_IN_FLIGHT", "1"),
                    ("STEEL_WORLDGEN_MAX_IN_FLIGHT_PER_PEER", "2"),
                ],
                "max in flight per peer",
            ),
            (
                "zero timeout",
                vec![("STEEL_WORLDGEN_REQUEST_TIMEOUT_MS", "0")],
                "request timeout must be",
            ),
            (
                "timeout too large",
                vec![("STEEL_WORLDGEN_REQUEST_TIMEOUT_MS", "600001")],
                "request timeout must be",
            ),
            (
                "cache entries too large",
                vec![("STEEL_WORLDGEN_MAX_CACHE_ENTRIES", "1000001")],
                "max cache entries",
            ),
            (
                "cache bytes too large",
                vec![("STEEL_WORLDGEN_MAX_CACHE_BYTES", "68719476737")],
                "max cache bytes",
            ),
            (
                "empty profile",
                vec![("STEEL_WORLDGEN_PROFILE_ID", "")],
                "profile id must contain",
            ),
            (
                "long profile",
                vec![(
                    "STEEL_WORLDGEN_PROFILE_ID",
                    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                )],
                "profile id must contain",
            ),
            (
                "profile control character",
                vec![(
                    "STEEL_WORLDGEN_PROFILE_ID",
                    "bad
profile",
                )],
                "profile id must contain",
            ),
        ] {
            run_case(name, &values, Some(error));
        }
        run_case(
            "maximum source URL length",
            &[(
                "STEEL_WORLDGEN_SOURCE_URL",
                &format!("https://{}", "x".repeat(2040)),
            )],
            None,
        );
        run_case(
            "source URL too long",
            &[(
                "STEEL_WORLDGEN_SOURCE_URL",
                &format!("https://{}", "x".repeat(2041)),
            )],
            Some("no longer than 2048 bytes"),
        );
    }
}
