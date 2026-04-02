# camera-gst-h264-rs-v2

A high-performance, multi-client WebRTC video streaming server built in Rust. Streams H.264 video from a USB camera to multiple simultaneous clients using peer-to-peer WebRTC connections.

Refactored from `sample-camera-h264-ws-rs` (v1) into a modular multi-file architecture with correctness fixes and Prometheus metrics.

## Features

- **Multi-client streaming** — Single encode, broadcast to up to N peers (configurable)
- **WebRTC P2P** — Low-latency direct peer connections via `webrtc-rs`
- **H.264 encoding** — x264 encoder, H.264 Baseline profile level 4.0 (1080p capable)
- **GStreamer capture** — `mfvideosrc` pipeline on Windows (DirectShow/MediaFoundation)
- **WebSocket signaling** — SDP offer/answer + ICE trickle
- **Prometheus metrics** — Frame counters, peer counts, encoder stats at `/metrics`
- **Atomic peer limit** — TOCTOU-safe connection gating with RAII cleanup
- **Cloudflare Tunnel** — Expose securely via your existing domain

## Architecture

```
USB Camera
    ↓
GStreamer Pipeline  (mfvideosrc → NV12 → I420, capture thread)
    ↓
x264 Encoder  (dedicated OS thread, single pass)
    ↓
Broadcast Channel  (encoded frames → N peers)
    ↓
Writer Tasks  (one per peer, RTP packetization)
    ↓
WebRTC Peer Connections  (browser clients)
```

**Key design decisions:**
- Single encoder instance — frames broadcast to all peers, no per-client re-encode
- Per-client frame dropping (`drop=true`) — slow clients don't block others or exhaust memory
- Encoder thread count = `max(1, cpus / 2)` — avoids contention with the Tokio runtime
- PeerSlot RAII guard — peer count is always decremented on disconnect or error

## Project Structure

```
.
├── src/
│   ├── main.rs          # Startup, thread spawning, router setup
│   ├── config.rs        # Config struct, env var loading
│   ├── types.rs         # RawFrame, EncodedFrame, Signal, AppState
│   ├── capture.rs       # GStreamer capture thread
│   ├── encoder.rs       # x264 encoder thread
│   ├── peer.rs          # peer_task — WebRTC setup, ICE, signaling loop
│   ├── writer.rs        # writer_task — broadcast frame → RTP track
│   ├── routes.rs        # build_router, ws_handler, health, metrics
│   ├── metrics.rs       # Prometheus helpers
│   └── webrtc/
│       ├── mod.rs
│       ├── api.rs        # build_api — RTCPeerConnection factory
│       ├── connection.rs # setup() — ICE servers, codec configuration
│       └── signaling.rs  # handle_message — Signal enum in/out
├── static/
│   └── index.html       # Browser client UI
├── run-device-0.ps1     # Launch script for device 0 (PC/built-in camera, 720p)
├── run-device-1.ps1     # Launch script for device 1 (USB camera, 1080p)
├── DEVICE-CONFIG.md     # Camera configuration reference
├── CLOUDFLARE_TUNNEL_SETUP.md
├── Cargo.toml
└── README.md
```

## Quick Start

### Prerequisites

- Windows (Linux deferred — requires pipeline swap to `v4l2src`)
- Rust 1.80+
- GStreamer runtime + development libraries (1.22+)
- A USB or built-in camera

### Build

```powershell
cd c:\dev\RUST\STREAM\camera-gst-h264-rs-v2
cargo build --release
```

### Run with launch scripts

```powershell
# Device 0 — PC/built-in camera (1280x720 @ 30fps, 2000 kbps)
.\run-device-0.ps1

# Device 1 — USB camera (1920x1080 @ 30fps, 5000 kbps)
.\run-device-1.ps1
```

Both scripts set environment variables and call `cargo run --release`. See [DEVICE-CONFIG.md](DEVICE-CONFIG.md) for full configuration details.

### Run manually

```powershell
$env:CAMERA_DEVICE = "1"
$env:ENCODER_BITRATE = "5000"
$env:PORT = "8080"
$env:MAX_PEERS = "50"
cargo run --release
```

## Configuration

All settings are controlled via environment variables — no source edits needed.

| Variable | Default | Description |
|---|---|---|
| `CAMERA_DEVICE` | `0` | Camera device index |
| `WIDTH` | `1280` | Capture width (pixels) |
| `HEIGHT` | `720` | Capture height (pixels) |
| `FPS` | `30` | Capture frame rate |
| `ENCODER_BITRATE` | `2000` | x264 bitrate (kbps) |
| `ENCODER_THREADS` | `max(1, cpus/2)` | x264 thread count |
| `PORT` | `8080` | HTTP server port |
| `MAX_PEERS` | `50` | Max simultaneous WebRTC peers |
| `ALLOWED_ORIGINS` | `*` | CORS allowed origins |
| `STUN_SERVERS` | Google STUN | Comma-separated STUN URLs |
| `TURN_SERVERS` | _(none)_ | `turn:host:port:user:pass` |

## API

### HTTP Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/` | Browser client UI (index.html) |
| `GET` | `/health` | Health check — returns `200 OK` |
| `GET` | `/ws` | WebSocket upgrade for WebRTC signaling |
| `GET` | `/metrics` | Prometheus metrics |

### WebSocket Signaling (`/ws`)

**Client → Server:**
```json
{"type": "offer", "sdp": "..."}
{"type": "candidate", "candidate": "...", "sdpMid": "...", "sdpMLineIndex": 0}
```

**Server → Client:**
```json
{"type": "answer", "sdp": "..."}
{"type": "candidate", "candidate": "...", "sdpMid": "...", "sdpMLineIndex": 0}
```

## Logging

```powershell
# Default (info)
cargo run --release

# Debug
$env:RUST_LOG = "camera_gst_h264_rs_v2=debug"; cargo run --release

# Trace (very verbose)
$env:RUST_LOG = "camera_gst_h264_rs_v2=trace,webrtc=debug"; cargo run --release
```

## Performance

- **Encoding:** Single x264 pass @ 1080p 30fps ≈ 10–20% CPU (depends on thread count)
- **Scaling:** Additional clients consume ~10 MB RAM and minimal extra CPU (no re-encode)
- **Latency:** 50–200 ms typical (peer-to-peer, LAN)
- **Memory:** ~80–120 MB base + ~10 MB per peer connection

## Troubleshooting

### Camera not found / pipeline fails to start
- Verify device index: `$env:CAMERA_DEVICE = "1"` and retry
- Check Windows Device Manager to confirm the camera is enumerated
- Try a lower resolution: `$env:WIDTH=1280; $env:HEIGHT=720`

### ICE warnings in logs (IPv6/link-local)
Normal on Windows. These addresses cannot be bound but do not affect connectivity — ignore them.

### No video in browser
1. Open browser DevTools → Console for WebRTC errors
2. Run with `RUST_LOG=camera_gst_h264_rs_v2=debug` to trace signaling
3. Check `/health` and `/metrics` to confirm the server and encoder are running

### Max peers reached
Increase the limit: `$env:MAX_PEERS = "100"` — or check `/metrics` to see current peer count.

### Deploy via Cloudflare Tunnel

See [CLOUDFLARE_TUNNEL_SETUP.md](CLOUDFLARE_TUNNEL_SETUP.md).

```powershell
# Terminal 1
.\run-device-1.ps1

# Terminal 2
cloudflared tunnel run --url http://localhost:8080 my-rtc-stream
```

## Dependencies

| Crate | Purpose |
|---|---|
| `axum` | HTTP server + WebSocket |
| `tokio` | Async runtime |
| `webrtc` | WebRTC peer connections |
| `gstreamer` / `gstreamer-app` | Camera capture pipeline |
| `x264` | H.264 encoding |
| `tower-http` | Static file serving, CORS |
| `metrics` + `metrics-exporter-prometheus` | Prometheus metrics |
| `dashmap` | Concurrent peer map |
| `crossbeam-channel` | Encoder ↔ writer channel |
| `serde` / `serde_json` | Signaling message serialization |
| `tracing` / `tracing-subscriber` | Structured logging |
| `anyhow` | Error handling |
| `uuid` | Peer IDs |
| `num_cpus` | Encoder thread auto-detection |

## License

MIT
