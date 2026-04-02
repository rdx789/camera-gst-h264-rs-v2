use axum::extract::ws::{Message, WebSocket};
use std::{
    sync::{atomic::Ordering, Arc},
};
use tracing::{error, info};
use webrtc::{
    rtp_transceiver::{
        rtp_transceiver_direction::RTCRtpTransceiverDirection, RTCRtpTransceiverInit,
    },
    track::track_local::{track_local_static_sample::TrackLocalStaticSample, TrackLocal},
};

use crate::types::{AppState, Signal};

/// RAII guard that decrements `peer_count` when dropped.
///
/// Ensures the count is decremented exactly once regardless of how peer_task
/// exits (normal return, early return, or future cancellation).
struct PeerSlot(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for PeerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Per-peer WebRTC connection task.
///
/// Handles WebRTC setup, RTP streaming, ICE trickle, and signaling for one
/// browser peer. Runs as a Tokio task for the lifetime of the WebSocket.
pub async fn peer_task(mut ws: WebSocket, state: AppState, peer_id: String) {
    // Decrement peer_count on every exit path via Drop.
    let _slot = PeerSlot(Arc::clone(&state.peer_count));

    let pc = match crate::webrtc::setup(&state).await {
        Ok(p) => p,
        Err(e) => {
            error!("peer {peer_id}: PC setup failed: {e:#}");
            return;
        }
    };

    // Register the track before inserting into the map / signaling.
    let track = Arc::new(TrackLocalStaticSample::new(
        webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
            mime_type: "video/h264".to_owned(),
            ..Default::default()
        },
        "video".to_owned(),
        "webcam".to_owned(),
    ));

    if let Err(e) = pc
        .add_transceiver_from_track(
            Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Sendonly,
                send_encodings: vec![],
            }),
        )
        .await
    {
        error!("peer {peer_id}: add_transceiver failed: {e:#}");
        let _ = pc.close().await;
        return;
    }

    // Subscribe to the encoded-frame broadcast BEFORE signaling completes so
    // we don't miss the first keyframe while the offer/answer round-trips.
    let frame_rx = state.frame_tx.subscribe();

    state.peers.insert(peer_id.clone(), Arc::clone(&pc));
    crate::metrics::increment_active_peers();
    // Request a keyframe so this peer gets a clean picture without waiting up
    // to key-int-max (60 frames / ~2 s) for the next natural IDR.
    let _ = state.keyframe_tx.try_send(());

    let peer_id_writer = peer_id.clone();
    let writer_handle = tokio::spawn(async move {
        crate::writer::writer_task(frame_rx, track, peer_id_writer).await;
    });

    // State-change callback: remove from map and update metrics gauge.
    // `remove().is_some()` ensures the gauge is only decremented once even
    // if multiple terminal states fire (e.g. Disconnected then Closed).
    let peers_map = Arc::clone(&state.peers);
    let peer_id_cc = peer_id.clone();
    pc.on_peer_connection_state_change(Box::new(move |s| {
        info!("peer {peer_id_cc} state → {s:?}");
        if matches!(
            s,
            webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
                | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected
                | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed
        ) {
            if peers_map.remove(&peer_id_cc).is_some() {
                crate::metrics::decrement_active_peers();
            }
        }
        Box::pin(async {})
    }));

    // ICE trickle: on_ice_candidate fires on a background thread; forward
    // candidates to the signaling loop via mpsc rather than holding the ws.
    let (ice_tx, mut ice_rx) =
        tokio::sync::mpsc::channel::<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>(32);
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
            msg = ws.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        crate::webrtc::handle_message(&pc, &mut ws, &text).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // Forward ICE candidates to the browser using the Signal enum so
            // the JSON structure is defined in one place.
            Some(candidate) = ice_rx.recv() => {
                if let Ok(msg) = serde_json::to_string(&Signal::Candidate { candidate }) {
                    if ws.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // Abort the writer task and wait for it to stop before closing the PC.
    // Without the await the task can still be mid-write when close() tears
    // down the underlying DTLS/RTP stack.
    writer_handle.abort();
    let _ = writer_handle.await; // returns Err(Cancelled) — that's expected

    let _ = pc.close().await;
    info!("peer {peer_id} disconnected");
}
