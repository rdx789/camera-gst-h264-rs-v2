# Camera Device Configuration Guide

Two pre-configured scripts are provided for optimal streaming with your USB cameras.

## Quick Start

### Device 0 (PC/Built-in Camera)
```powershell
.\run-device-0.ps1
```

### Device 1 (USB Camera) ← Your External Camera
```powershell
.\run-device-1.ps1
```

## Configuration Details

Both scripts use the same optimized settings:

| Setting | Value |
|---------|-------|
| **Resolution** | 1920×1080 @ 30fps |
| **Encoder Bitrate** | 5000 kbps |
| **Encoder Threads** | All CPU cores (auto-detected) |
| **HTTP Port** | 8080 |
| **Max Simultaneous Peers** | 50 |

### Device Mapping

- **Device 0** → PC/built-in camera
- **Device 1** → USB camera (your external camera)

## Access Points

Once the server starts, you can access:

| Service | URL |
|---------|-----|
| **Web Interface** | `http://localhost:8080` |
| **WebSocket Signaling** | `ws://localhost:8080/ws` |
| **Health Check** | `http://localhost:8080/health` |
| **Prometheus Metrics** | `http://localhost:8080/metrics` |

## Performance Metrics

### Quality vs. Bitrate

- **5000 kbps** (default) → Full quality 1080p, smooth playback, minimal artifacts
- **3000 kbps** → Lower bandwidth, acceptable quality for slower networks
- **8000 kbps** → Maximum quality, requires more bandwidth

### Network Requirements

| Bitrate | Network | Recommendation |
|---------|---------|-----------------|
| 3000 kbps | < 10 Mbps | Mobile/WiFi |
| 5000 kbps | > 10 Mbps | **Default (LAN/home)** |
| 8000 kbps | > 20 Mbps | High-speed networks |

## Manual Configuration

To customize, set environment variables before running:

```powershell
# Custom settings
$env:CAMERA_DEVICE = "1"
$env:ENCODER_BITRATE = "6000"
$env:ENCODER_THREADS = "8"
$env:MAX_PEERS = "100"

cargo run --release
```

## Troubleshooting

### Video quality issues?
- Increase bitrate: `$env:ENCODER_BITRATE = "8000"`
- Check network: `http://localhost:8080/metrics` for dropped frames

### Camera not detected?
- Verify device index (0 or 1): `$env:CAMERA_DEVICE = "1"`
- Check Windows Device Manager for available cameras

### Server slow?
- Reduce bitrate: `$env:ENCODER_BITRATE = "3000"`
- Limit peers: `$env:MAX_PEERS = "10"`

## Multi-Client Streaming

The server supports multiple simultaneous WebRTC connections (up to `MAX_PEERS`).

Each client:
- Receives independent P2P connection
- Gets individual frame drops if slow (doesn't affect others)
- Full 1920×1080 @ 30fps stream at configured bitrate

## Docker Deployment

To run in Docker, ensure device passthrough:

```bash
docker run --device /dev/video0 --device /dev/video1 \
  -e CAMERA_DEVICE=1 \
  -e ENCODER_BITRATE=5000 \
  -p 8080:8080 \
  camera-gst-h264-rs
```

## Linux Support

On Linux, edit the pipeline in `src/gst_pipeline.rs`:

```rust
// Replace mfvideosrc with v4l2src:
let pipeline_str = format!(
    "v4l2src device=/dev/video{} ! \
     video/x-raw,format=NV12,width=1920,height=1080,framerate=30/1 \
     ...",
    device_index
);
```

Then rebuild: `cargo build --release`
