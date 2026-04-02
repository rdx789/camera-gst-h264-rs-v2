# ============================================================================
# WebRTC H.264 Streaming Server - Device 0 Configuration
# ============================================================================
# Optimized settings for PC/built-in camera on device index 0
# 1280x720 @ 30fps, 5000 kbps bitrate, all CPU cores
# ============================================================================

Write-Host "================================================" -ForegroundColor Cyan
Write-Host "  WebRTC Camera Streamer - Device 0" -ForegroundColor Cyan
Write-Host "================================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Configuration:" -ForegroundColor Yellow
Write-Host "  Camera Device:     0 (PC/built-in camera)" -ForegroundColor White
Write-Host "  Resolution:        1280x720 @ 30fps" -ForegroundColor White
Write-Host "  Encoder Bitrate:   2000 kbps" -ForegroundColor White
Write-Host "  Encoder Threads:   All CPU cores (auto)" -ForegroundColor White
Write-Host "  HTTP Port:         8080" -ForegroundColor White
Write-Host "  Max Peers:         50" -ForegroundColor White
Write-Host ""
Write-Host "Server will be available at:  http://localhost:8080" -ForegroundColor Green
Write-Host "WebSocket endpoint:            ws://localhost:8080/ws" -ForegroundColor Green
Write-Host "Health check:                  http://localhost:8080/health" -ForegroundColor Green
Write-Host "Metrics endpoint:              http://localhost:8080/metrics" -ForegroundColor Green
Write-Host ""
Write-Host "Press Ctrl+C to stop the server" -ForegroundColor Yellow
Write-Host ""

# Set environment variables
$env:CAMERA_DEVICE = "0"
$env:ENCODER_BITRATE = "2000"
$env:ENCODER_THREADS = [string]$([System.Environment]::ProcessorCount)
$env:PORT = "8080"
$env:MAX_PEERS = "50"

# Run the release binary
& cargo run --release
