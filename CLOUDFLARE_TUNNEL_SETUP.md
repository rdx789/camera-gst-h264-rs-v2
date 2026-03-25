# Cloudflare Tunnel Deployment Guide

This guide explains how to deploy the WebRTC video streaming app through Cloudflare Tunnel using your existing domain.

## Architecture

```
Internet (Your Domain) 
    ↓
Cloudflare Edge
    ↓ (via encrypted tunnel)
Your Machine (localhost:8080)
    ↓
Local Clients + WebRTC P2P streams
```

## Prerequisites

- ✅ Cloudflare account with a domain
- ✅ Cloudflare CLI installed
- ✅ App running on `localhost:8080`

## Installation

### 1. Install Cloudflare CLI

**Windows:**
```powershell
# Option A: Download directly
# https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/

# Option B: Via Chocolatey
choco install cloudflare-warp
```

### 2. Authenticate

```powershell
cloudflared login
# Opens browser to authorize -> select your domain
```

## Deployment

### Step 1: Run the App

```powershell
cd c:\Dev\RUST\STREAM\sample-camera-h264-ws-rs
cargo run
# App listens on http://localhost:8080
```

### Step 2: Create Tunnel (One-time setup)

```powershell
cloudflared tunnel create my-rtc-stream
# Returns: Tunnel ID and tunnel.json credentials file
```

### Step 3: Route Domain to Tunnel

```powershell
# If your domain is "yourdomain.com"
cloudflared tunnel route dns my-rtc-stream yourdomain.com

# Or for subdomain: "stream.yourdomain.com"
cloudflared tunnel route dns my-rtc-stream stream.yourdomain.com
```

### Step 4: Run the Tunnel

```powershell
cloudflared tunnel run --url http://localhost:8080 my-rtc-stream
# Tunnel is now live!
```

## Usage

Once deployed, clients connect to:
```
https://yourdomain.com  (or your configured subdomain)
```

**How it works:**
1. Client browser connects via HTTPS to your domain (through Cloudflare)
2. WebSocket signaling (SDP offers/answers, ICE candidates) flows through the tunnel
3. Media streams (H.264 video) can go direct P2P or through tunnel depending on firewall

## Advanced: Multiple Tunnels

Create routes in `~/.cloudflared/config.yml`:

```yaml
tunnel: my-rtc-stream
credentials-file: ~/.cloudflared/my-rtc-stream.json

ingress:
  - hostname: yourdomain.com
    service: http://localhost:8080
  - hostname: stream.yourdomain.com
    service: http://localhost:8080
  - service: http_status:404
```

Then run:
```powershell
cloudflared tunnel run
```

## Environment Variables (Optional)

### CORS — Allowed Origins

By default the server only accepts connections from `http://localhost:8080`.
When serving through Cloudflare you must set `ALLOWED_ORIGINS` to your public domain
(Cloudflare always serves over HTTPS):

```powershell
# Single domain
$env:ALLOWED_ORIGINS="https://stream.yourdomain.com"
cargo run

# Multiple domains (comma-separated)
$env:ALLOWED_ORIGINS="https://stream.yourdomain.com,https://yourdomain.com"
cargo run
```

> **Note:** The origin must match exactly — scheme (`https://`), hostname, and port (if non-standard).
> No trailing slashes.

### TURN Servers

For production with TURN servers (format: `url|user|pass`, semicolon-separated):

```powershell
$env:TURN_SERVERS="turn:yourserver.com:3478|user|pass"
cargo run
```

## Troubleshooting

**Tunnel won't start:**
```powershell
# Check tunnel status
cloudflared tunnel list

# View tunnel logs
cloudflared tunnel logs my-rtc-stream
```

**Can't connect from outside:**
- Verify DNS is pointing to Cloudflare (nameservers)
- Check Cloudflare SSL/TLS is set to "Flexible" or higher
- Ensure firewall allows WebSocket upgrades

**ICE connection issues:**
- Default STUN server works for most cases
- If behind very restrictive NAT, add TURN with `$env:TURN_SERVERS`

## Security Considerations

✅ **Cloudflare Tunnel provides:**
- End-to-end encryption (TLS)
- DDoS protection
- No inbound firewall rules needed
- Access control via Cloudflare Teams (optional)

✅ **WebRTC provides:**
- Direct P2P encryption between peers
- DTLS-SRTP for media

## Cost

- **Cloudflare Tunnel**: Free tier available
- Check: https://developers.cloudflare.com/cloudflare-one/pricing/

## References

- [Cloudflare Tunnel Docs](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
- [CloudflareD CLI Docs](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/install-and-setup/installation/)
