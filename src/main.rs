use anyhow::Result;
use crossbeam_channel::bounded;
use std::{
    net::SocketAddr,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};
use tracing::info;

mod capture;
mod config;
mod encoder;
mod metrics;
mod peer;
mod routes;
mod types;
mod webrtc;
mod writer;

use config::Config;
use types::{AppState, EncodedFrame, IceServers, RawFrame};

fn main() -> Result<()> {
    // rustls 0.23 requires a crypto provider before any TLS use.
    // webrtc-rs uses rustls internally for DTLS; install ring as the provider.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // ok() — harmless if already installed

    // Structured logging. Set RUST_LOG=debug for verbose output.
    // Suppress benign "agent is closed" warnings from webrtc-rs ICE cleanup.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,webrtc_ice::agent::agent_internal=error".into()
            }),
        )
        .init();

    let config = Config::from_env();
    config.log();

    // ── GST → encoder channel (bounded, back-pressure safe) ──────────────────
    let (raw_tx, raw_rx) = bounded::<RawFrame>(2);

    // ── encoder → N peer tasks (broadcast) ───────────────────────────────────
    // The initial `_` receiver keeps the channel alive when no peers are
    // connected so the encoder does not see a "no receivers" error on send.
    let (frame_tx, _keep_alive_rx) = tokio::sync::broadcast::channel::<EncodedFrame>(32);

    // ── OS thread 1: GStreamer capture with restart watchdog ──────────────────
    let raw_tx_gst = raw_tx.clone();
    let (dev, w, h, fps) = (config.camera_device, config.width, config.height, config.fps);
    std::thread::Builder::new()
        .name("gst-watchdog".into())
        .spawn(move || loop {
            info!("starting capture pipeline (device={dev} {w}x{h}@{fps})");
            capture::thread(raw_tx_gst.clone(), dev, w, h, fps);
            tracing::warn!("capture pipeline stopped — restarting in 2 s…");
            std::thread::sleep(Duration::from_secs(2));
        })?;

    // ── keyframe request channel: peer task → encoder ─────────────────────────
    // Bounded(4): several peers may join simultaneously; extra signals are safe
    // to coalesce — the encoder drains all of them in a single GstForceKeyUnit.
    let (keyframe_tx, keyframe_rx) = bounded::<()>(4);

    // ── OS thread 2: x264 encoder ─────────────────────────────────────────────
    let frame_tx_enc = frame_tx.clone();
    let (eb, et) = (config.encoder_bitrate, config.encoder_threads);
    std::thread::Builder::new()
        .name("encoder".into())
        .spawn(move || encoder::thread(raw_rx, frame_tx_enc, keyframe_rx, eb, et, w, h, fps))?;

    // ── WebRTC API (shared across all peer connections) ───────────────────────
    let webrtc_api = Arc::new(webrtc::build_api()?);

    // ── ICE servers ───────────────────────────────────────────────────────────
    let ice_servers = IceServers::from_env();
    info!("ICE STUN: {:?}", ice_servers.stun);
    if ice_servers.turn.is_empty() {
        info!("No TURN servers — set TURN_SERVERS for production deployments");
    } else {
        info!("ICE TURN: {} server(s)", ice_servers.turn.len());
    }

    // ── Prometheus metrics ────────────────────────────────────────────────────
    let prometheus_handle = metrics::init()?;

    // ── Application state ─────────────────────────────────────────────────────
    let state = AppState {
        frame_tx,
        peers: Arc::new(dashmap::DashMap::new()),
        peer_count: Arc::new(AtomicUsize::new(0)),
        webrtc_api,
        ice_servers,
        max_peers: config.max_peers,
        keyframe_tx,
        prometheus_handle,
    };

    // ── Tokio runtime: HTTP + WebRTC ──────────────────────────────────────────
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_cpus::get())
        .enable_all()
        .thread_name("tokio-worker")
        .build()?
        .block_on(async move {
            let shutdown_peers = Arc::clone(&state.peers);

            let app = routes::build_router(state, &config);

            let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
            info!("listening → http://localhost:{}", config.port);

            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::signal::ctrl_c().await.ok();
                    info!("Ctrl+C — shutting down");
                })
                .await?;

            // Collect peers into a Vec before awaiting close() so we hold no
            // DashMap shard read-guards while the on_peer_connection_state_change
            // callback tries to acquire a write guard (deadlock otherwise).
            info!("closing {} peer connection(s)…", shutdown_peers.len());
            let pcs: Vec<Arc<crate::webrtc::RTCPeerConnection>> = shutdown_peers
                .iter()
                .map(|e| Arc::clone(e.value()))
                .collect();
            drop(shutdown_peers);

            for pc in pcs {
                // 500 ms timeout: webrtc-rs close() can stall on ICE teardown.
                let _ = tokio::time::timeout(Duration::from_millis(500), pc.close()).await;
            }

            // Give background ICE teardown tasks a moment before the runtime drops.
            tokio::time::sleep(Duration::from_millis(200)).await;
            info!("shutdown complete");

            Ok::<_, anyhow::Error>(())
        })?;

    // On Windows, Ctrl+C re-signals the process after our handler returns,
    // producing exit code 0xc000013a. Exit explicitly with 0.
    std::process::exit(0);
}
