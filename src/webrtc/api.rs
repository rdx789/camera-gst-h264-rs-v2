use anyhow::Result;
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors,
        media_engine::{MediaEngine, MIME_TYPE_H264},
        APIBuilder,
    },
    interceptor::registry::Registry,
    rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType},
};

/// Build the WebRTC API with H.264 codec registration.
///
/// Created once in main and shared (via `Arc`) across all peer connections.
///
/// Profile-level-id `42e028`:
///   - `42` = Baseline profile
///   - `e0` = constraint flags (constraint_set0/1/2 all set, per Baseline spec)
///   - `28` = level 4.0 (hex 0x28 = decimal 40)
///
/// Level 4.0 supports up to 1920×1080@30fps. The previous `42001f`
/// (Baseline@3.1) capped at 1280×720 and caused browsers to refuse or
/// mishandle 1080p streams.
pub fn build_api() -> Result<webrtc::api::API> {
    let mut m = MediaEngine::default();
    m.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;\
                                 packetization-mode=1;\
                                 profile-level-id=42e028"
                    .to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 102,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?;

    Ok(APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build())
}
