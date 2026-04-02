use anyhow::{Context, Result};
use crossbeam_channel::Sender as CbSender;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::types::RawFrame;

/// GStreamer capture thread entry point.
///
/// Runs on a dedicated OS thread. Errors are logged; the thread exits so the
/// watchdog loop in main can restart it.
pub fn thread(raw_tx: CbSender<RawFrame>, device_index: u32, width: u32, height: u32, fps: u32) {
    if let Err(e) = run_pipeline(raw_tx, device_index, width, height, fps) {
        error!("capture thread died: {e:#}");
    }
}

/// mfvideosrc (NV12) → videoconvert → I420 → appsink
fn run_pipeline(
    raw_tx: CbSender<RawFrame>,
    device_index: u32,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<()> {
    gst::init()?;

    let pipeline_str = format!(
        "mfvideosrc device-index={device_index} \
         ! video/x-raw,format=NV12,width={width},height={height},framerate={fps}/1 \
         ! videoconvert \
         ! video/x-raw,format=I420 \
         ! appsink name=sink emit-signals=true max-buffers=2 drop=true sync=false"
    );

    let pipeline = gst::parse::launch(&pipeline_str)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("launch did not return a Pipeline"))?;

    let sink = pipeline
        .by_name("sink")
        .context("appsink 'sink' not found")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow::anyhow!("'sink' element is not an AppSink"))?;

    // appsink callback — runs on the GStreamer streaming thread.
    // Keep it minimal: copy bytes, try_send, return.
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = match sink.pull_sample() {
                    Ok(s) => s,
                    Err(_) => return Err(gst::FlowError::Eos),
                };

                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let pts = buffer
                    .pts()
                    .map(|t| Duration::from_nanos(t.nseconds()))
                    .unwrap_or_default();

                let frame = RawFrame {
                    data: bytes::Bytes::copy_from_slice(map.as_slice()),
                    pts,
                };

                // bounded(2): drop frame if encoder is behind rather than
                // blocking the GStreamer thread.
                match raw_tx.try_send(frame) {
                    Ok(_) => {}
                    Err(crossbeam_channel::TrySendError::Full(_)) => {
                        warn!("encoder busy — dropping capture frame");
                        crate::metrics::record_encoder_drop();
                    }
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        return Err(gst::FlowError::Eos);
                    }
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    let bus = pipeline.bus().context("pipeline has no bus")?;

    match pipeline.set_state(gst::State::Playing) {
        Ok(_) => info!("capture pipeline playing (device={device_index} {width}x{height}@{fps})"),
        Err(e) => {
            error!("failed to start capture pipeline: {e:?}");
            for msg in bus.iter_timed(gst::ClockTime::from_seconds(1)) {
                if let gst::MessageView::Error(e) = msg.view() {
                    error!("GST error: {} — {:?}", e.error(), e.debug());
                }
            }
            return Err(anyhow::anyhow!("capture pipeline state change failed"));
        }
    }

    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(..) => {
                info!("capture: EOS");
                break;
            }
            MessageView::Error(e) => {
                error!("capture error: {} — {:?}", e.error(), e.debug());
                break;
            }
            MessageView::Warning(w) => {
                warn!("capture warning: {}", w.error());
            }
            MessageView::StateChanged(s)
                if msg.src().as_ref() == Some(&pipeline.upcast_ref::<gst::Object>()) =>
            {
                info!("capture pipeline: {:?} → {:?}", s.old(), s.current());
            }
            _ => {}
        }
    }

    pipeline.set_state(gst::State::Null)?;
    Ok(())
}
