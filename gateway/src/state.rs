use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::info;

use crate::events::MarketEvent;

pub type ClientId = u64;

#[derive(Clone)]
pub struct GatewayState {
    next_client_id: Arc<RwLock<ClientId>>,
    event_tx: broadcast::Sender<MarketEvent>,
}

impl GatewayState {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(10000);
        Self {
            next_client_id: Arc::new(RwLock::new(1)),
            event_tx,
        }
    }

    pub async fn add_client(&self) -> (ClientId, broadcast::Receiver<MarketEvent>) {
        let mut id = self.next_client_id.write().await;
        let client_id = *id;
        *id += 1;

        info!("Client {} connected", client_id);
        (client_id, self.event_tx.subscribe())
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<MarketEvent> {
        self.event_tx.subscribe()
    }

    pub fn publish_event(&self, event: MarketEvent) {
        let _ = self.event_tx.send(event);
    }
}
