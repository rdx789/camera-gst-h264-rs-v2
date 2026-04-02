use anyhow::{Context, Result};
use crossbeam_channel::Receiver as CbReceiver;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::types::{EncodedFrame, RawFrame};

/// x264 encoder thread entry point.
///
/// Runs on a dedicated OS thread. Errors are logged and the thread exits.
pub fn thread(
    raw_rx: CbReceiver<RawFrame>,
    frame_tx: broadcast::Sender<EncodedFrame>,
    keyframe_rx: CbReceiver<()>,
    bitrate: u32,
    encoder_threads: u32,
    width: u32,
    height: u32,
    fps: u32,
) {
    if let Err(e) = run_encoder(raw_rx, frame_tx, keyframe_rx, bitrate, encoder_threads, width, height, fps) {
        error!("encoder thread died: {e:#}");
    }
}

/// appsrc → x264enc → appsink (byte-stream H.264 NAL units)
fn run_encoder(
    raw_rx: CbReceiver<RawFrame>,
    frame_tx: broadcast::Sender<EncodedFrame>,
    keyframe_rx: CbReceiver<()>,
    bitrate: u32,
    encoder_threads: u32,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<()> {
    gst::init()?;

    let frame_duration_ns = 1_000_000_000u64 / fps as u64;

    let enc_pipeline = gst::parse::launch(&format!(
        "appsrc name=src format=time is-live=true \
         ! video/x-raw,format=I420,width={width},height={height},framerate={fps}/1 \
         ! x264enc tune=zerolatency bitrate={bitrate} key-int-max=60 threads={encoder_threads} \
         ! video/x-h264,profile=baseline,stream-format=byte-stream \
         ! appsink name=enc_sink emit-signals=true max-buffers=4 drop=true sync=false"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("launch did not return a Pipeline"))?;

    let src = enc_pipeline
        .by_name("src")
        .context("appsrc 'src' not found")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| anyhow::anyhow!("'src' element is not an AppSrc"))?;

    let enc_sink = enc_pipeline
        .by_name("enc_sink")
        .context("appsink 'enc_sink' not found")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow::anyhow!("'enc_sink' element is not an AppSink"))?;

    enc_pipeline.set_state(gst::State::Playing)?;
    info!("encoder pipeline playing ({width}x{height}@{fps} {bitrate}kbps)");

    // Pull encoded frames on a dedicated thread so the appsrc push loop is
    // never blocked by broadcast receivers.
    let frame_tx_clone = frame_tx.clone();
    let sink_handle = std::thread::Builder::new()
        .name("enc-sink".into())
        .spawn(move || {
            // Track last PTS as Option so the first frame gets a correct
            // frame_duration rather than an inflated (pts - ZERO) value.
            let mut last_pts: Option<Duration> = None;
            let frame_duration = Duration::from_nanos(frame_duration_ns);
            let mut fps_frames = 0u64;
            let mut fps_timer = std::time::Instant::now();

            loop {
                match enc_sink.pull_sample() {
                    Ok(sample) => {
                        let Some(buffer) = sample.buffer() else { continue };
                        let Ok(map) = buffer.map_readable() else { continue };

                        let pts = buffer
                            .pts()
                            .map(|t| Duration::from_nanos(t.nseconds()))
                            .unwrap_or_default();

                        let duration = match last_pts {
                            Some(prev) if pts > prev => pts - prev,
                            _ => frame_duration,
                        };
                        last_pts = Some(pts);

                        let encoded = EncodedFrame {
                            data: bytes::Bytes::copy_from_slice(map.as_slice()),
                            duration,
                        };

                        // No receivers yet is fine — broadcast discards the frame.
                        let _ = frame_tx_clone.send(encoded);

                        crate::metrics::record_frame_encoded();
                        fps_frames += 1;
                        let elapsed = fps_timer.elapsed();
                        if elapsed >= Duration::from_secs(10) {
                            let fps = fps_frames as f64 / elapsed.as_secs_f64();
                            tracing::debug!(
                                "encoder: {fps:.1} fps ({fps_frames} frames in {elapsed:.1?})"
                            );
                            fps_frames = 0;
                            fps_timer = std::time::Instant::now();
                        }
                    }
                    Err(_) => {
                        info!("enc-sink: EOS or pipeline stopped");
                        break;
                    }
                }
            }
        })?;

    // Push raw I420 frames into the encoder.
    let mut seq = 0u64;
    for frame in raw_rx.iter() {
        let mut buffer = gst::Buffer::with_size(frame.data.len())
            .map_err(|_| anyhow::anyhow!("GStreamer buffer allocation failed"))?;

        {
            let buf_ref = buffer.get_mut().unwrap();
            let pts_ns = frame.pts.as_nanos() as u64;
            buf_ref.set_pts(gst::ClockTime::from_nseconds(pts_ns));
            buf_ref.set_dts(gst::ClockTime::from_nseconds(pts_ns));
            buf_ref.set_duration(gst::ClockTime::from_nseconds(frame_duration_ns));
            let mut map = buf_ref.map_writable().unwrap();
            map.copy_from_slice(&frame.data);
        }

        // Drain all pending keyframe requests (one event per peer join).
        // A single GstForceKeyUnit covers all of them for this frame.
        let mut force_keyframe = false;
        while keyframe_rx.try_recv().is_ok() {
            force_keyframe = true;
        }
        if force_keyframe {
            let s = gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build();
            src.send_event(gst::event::CustomDownstream::builder(s).build());
            info!("forced keyframe (seq {seq})");
        }

        if src.push_buffer(buffer) != Ok(gst::FlowSuccess::Ok) {
            warn!("encoder appsrc push failed at seq {seq}");
        }
        seq += 1;
    }

    // Signal EOS so x264enc flushes and the sink thread exits cleanly.
    let _ = src.end_of_stream();

    if sink_handle.join().is_err() {
        warn!("enc-sink thread panicked");
    }

    enc_pipeline.set_state(gst::State::Null)?;
    Ok(())
}
