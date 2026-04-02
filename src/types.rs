use bytes::Bytes;
use crossbeam_channel::Sender as CbSender;
use dashmap::DashMap;
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use std::{env, sync::{atomic::AtomicUsize, Arc}, time::Duration};
use tokio::sync::broadcast;
use tracing::warn;
use webrtc::{
    ice_transport::ice_candidate::RTCIceCandidateInit,
    peer_connection::RTCPeerConnection,
};

/// A raw I420 frame from the GStreamer capture pipeline.
#[derive(Clone)]
pub struct RawFrame {
    pub data: Bytes,
    pub pts:  Duration,
}

/// An encoded H.264 NAL unit ready for RTP packetization.
#[derive(Clone)]
pub struct EncodedFrame {
    pub data:     Bytes,
    pub duration: Duration,
}

/// WebRTC signaling message (SDP offer/answer or ICE candidate).
///
/// Both inbound (browser → server) and outbound (server → browser) messages
/// are represented here so serialization is handled in one place.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Signal {
    Offer     { sdp: String },
    Answer    { sdp: String },
    Candidate { candidate: RTCIceCandidateInit },
}

/// ICE server configuration (STUN/TURN) for NAT traversal.
#[derive(Clone)]
pub struct IceServers {
    pub stun: Vec<String>,
    /// Each entry is (url, username, password).
    pub turn: Vec<(String, String, String)>,
}

impl Default for IceServers {
    fn default() -> Self {
        Self {
            stun: vec!["stun:stun.l.google.com:19302".to_owned()],
            turn: vec![],
        }
    }
}

impl IceServers {
    /// Load ICE servers from environment variables or use defaults.
    ///
    /// - `STUN_SERVERS`: comma-separated STUN URLs.
    /// - `TURN_SERVERS`: semicolon-separated `url|user|pass` triplets.
    pub fn from_env() -> Self {
        let mut servers = IceServers::default();

        if let Ok(stun_list) = env::var("STUN_SERVERS") {
            servers.stun = stun_list.split(',').map(|s| s.trim().to_owned()).collect();
        }

        if let Ok(turn_list) = env::var("TURN_SERVERS") {
            servers.turn = turn_list
                .split(';')
                .filter_map(|s| {
                    let parts: Vec<&str> = s.trim().splitn(3, '|').collect();
                    if parts.len() == 3 {
                        Some((parts[0].to_owned(), parts[1].to_owned(), parts[2].to_owned()))
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

/// Shared application state — cheap to clone (all fields are Arc-wrapped).
#[derive(Clone)]
pub struct AppState {
    /// Broadcast channel: encoder → N peer tasks.
    pub frame_tx: broadcast::Sender<EncodedFrame>,
    /// Active peer connections (uuid → PC). Used for graceful shutdown.
    pub peers: Arc<DashMap<String, Arc<RTCPeerConnection>>>,
    /// Atomic peer counter used for the connection limit check.
    ///
    /// Separate from `peers.len()` to allow an atomic check-and-increment
    /// in the WebSocket upgrade handler, avoiding the TOCTOU race where
    /// `peers.len() >= max_peers` could be true for many concurrent arrivals
    /// before any of them has inserted into the map.
    pub peer_count: Arc<AtomicUsize>,
    /// WebRTC API (reused across peers for efficiency).
    pub webrtc_api: Arc<webrtc::api::API>,
    /// ICE servers for NAT traversal (STUN/TURN).
    pub ice_servers: IceServers,
    /// Maximum simultaneous viewers.
    pub max_peers: usize,
    /// Signal the encoder to produce a keyframe (sent when a new peer joins).
    pub keyframe_tx: CbSender<()>,
    /// Prometheus metrics handle — rendered at GET /metrics.
    pub prometheus_handle: PrometheusHandle,
}
