use anyhow::Result;
use std::sync::Arc;
use webrtc::{
    ice_transport::ice_server::RTCIceServer,
    peer_connection::{configuration::RTCConfiguration, RTCPeerConnection},
};

use crate::types::AppState;

/// Create a new peer connection configured with the ICE servers and WebRTC API
/// stored in `state`.
pub async fn setup(state: &AppState) -> Result<Arc<RTCPeerConnection>> {
    let mut rtc_ice_servers = vec![];

    for stun_url in &state.ice_servers.stun {
        rtc_ice_servers.push(RTCIceServer {
            urls: vec![stun_url.clone()],
            ..Default::default()
        });
    }

    for (url, username, password) in &state.ice_servers.turn {
        rtc_ice_servers.push(RTCIceServer {
            urls:       vec![url.clone()],
            username:   username.clone(),
            credential: password.clone(),
            ..Default::default()
        });
    }

    let config = RTCConfiguration {
        ice_servers: rtc_ice_servers,
        ..Default::default()
    };

    Ok(Arc::new(state.webrtc_api.new_peer_connection(config).await?))
}
