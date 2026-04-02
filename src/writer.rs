use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;
use webrtc::{media::Sample, track::track_local::track_local_static_sample::TrackLocalStaticSample};

use crate::types::EncodedFrame;

/// Consume encoded frames from the broadcast channel and write them to a
/// WebRTC track via RTP packetization.
///
/// Runs as a Tokio task per peer. Handles broadcast lag gracefully by logging
/// and skipping rather than disconnecting, so a momentarily slow client
/// recovers automatically.
pub async fn writer_task(
    mut frame_rx: broadcast::Receiver<EncodedFrame>,
    track: Arc<TrackLocalStaticSample>,
    peer_id: String,
) {
    loop {
        match frame_rx.recv().await {
            Ok(frame) => {
                let sample = Sample {
                    data: frame.data,
                    duration: frame.duration,
                    ..Default::default()
                };
                if track.write_sample(&sample).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("peer {peer_id} lagged by {n} frames — skipping");
                crate::metrics::record_peer_lag(n);
                // Stay connected; catch up from the next available frame.
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    tracing::info!("RTP writer done for peer {peer_id}");
}
