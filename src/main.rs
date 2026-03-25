use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver as CbReceiver, Sender as CbSender};
use dashmap::DashMap;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use serde::{Deserialize, Serialize};
use std::{
    env,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::sync::broadcast;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::ServeDir,
};
use tracing::{error, info, warn};
use uuid::Uuid;
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors,
        media_engine::{MediaEngine, MIME_TYPE_H264},
        APIBuilder,
    },
    ice_transport::{
        ice_candidate::RTCIceCandidateInit,
        ice_server::RTCIceServer,
    },
    interceptor::registry::Registry,
    media::Sample,
    peer_connection::{
        configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
        RTCPeerConnection,
    },
    rtp_transceiver::{
        rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType},
        rtp_transceiver_direction::RTCRtpTransceiverDirection, RTCRtpTransceiverInit,
    },
    track::track_local::{
        track_local_static_sample::TrackLocalStaticSample, TrackLocal,
    },
};

// ── types ─────────────────────────────────────────────────────────────────────

/// A raw I420 frame coming off the appsink callback
#[derive(Clone)]
struct RawFrame {
    data: Bytes,
    pts:  Duration,
}

/// An encoded H.264 NAL unit ready for RTP
#[derive(Clone)]
struct EncodedFrame {
    data:     Bytes,
    duration: Duration,
}

// ── shared app state (cheap to clone — all Arc inside) ───────────────────────
// ── ICE server configuration ──────────────────────────────────────────────────
/// Holds STUN/TURN server URLs for NAT traversal.
/// For local testing, only STUN is needed (loopback).
/// For production/remote clients, add TURN servers for NAT traversal.
#[derive(Clone)]
struct IceServers {
    stun:  Vec<String>,
    turn:  Vec<(String, String, String)>, // (urls, username, password)
}

impl Default for IceServers {
    fn default() -> Self {
        Self {
            // Google's public STUN server (for initial connectivity)
            stun: vec!["stun:stun.l.google.com:19302".to_owned()],
            // For production set TURN_SERVERS env var, e.g.:
            // TURN_SERVERS="turn:turnserver.example.com:3478|user|pass"
            turn: vec![],
        }
    }
}

impl IceServers {
    /// Load ICE servers from environment variables or use defaults.
    /// STUN_SERVERS: comma-separated list of STUN URLs
    /// TURN_SERVERS: semicolon-separated list of "url|user|pass" triplets
    ///   e.g. "turn:server.example.com:3478|alice|secret;turns:server2.example.com:5349|bob|s3cr3t"
    fn from_env() -> Self {
        let mut servers = IceServers::default();

        if let Ok(stun_list) = env::var("STUN_SERVERS") {
            servers.stun = stun_list
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect();
        }

        if let Ok(turn_list) = env::var("TURN_SERVERS") {
            // Format: "turn:host:port|user|pass;turn:host2:port2|user2|pass2"
            // Fields are separated by '|' so the URL (which contains ':') is preserved intact.
            servers.turn = turn_list
                .split(';')
                .filter_map(|s| {
                    let parts: Vec<&str> = s.trim().splitn(3, '|').collect();
                    if parts.len() == 3 {
                        Some((
                            parts[0].to_owned(),
                            parts[1].to_owned(),
                            parts[2].to_owned(),
                        ))
                    } else {
                        warn!("TURN_SERVERS entry {:?} ignored — expected 'url|user|pass'", s.trim());
                        None
                    }
                })
                .collect();
        }

        servers
    }
}

#[derive(Clone)]
struct AppState {
    /// broadcast channel — encoder → N peer tasks
    frame_tx:    broadcast::Sender<EncodedFrame>,
    /// peer table — uuid → PeerConnection (for cleanup)
    peers:       Arc<DashMap<String, Arc<RTCPeerConnection>>>,
    /// WebRTC API (reused across peers)
    webrtc_api:  Arc<webrtc::api::API>,
    /// ICE servers for NAT traversal (STUN/TURN)
    ice_servers: IceServers,
    /// Maximum simultaneous viewers (env: MAX_PEERS, default 50)
    max_peers:        usize,
    /// Trigger a keyframe from the encoder on the next frame (new peer joined)
    keyframe_tx:       CbSender<()>,
    /// Prometheus metrics handle — rendered at GET /metrics
    prometheus_handle: PrometheusHandle,
}

// ── signaling ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Signal {
    Offer     { sdp: String },
    Answer    { sdp: String },
    Candidate { candidate: RTCIceCandidateInit },
}

// ─────────────────────────────────────────────────────────────────────────────
// THREAD 1 — GStreamer capture (dedicated OS thread, never async)
// ─────────────────────────────────────────────────────────────────────────────

fn gst_thread(raw_tx: CbSender<RawFrame>) {
    if let Err(e) = run_gst_pipeline(raw_tx) {
        error!("GST thread died: {e:#}");
    }
}

fn run_gst_pipeline(raw_tx: CbSender<RawFrame>) -> Result<()> {
    gst::init()?;

    // Build pipeline string — uses mfvideosrc without specifying a device
    // which will use the first available camera
    let pipeline_str = concat!(
        "mfvideosrc ! ",
        "video/x-raw,format=NV12,width=1280,height=720,framerate=30/1 ",
        "! videoconvert ",
        "! video/x-raw,format=I420 ",
        "! appsink name=sink emit-signals=true max-buffers=2 drop=true sync=false",
    );

    let pipeline = gst::parse::launch(pipeline_str)?
        .downcast::<gst::Pipeline>()
        .unwrap();

    let sink = pipeline
        .by_name("sink")
        .context("appsink not found")?
        .downcast::<gst_app::AppSink>()
        .unwrap();

    // appsink callback — runs on the GStreamer streaming thread.
    // We do the absolute minimum here: copy bytes, send to channel, done.
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = match sink.pull_sample() {
                    Ok(s) => s,
                    Err(_) => return Err(gst::FlowError::Eos),
                };

                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map    = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let pts    = buffer
                    .pts()
                    .map(|t| Duration::from_nanos(t.nseconds()))
                    .unwrap_or_default();

                let frame = RawFrame {
                    data: Bytes::copy_from_slice(map.as_slice()),
                    pts,
                };

                // bounded(2): if encoder is behind, drop this frame silently
                // rather than blocking the GST thread
                match raw_tx.try_send(frame) {
                    Ok(_) => {}
                    Err(crossbeam_channel::TrySendError::Full(_)) => {
                        warn!("encoder busy — dropping frame");
                        metrics::counter!("stream_encoder_drops_total").increment(1);
                    }
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        return Err(gst::FlowError::Eos);
                    }
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    let bus = pipeline.bus().context("no bus")?;
    
    match pipeline.set_state(gst::State::Playing) {
        Ok(_) => info!("GStreamer pipeline playing"),
        Err(e) => {
            error!("Failed to set pipeline to Playing state: {e:?}");
            // Try to collect error messages from the bus
            for msg in bus.iter_timed(gst::ClockTime::from_seconds(1)) {
                if let gst::MessageView::Error(e) = msg.view() {
                    error!("GST error from bus: {} — {:?}", e.error(), e.debug());
                }
            }
            return Err(anyhow::anyhow!("Pipeline state change failed"));
        }
    }

    // Block this thread on the GStreamer bus
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(..) => {
                info!("GST: EOS");
                break;
            }
            MessageView::Error(e) => {
                error!("GST error: {} — {:?}", e.error(), e.debug());
                break;
            }
            MessageView::Warning(w) => {
                warn!("GST warning: {}", w.error());
            }
            MessageView::StateChanged(s) if msg.src().as_ref() == Some(&pipeline.upcast_ref::<gst::Object>()) => {
                info!(
                    "pipeline state: {:?} → {:?}",
                    s.old(),
                    s.current()
                );
            }
            _ => {}
        }
    }

    pipeline.set_state(gst::State::Null)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// THREAD 2 — Encoder (dedicated OS thread, CPU-bound x264)
// ─────────────────────────────────────────────────────────────────────────────

fn encoder_thread(
    raw_rx:      CbReceiver<RawFrame>,
    frame_tx:    broadcast::Sender<EncodedFrame>,
    keyframe_rx: CbReceiver<()>,
) {
    if let Err(e) = run_encoder(raw_rx, frame_tx, keyframe_rx) {
        error!("Encoder thread died: {e:#}");
    }
}

fn run_encoder(
    raw_rx:      CbReceiver<RawFrame>,
    frame_tx:    broadcast::Sender<EncodedFrame>,
    keyframe_rx: CbReceiver<()>,
) -> Result<()> {
    gst::init()?;

    // Dedicated encoder pipeline: appsrc → x264enc → appsink
    let enc_pipeline = gst::parse::launch(
        "appsrc name=src format=time is-live=true \
         ! video/x-raw,format=I420,width=1280,height=720,framerate=30/1 \
         ! x264enc tune=zerolatency bitrate=2000 key-int-max=60 threads=4 \
         ! video/x-h264,profile=baseline,stream-format=byte-stream \
         ! appsink name=enc_sink emit-signals=true max-buffers=4 drop=false sync=false",
    )?
    .downcast::<gst::Pipeline>()
    .unwrap();

    let src = enc_pipeline
        .by_name("src")
        .context("appsrc not found")?
        .downcast::<gst_app::AppSrc>()
        .unwrap();

    let enc_sink = enc_pipeline
        .by_name("enc_sink")
        .context("enc_sink not found")?
        .downcast::<gst_app::AppSink>()
        .unwrap();

    enc_pipeline.set_state(gst::State::Playing)?;
    info!("Encoder pipeline playing");

    // Spawn a thread to pull encoded frames from enc_sink and broadcast them.
    // The handle is kept so we can join it during shutdown.
    let frame_tx_clone = frame_tx.clone();
    let sink_handle = std::thread::Builder::new()
        .name("enc-sink".into())
        .spawn(move || {
            let mut last_pts   = Duration::ZERO;
            let mut fps_frames = 0u64;
            let mut fps_timer  = std::time::Instant::now();

            loop {
                match enc_sink.pull_sample() {
                    Ok(sample) => {
                        let Some(buffer) = sample.buffer() else { continue };
                        let Ok(map) = buffer.map_readable() else { continue };

                        let pts = buffer
                            .pts()
                            .map(|t| Duration::from_nanos(t.nseconds()))
                            .unwrap_or_default();

                        let duration = if pts > last_pts {
                            pts - last_pts
                        } else {
                            Duration::from_millis(33)
                        };
                        last_pts = pts;

                        let encoded = EncodedFrame {
                            data: Bytes::copy_from_slice(map.as_slice()),
                            duration,
                        };

                        // broadcast — if no receivers yet, that's fine
                        let _ = frame_tx_clone.send(encoded);

                        // ── item 14 & 15: metrics + periodic fps log ──────
                        metrics::counter!("stream_frames_encoded_total").increment(1);
                        fps_frames += 1;
                        let elapsed = fps_timer.elapsed();
                        if elapsed >= Duration::from_secs(10) {
                            let fps = fps_frames as f64 / elapsed.as_secs_f64();
                            tracing::debug!("encoder: {fps:.1} fps ({fps_frames} frames in {elapsed:.1?})");
                            fps_frames = 0;
                            fps_timer  = std::time::Instant::now();
                        }
                    }
                    Err(_) => {
                        info!("enc-sink: EOS or pipeline stopped, exiting");
                        break;
                    }
                }
            }
        })?;

    // Push raw I420 frames into the encoder appsrc
    let mut seq = 0u64;
    for frame in raw_rx.iter() {
        let mut buffer = gst::Buffer::with_size(frame.data.len())
            .map_err(|_| anyhow::anyhow!("buffer alloc"))?;

        {
            let buf_ref = buffer.get_mut().unwrap();
            buf_ref.set_pts(gst::ClockTime::from_nseconds(
                frame.pts.as_nanos() as u64,
            ));
            buf_ref.set_dts(gst::ClockTime::from_nseconds(
                frame.pts.as_nanos() as u64,
            ));
            buf_ref.set_duration(gst::ClockTime::from_nseconds(
                Duration::from_millis(33).as_nanos() as u64,
            ));
            let mut map = buf_ref.map_writable().unwrap();
            map.copy_from_slice(&frame.data);
        }

        // If a peer just joined, ask x264enc for a keyframe before this buffer.
        if keyframe_rx.try_recv().is_ok() {
            let s = gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build();
            src.send_event(gst::event::CustomDownstream::builder(s).build());
            info!("forced keyframe for new peer");
        }

        if src.push_buffer(buffer) != Ok(gst::FlowSuccess::Ok) {
            warn!("encoder appsrc push failed at seq {seq}");
        }
        seq += 1;
    }

    // Signal EOS so x264enc flushes its internal buffer and the sink thread
    // receives the EOS event and exits its pull_sample loop cleanly.
    let _ = src.end_of_stream();

    // Wait for the sink thread to drain before tearing down the pipeline.
    if sink_handle.join().is_err() {
        warn!("enc-sink thread panicked");
    }

    enc_pipeline.set_state(gst::State::Null)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// TOKIO RUNTIME — WebRTC + HTTP (async I/O tasks)
// ─────────────────────────────────────────────────────────────────────────────

fn build_webrtc_api() -> Result<webrtc::api::API> {
    let mut m = MediaEngine::default();
    m.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type:      MIME_TYPE_H264.to_owned(),
                clock_rate:     90000,
                channels:       0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;\
                                 packetization-mode=1;\
                                 profile-level-id=42001f"
                    .to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 102,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?;

    Ok(APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build())
}

// ── per-peer WebRTC task ──────────────────────────────────────────────────────

async fn peer_task(
    mut ws:       WebSocket,
    state:        AppState,
    peer_id:      String,
) {
    let pc = match setup_peer_connection(&state).await {
        Ok(p) => p,
        Err(e) => { error!("pc setup: {e:#}"); return; }
    };

    state.peers.insert(peer_id.clone(), Arc::clone(&pc));
    metrics::gauge!("stream_peers_active").increment(1.0);
    // Ask the encoder to produce a keyframe so this peer gets a clean picture ASAP.
    let _ = state.keyframe_tx.try_send(());

    // subscribe to encoded frames BEFORE signaling completes so we
    // don't miss the first keyframe
    let mut frame_rx = state.frame_tx.subscribe();

    // spawn the RTP writer task — runs independently of signaling
    let track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_H264.to_owned(),
            ..Default::default()
        },
        "video".to_owned(),
        "webcam".to_owned(),
    ));
    let track_clone = Arc::clone(&track);

    let _ = pc
        .add_transceiver_from_track(
            Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Sendonly,
                send_encodings: vec![],
            }),
        )
        .await;

    let peer_id_writer = peer_id.clone();
    let writer_handle = tokio::spawn(async move {
        loop {
            match frame_rx.recv().await {
                Ok(frame) => {
                    let sample = Sample {
                        data:     frame.data,
                        duration: frame.duration,
                        ..Default::default()
                    };
                    if track_clone.write_sample(&sample).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("peer {} lagged by {n} frames — skipping", peer_id_writer);
                    metrics::counter!("stream_peer_lag_frames_total").increment(n);
                    // continue: stay connected, catch up from next available frame
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        info!("RTP writer task done for peer {}", peer_id_writer);
    });

    // connection state → cleanup
    let peers_map  = Arc::clone(&state.peers);
    let peer_id_cc = peer_id.clone();
    pc.on_peer_connection_state_change(Box::new(move |s| {
        info!("peer {peer_id_cc} state: {s:?}");
        if matches!(
            s,
            RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Closed
        ) {
            // remove() returns Some only the first time — guards against the gauge
            // being decremented more than once if multiple terminal states fire
            // (e.g. Disconnected → Closed both triggering this callback).
            if peers_map.remove(&peer_id_cc).is_some() {
                metrics::gauge!("stream_peers_active").decrement(1.0);
            }
        }
        Box::pin(async {})
    }));

    // ICE trickle → forward to browser via WebSocket
    // We use a mpsc so the on_ice_candidate closure doesn't need to hold the ws
    let (ice_tx, mut ice_rx) = tokio::sync::mpsc::channel::<RTCIceCandidateInit>(32);
    pc.on_ice_candidate(Box::new(move |c| {
        let ice_tx = ice_tx.clone();
        Box::pin(async move {
            if let Some(c) = c {
                if let Ok(init) = c.to_json() {
                    let _ = ice_tx.send(init).await;
                }
            }
        })
    }));

    // ── signaling loop ────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            // inbound WS message
            msg = ws.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_signal(&pc, &mut ws, &text).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // outbound ICE candidate
            Some(candidate) = ice_rx.recv() => {
                let msg = serde_json::json!({
                    "type": "candidate",
                    "candidate": candidate
                });
                if ws.send(Message::Text(msg.to_string())).await.is_err() {
                    break;
                }
            }
        }
    }

    writer_handle.abort();
    let _ = pc.close().await;
    info!("peer {} disconnected", peer_id);
}

async fn handle_signal(
    pc:  &Arc<RTCPeerConnection>,
    ws:  &mut WebSocket,
    raw: &str,
) {
    let sig: Signal = match serde_json::from_str(raw) {
        Ok(s) => s,
        Err(e) => { warn!("bad signal: {e}"); return; }
    };

    match sig {
        Signal::Offer { sdp } => {
            let offer = match RTCSessionDescription::offer(sdp) {
                Ok(o) => o,
                Err(e) => { error!("offer parse: {e}"); return; }
            };
            if let Err(e) = pc.set_remote_description(offer).await {
                error!("set_remote: {e}"); return;
            }

            let answer = match pc.create_answer(None).await {
                Ok(a) => a,
                Err(e) => { error!("create_answer: {e}"); return; }
            };
            if let Err(e) = pc.set_local_description(answer.clone()).await {
                error!("set_local: {e}"); return;
            }

            let reply = serde_json::json!({
                "type": "answer",
                "sdp":  answer.sdp
            });
            let _ = ws.send(Message::Text(reply.to_string())).await;
        }

        Signal::Candidate { candidate } => {
            if let Err(e) = pc.add_ice_candidate(candidate).await {
                warn!("bad candidate: {e}");
            }
        }

        _ => {}
    }
}

async fn setup_peer_connection(
    state: &AppState,
) -> Result<Arc<RTCPeerConnection>> {
    let mut ice_servers = vec![];

    // Add STUN servers
    for stun_url in &state.ice_servers.stun {
        ice_servers.push(RTCIceServer {
            urls: vec![stun_url.clone()],
            ..Default::default()
        });
    }

    // Add TURN servers
    for (turn_urls, username, password) in &state.ice_servers.turn {
        ice_servers.push(RTCIceServer {
            urls: vec![turn_urls.clone()],
            username: username.clone(),
            credential: password.clone(),
            ..Default::default()
        });
    }

    let config = RTCConfiguration {
        ice_servers,
        ..Default::default()
    };
    Ok(Arc::new(state.webrtc_api.new_peer_connection(config).await?))
}

// ── axum handlers ─────────────────────────────────────────────────────────────

async fn ws_handler(
    ws:    WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    if state.peers.len() >= state.max_peers {
        warn!("peer limit ({}) reached, rejecting connection", state.max_peers);
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.on_upgrade(move |socket| {
        let peer_id = Uuid::new_v4().to_string();
        info!("new peer: {peer_id}");
        peer_task(socket, state, peer_id)
    })
}

async fn health() -> &'static str { "ok" }

async fn metrics_handler(State(state): State<AppState>) -> String {
    state.prometheus_handle.render()
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // rustls 0.23 requires a crypto provider to be installed before any TLS use.
    // webrtc-rs uses rustls internally for DTLS; install ring as the provider.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // ok() — harmless if already installed (e.g. in tests)

    // structured logging — set RUST_LOG=stream=debug for verbose output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    // Suppress benign "agent is closed" warnings from webrtc-rs ICE
                    // cleanup — they fire because candidate teardown races the agent close.
                    "info,webrtc_ice::agent::agent_internal=error".into()
                }),
        )
        .init();

    // ── channel: GST → encoder (bounded, back-pressure safe) ─────────────────
    let (raw_tx, raw_rx) = bounded::<RawFrame>(2);

    // ── channel: encoder → N WebRTC peer tasks (broadcast) ───────────────────
    let (frame_tx, _) = broadcast::channel::<EncodedFrame>(32);

    // ── OS thread 1: GStreamer capture (with restart watchdog) ────────────────
    // If the pipeline dies (camera unplugged, driver crash, etc.) we wait 2 s
    // and restart it automatically. The encoder channel stays alive because
    // raw_tx_gst is cloned on every iteration.
    let raw_tx_gst = raw_tx.clone();
    std::thread::Builder::new()
        .name("gst-watchdog".into())
        .spawn(move || loop {
            info!("Starting GStreamer capture pipeline");
            gst_thread(raw_tx_gst.clone());
            warn!("GStreamer pipeline stopped — restarting in 2 s…");
            std::thread::sleep(Duration::from_secs(2));
        })?;

    // ── channel: peer task → encoder push loop (keyframe requests) ───────────
    // Bounded(4): multiple peers may join at once; excess signals are fine to drop.
    let (keyframe_tx, keyframe_rx) = bounded::<()>(4);

    // ── OS thread 2: x264 encoder ────────────────────────────────────────────
    let frame_tx_enc = frame_tx.clone();
    std::thread::Builder::new()
        .name("encoder".into())
        .spawn(move || encoder_thread(raw_rx, frame_tx_enc, keyframe_rx))?;

    // ── Build the WebRTC API once, share across peers ─────────────────────────
    let webrtc_api = Arc::new(build_webrtc_api()?);

    // ── Load ICE servers from environment or use defaults ─────────────────────
    let ice_servers = IceServers::from_env();
    info!("ICE STUN servers: {:?}", ice_servers.stun);
    if !ice_servers.turn.is_empty() {
        info!("ICE TURN servers configured: {} server(s)", ice_servers.turn.len());
    } else {
        info!("No TURN servers configured. Set TURN_SERVERS env var for production deployments.");
    }

    let max_peers = env::var("MAX_PEERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_usize);
    info!("Max simultaneous peers: {max_peers}");

    let prometheus_handle = PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus recorder")?;

    let state = AppState {
        frame_tx,
        peers: Arc::new(DashMap::new()),
        webrtc_api,
        ice_servers,
        max_peers,
        keyframe_tx,
        prometheus_handle,
    };

    // ── Tokio multi-thread runtime: HTTP + WebRTC ─────────────────────────────
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_cpus::get())   // one worker per logical CPU
        .enable_all()
        .thread_name("tokio-worker")
        .build()?
        .block_on(async move {
            // Keep a handle to the peers map for cleanup after shutdown.
            let shutdown_peers = Arc::clone(&state.peers);

            let app = Router::new()
                .route("/ws",      get(ws_handler))
                .route("/health",  get(health))
                .route("/metrics", get(metrics_handler))
                .nest_service("/", ServeDir::new("static"))
                .layer({
                    // ALLOWED_ORIGINS: comma-separated list of exact origins.
                    // Defaults to localhost for local dev.
                    // For Cloudflare: ALLOWED_ORIGINS=https://stream.yourdomain.com
                    let raw = env::var("ALLOWED_ORIGINS")
                        .unwrap_or_else(|_| "http://localhost:8080,http://127.0.0.1:8080".into());

                    let origins: Vec<axum::http::HeaderValue> = raw
                        .split(',')
                        .map(|s| s.trim().parse().expect("invalid origin in ALLOWED_ORIGINS"))
                        .collect();

                    info!("CORS allowed origins: {:?}", raw);

                    CorsLayer::new()
                        .allow_origin(AllowOrigin::list(origins))
                        .allow_methods([axum::http::Method::GET])
                })
                .with_state(state);

            let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
            info!("Listening → http://localhost:8080");

            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::signal::ctrl_c().await.ok();
                    info!("Ctrl+C received — shutting down");
                })
                .await?;

            // Close every active peer connection cleanly.
            // Collect into a Vec FIRST so we hold no DashMap shard read-guards
            // while awaiting close().  The on_peer_connection_state_change callback
            // calls peers_map.remove() (needs a write guard) during close(); if the
            // iterator's read guard is still held on the same shard, it deadlocks.
            info!("Closing {} peer connection(s)…", shutdown_peers.len());
            let peers_to_close: Vec<Arc<RTCPeerConnection>> = shutdown_peers
                .iter()
                .map(|e| Arc::clone(e.value()))
                .collect();
            drop(shutdown_peers);

            for pc in peers_to_close {
                // Timeout: webrtc-rs close() can stall on ICE teardown; 500 ms is
                // enough for a clean shutdown.  If it times out we move on anyway.
                let _ = tokio::time::timeout(Duration::from_millis(500), pc.close()).await;
            }

            // Give webrtc-rs background tasks a moment to finish ICE teardown
            // before the runtime drops them.
            tokio::time::sleep(Duration::from_millis(200)).await;
            info!("Shutdown complete");

            Ok::<_, anyhow::Error>(())
        })?;

    // On Windows, Ctrl+C re-signals the process after our handler returns,
    // producing exit code 0xc000013a (STATUS_CONTROL_C_EXIT).
    // Exit explicitly with 0 to signal a clean shutdown to the shell/supervisor.
    std::process::exit(0);
}
