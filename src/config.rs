use std::env;
use tracing::info;

/// Application configuration loaded from environment variables.
#[derive(Clone)]
pub struct Config {
    /// HTTP server port (env: PORT, default 8080)
    pub port: u16,
    /// Maximum simultaneous peer connections (env: MAX_PEERS, default 50)
    pub max_peers: usize,
    /// CORS allowed origins (env: ALLOWED_ORIGINS, default localhost)
    pub allowed_origins: Vec<String>,
    /// USB camera device index (env: CAMERA_DEVICE, default 0)
    pub camera_device: u32,
    /// Capture/encode width — derived from CAMERA_DEVICE, overridable via CAMERA_WIDTH
    pub width: u32,
    /// Capture/encode height — derived from CAMERA_DEVICE, overridable via CAMERA_HEIGHT
    pub height: u32,
    /// Capture frame rate (env: CAMERA_FPS, default 30)
    pub fps: u32,
    /// H.264 encoder bitrate in kbps (env: ENCODER_BITRATE, default 5000)
    pub encoder_bitrate: u32,
    /// Number of x264 encoder threads (env: ENCODER_THREADS, default half of CPU cores)
    ///
    /// Defaults to half the logical CPU count so the Tokio runtime and the
    /// GStreamer capture thread still have cores available.
    pub encoder_threads: u32,
}

impl Config {
    /// Load configuration from environment variables with sensible defaults.
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);

        let max_peers = env::var("MAX_PEERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);

        let allowed_origins = env::var("ALLOWED_ORIGINS")
            .unwrap_or_else(|_| "http://localhost:8080,http://127.0.0.1:8080".into())
            .split(',')
            .map(|s| s.trim().to_owned())
            .collect();

        let camera_device: u32 = env::var("CAMERA_DEVICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        // Resolution is derived from device index so capture and encoder share
        // a single source of truth — no more duplicated match arms.
        // Device 0 (PC / built-in): 1280×720. Device 1+ (USB): 1920×1080.
        let (default_w, default_h) = match camera_device {
            0 => (1280u32, 720u32),
            _ => (1920u32, 1080u32),
        };
        let width = env::var("CAMERA_WIDTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_w);
        let height = env::var("CAMERA_HEIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_h);
        let fps = env::var("CAMERA_FPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30u32);

        let encoder_bitrate = env::var("ENCODER_BITRATE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000);

        // Default to half the logical cores: x264 is CPU-heavy and the Tokio
        // runtime + capture thread also need headroom. Minimum 1.
        let default_threads = ((num_cpus::get() / 2).max(1)) as u32;
        let encoder_threads = env::var("ENCODER_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_threads);

        Self {
            port,
            max_peers,
            allowed_origins,
            camera_device,
            width,
            height,
            fps,
            encoder_bitrate,
            encoder_threads,
        }
    }

    /// Log the resolved configuration. Called from main after tracing is ready.
    pub fn log(&self) {
        info!(
            "Config: port={} max_peers={} device={} resolution={}x{}@{}fps \
             bitrate={}kbps enc_threads={}",
            self.port,
            self.max_peers,
            self.camera_device,
            self.width,
            self.height,
            self.fps,
            self.encoder_bitrate,
            self.encoder_threads,
        );
        info!("CORS allowed origins: {:?}", self.allowed_origins);
    }
}
