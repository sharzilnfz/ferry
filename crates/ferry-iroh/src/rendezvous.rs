pub use ferry_rendezvous::{
    bind_discovery_socket, client_discover_and_join, recv_frame, send_frame,
    service_name_for_code, topic_for_code, PairingServerHandle, DISCOVERY_PORT, MAX_PAIRING_FRAME_LEN,
    MULTICAST_ADDR, PAIRING_TOPIC_KEY,
};

pub fn start_pairing_server<F>(
    code: String,
    offer_bytes: Vec<u8>,
    expires_at: std::time::SystemTime,
    on_response: F,
) -> std::io::Result<PairingServerHandle>
where
    F: FnOnce(Vec<u8>) -> std::io::Result<Vec<u8>> + Send + 'static,
{
    ferry_rendezvous::start_pairing_server(code, offer_bytes, expires_at, on_response)
}
