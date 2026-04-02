use axum::extract::ws::{Message, WebSocket};
use std::sync::Arc;
use tracing::{error, warn};
use webrtc::peer_connection::{sdp::session_description::RTCSessionDescription, RTCPeerConnection};

use crate::types::Signal;

/// Handle an inbound WebRTC signaling message (SDP offer or ICE candidate).
///
/// Uses `Signal` for both deserialization and serialization so the JSON
/// wire format is defined in exactly one place.
pub async fn handle_message(pc: &Arc<RTCPeerConnection>, ws: &mut WebSocket, raw: &str) {
    let sig: Signal = match serde_json::from_str(raw) {
        Ok(s) => s,
        Err(e) => {
            warn!("bad signaling message: {e}");
            return;
        }
    };

    match sig {
        Signal::Offer { sdp } => {
            let offer = match RTCSessionDescription::offer(sdp) {
                Ok(o) => o,
                Err(e) => { error!("offer parse failed: {e}"); return; }
            };
            if let Err(e) = pc.set_remote_description(offer).await {
                error!("set_remote_description failed: {e}"); return;
            }

            let answer = match pc.create_answer(None).await {
                Ok(a) => a,
                Err(e) => { error!("create_answer failed: {e}"); return; }
            };
            if let Err(e) = pc.set_local_description(answer.clone()).await {
                error!("set_local_description failed: {e}"); return;
            }

            // Serialize via Signal::Answer so the structure matches what the
            // browser expects and is consistent with outbound ICE candidates.
            match serde_json::to_string(&Signal::Answer { sdp: answer.sdp }) {
                Ok(msg) => { let _ = ws.send(Message::Text(msg)).await; }
                Err(e)  => { error!("answer serialize failed: {e}"); }
            }
        }

        Signal::Candidate { candidate } => {
            if let Err(e) = pc.add_ice_candidate(candidate).await {
                warn!("add_ice_candidate failed: {e}");
            }
        }

        // The server is always the answerer in this design; an Answer arriving
        // from the browser is unexpected but harmless.
        Signal::Answer { .. } => {
            warn!("received unexpected Answer signal — ignoring");
        }
    }
}
