//! `resolve_now` uses the process-wide active client and network state, so it gets its own test binary.

use std::sync::Arc;

use nori_core::client::{Client, NetProfile};
use nori_core::stream::{network_metered, resolve_now};
use nori_core::transport::{Exchange, Transport, TransportError, TransportResponse};
use nori_core::Core;

/// A transport that is never needed.
struct NoApi;

#[async_trait::async_trait]
impl Transport for NoApi {
    async fn get(&self, _url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    async fn send(&self, _request: Exchange) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    fn address_changed(&self) {}
}

#[test]
fn resolve_follows_client_and_net() {
    assert!(resolve_now("s1").is_none(), "no client yet");
    let core = Core::new(String::new(), "t".into()).unwrap();
    let client = Client::new(core, Arc::new(NoApi));
    client.set_profile(NetProfile { url: "h".into(), ..Default::default() });
    // A distinct mobile quality shows which network was used.
    let dir = nori_testdir::TempDir::new("active-client");
    nori_core::settings_store::settings_open(dir.join("app.db").to_string_lossy().into_owned()).unwrap();
    nori_core::settings_store::shared().edit_by_name("mobile", "192:opus");
    network_metered(true);
    let metered = resolve_now("s1").expect("the client just made");
    network_metered(false);
    let wifi = resolve_now("s1").expect("the client just made");
    assert_eq!((metered.key.as_str(), wifi.key.as_str()), ("s1:192opus", "s1:0"));
    drop(client);
    assert!(resolve_now("s1").is_none(), "client dropped");
}
