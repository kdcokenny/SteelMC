//! Steel remote world-generation worker executable.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use rustls::crypto::ring::default_provider;
use steel_core::bootstrap;
use steel_worldgen_service::{
    artifact::MAX_ARTIFACT_BYTES, config::Config, engine::Engine,
    proto::v1::world_gen_service_server::WorldGenServiceServer, server::Service,
};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::{
    fs::read,
    runtime::{Builder, Runtime},
    signal::ctrl_c,
};
use tokio_util::sync::CancellationToken;
use tonic::{
    codec::CompressionEncoding,
    transport::{Certificate, Identity, Server, ServerTlsConfig},
};
use tonic_health::server::health_reporter;
use tracing_subscriber::EnvFilter;

const CONTROL_PLANE_STREAM_HEADROOM: usize = 8;

fn main() -> Result<()> {
    let _ = default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = Config::from_env()?;
    bootstrap::init_globals().map_err(anyhow::Error::msg)?;
    let runtime = Arc::new(
        Builder::new_multi_thread()
            .enable_all()
            .thread_name("steel-worldgen-runtime")
            .build()
            .context("failed to create Tokio runtime")?,
    );
    let runtime_for_engine = Arc::clone(&runtime);
    runtime.block_on(async move { run(config, runtime_for_engine).await })
}

async fn run(config: Config, runtime: Arc<Runtime>) -> Result<()> {
    let engine = Arc::new(Engine::new(&config, runtime).await?);
    let bind = config.bind;
    let max_in_flight_per_peer = config.max_in_flight_per_peer;
    let tls = load_tls_config(&config).await?;
    let (health_reporter, health_service) = health_reporter();
    health_reporter
        .set_serving::<WorldGenServiceServer<Service>>()
        .await;
    let fatal_shutdown = CancellationToken::new();
    let service = Service::new_with_health(
        config,
        Arc::clone(&engine),
        Some(health_reporter.clone()),
        fatal_shutdown.clone(),
    );
    let grpc = WorldGenServiceServer::new(service)
        .accept_compressed(CompressionEncoding::Gzip)
        .send_compressed(CompressionEncoding::Gzip)
        .max_decoding_message_size(64 * 1024)
        .max_encoding_message_size(MAX_ARTIFACT_BYTES + 64 * 1024);

    let per_connection_streams = max_in_flight_per_peer + CONTROL_PLANE_STREAM_HEADROOM;
    let mut server = Server::builder()
        .concurrency_limit_per_connection(per_connection_streams)
        .max_concurrent_streams(Some(u32::try_from(per_connection_streams)?))
        .load_shed(true);
    if let Some(tls) = tls {
        server = server
            .tls_config(tls)
            .context("failed to configure worker mutual TLS")?;
    }
    tracing::info!(%bind, "Steel world-generation worker ready");
    let result = server
        .add_service(health_service)
        .add_service(grpc)
        .serve_with_shutdown(bind, shutdown_signal(fatal_shutdown))
        .await;
    engine.stop();
    result.context("gRPC server failed")
}

async fn load_tls_config(config: &Config) -> Result<Option<ServerTlsConfig>> {
    let Some(certificate_path) = config.tls_certificate.as_ref() else {
        return Ok(None);
    };
    let private_key_path = config
        .tls_private_key
        .as_ref()
        .context("TLS private key path disappeared after validation")?;
    let client_ca_path = config
        .tls_client_ca
        .as_ref()
        .context("TLS client CA path disappeared after validation")?;
    let certificate = read(certificate_path)
        .await
        .with_context(|| format!("failed to read {}", certificate_path.display()))?;
    let private_key = read(private_key_path)
        .await
        .with_context(|| format!("failed to read {}", private_key_path.display()))?;
    let client_ca = read(client_ca_path)
        .await
        .with_context(|| format!("failed to read {}", client_ca_path.display()))?;
    Ok(Some(
        ServerTlsConfig::new()
            .identity(Identity::from_pem(certificate, private_key))
            .client_ca_root(Certificate::from_pem(client_ca)),
    ))
}

async fn shutdown_signal(fatal_shutdown: CancellationToken) {
    #[cfg(unix)]
    {
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    () = fatal_shutdown.cancelled() => {
                        tracing::error!("fatal generation failure requested worker shutdown");
                    }
                    result = ctrl_c() => {
                        if let Err(error) = result {
                            tracing::error!(?error, "failed to listen for Ctrl-C");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                tracing::error!(?error, "failed to listen for SIGTERM");
                tokio::select! {
                    () = fatal_shutdown.cancelled() => {
                        tracing::error!("fatal generation failure requested worker shutdown");
                    }
                    result = ctrl_c() => {
                        if let Err(error) = result {
                            tracing::error!(?error, "failed to listen for Ctrl-C");
                        }
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    tokio::select! {
        () = fatal_shutdown.cancelled() => {
            tracing::error!("fatal generation failure requested worker shutdown");
        }
        result = ctrl_c() => {
            if let Err(error) = result {
                tracing::error!(?error, "failed to listen for Ctrl-C");
            }
        }
    }
}
