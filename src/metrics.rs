use anyhow::{Context, Result};
use metrics_exporter_prometheus::PrometheusBuilder;

/// Initialize Prometheus metrics collection
pub fn init() -> Result<metrics_exporter_prometheus::PrometheusHandle> {
    PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus recorder")
}

/// Record metrics are defined inline throughout the codebase:
/// - `stream_encoder_drops_total` — Raw frames dropped due to encoder backpressure
/// - `stream_frames_encoded_total` — H.264 frames successfully encoded
/// - `stream_peers_active` — Currently connected peer connections (gauge)
/// - `stream_peer_lag_frames_total` — Cumulative frames skipped by slow clients
pub fn record_encoder_drop() {
    metrics::counter!("stream_encoder_drops_total").increment(1);
}

pub fn record_frame_encoded() {
    metrics::counter!("stream_frames_encoded_total").increment(1);
}

pub fn record_peer_lag(n: u64) {
    metrics::counter!("stream_peer_lag_frames_total").increment(n);
}

pub fn increment_active_peers() {
    metrics::gauge!("stream_peers_active").increment(1.0);
}

pub fn decrement_active_peers() {
    metrics::gauge!("stream_peers_active").decrement(1.0);
}
