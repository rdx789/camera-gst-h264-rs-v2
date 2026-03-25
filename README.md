# WebRTC H.264 Video Streaming Server

A high-performance, multi-client WebRTC video streaming server built in Rust. Streams H.264 video from a USB camera to multiple clients simultaneously using P2P connections.

## Features

✅ **Multi-client streaming** — Single encode, broadcast to unlimited clients  
✅ **WebRTC P2P** — Low-latency direct peer connections  
✅ **H.264 encoding** — Hardware-accelerated x264 encoder  
✅ **GStreamer integration** — Flexible media pipeline  
✅ **WebSocket signaling** — SDP offer/answer + ICE candidates  
✅ **Production-ready** — Proper error handling, logging, resource cleanup  
✅ **Cloudflare Tunnel** — Expose securely via your existing domain  

## Architecture

```
USB Camera
    ↓
GStreamer Pipeline (NV12 capture → I420 conversion)
    ↓
x264 Encoder (dedicated OS thread, single pass)
    ↓
Broadcast Channel (encoded frames → N peers)
    ↓
WebRTC Peers (each client gets own peer connection)
    ↓
Clients (browser WebRTC)
```

**Key Design:**
- **Single encode** — One x264 encoder instance, frames broadcast to all clients
- **Per-client frame dropping** — Slow clients don't affect others
- **Efficient** — Scales to many clients with minimal CPU/memory
- **Production pattern** — Used by Janus, Kurento, LiveKit

## Quick Start

### Prerequisites

- Windows with USB camera
- Rust 1.70+
- GStreamer development libraries
- x264 encoder

### Build

```powershell
cd c:\Dev\RUST\STREAM\sample-camera-h264-ws-rs
cargo build --release
```

### Run Locally

```powershell
cargo run
# Server listens on http://localhost:8080
# Open browser and connect to ws://localhost:8080/ws
```

### Deploy via Cloudflare Tunnel

See [CLOUDFLARE_TUNNEL_SETUP.md](CLOUDFLARE_TUNNEL_SETUP.md) for detailed instructions.

Quick version:
```powershell
# Terminal 1: Run app
cargo run

# Terminal 2: Start tunnel (requires cloudflared CLI)
cloudflared tunnel run --url http://localhost:8080 my-rtc-stream
```

## Configuration

### HTTP Server Port

Change port from default 8080:

```rust
// src/main.rs:596
let addr = SocketAddr::from(([0, 0, 0, 0], 8080));  // ← Change here
```

### ICE Servers (STUN/TURN)

**Local testing (default):**
```powershell
cargo run
# Uses Google's public STUN server
```

**Production with TURN:**
```powershell
$env:STUN_SERVERS="stun:stun.l.google.com:19302,stun:stun1.l.google.com:19302"
$env:TURN_SERVERS="turn:turnserver.com:3478:user:pass"
cargo run
```

## API

### WebSocket Endpoint

**`ws://localhost:8080/ws`**

**Client → Server:**
```json
{"type": "offer", "sdp": "..."}
{"type": "candidate", "candidate": "..."}
```

**Server → Client:**
```json
{"type": "answer", "sdp": "..."}
{"type": "candidate", "candidate": "..."}
```

### HTTP Endpoints

- `GET /` — Serve index.html (static UI)
- `GET /health` — Health check
- `GET /ws` — WebSocket upgrade for WebRTC signaling

## Logging

Control verbosity with `RUST_LOG`:

```powershell
# Info level (default)
cargo run

# Debug level
$env:RUST_LOG="stream=debug"; cargo run

# Trace (very verbose)
$env:RUST_LOG="stream=trace,webrtc=debug"; cargo run
```

## Performance

- **Encoding:** Single x264 pass @ 1280x720 30fps ≈ 5-15% CPU per x264 thread
- **Scaling:** Add clients without re-encoding
- **Latency:** 50-200ms typical (peer-to-peer)
- **Memory:** ~50-100MB base + ~10MB per peer connection

## Troubleshooting

### "mfvideosrc Failed to start"
- **Issue:** USB camera not found or wrong format
- **Fix:** Update pipeline in `src/main.rs` line ~106 with correct device/resolution

### ICE warnings in logs
- **Issue:** IPv6/link-local addresses can't be bound
- **Fix:** Normal on Windows, ignore them (only affects IPv6 neighbors, not needed locally)

### WebSocket connection fails
- **Issue:** Client trying to connect to wrong address
- **Fix:** Ensure WebSocket URL matches server address (e.g., `ws://localhost:8080/ws`)

### No video in browser
1. Check browser console for errors
2. Run with `RUST_LOG=stream=debug` to see server logs
3. Verify camera is working with `cargo run` output

## Project Structure

```
.
├── src/
│   └── main.rs          # All logic (single file for simplicity)
├── static/
│   └── index.html       # Browser client UI
├── Cargo.toml           # Dependencies
├── CLOUDFLARE_TUNNEL_SETUP.md   # Deployment guide
└── README.md            # This file
```

## Dependencies

- `axum` — HTTP server
- `tokio` — Async runtime
- `webrtc` — WebRTC implementation
- `gstreamer` — Media capture/processing
- `x264` — H.264 encoding
- `serde_json` — JSON serialization

## License

MIT

## References

- [WebRTC Spec](https://w3c.github.io/webrtc-pc/)
- [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
- [GStreamer Docs](https://gstreamer.freedesktop.org/)
- [x264 Encoder](https://www.videolan.org/developers/x264.html)
