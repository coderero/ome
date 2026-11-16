use ome_protocol::ClientMessage;
use ome_ui_data::{BrokerLink, Credentials, LinkUpdate, OrderEntry, OrderRequest};
use std::sync::mpsc::{self, Receiver};

pub trait Session {
    fn submit(&self, request: OrderRequest);
    fn query(&self, message: ClientMessage);
}

pub struct Connection {
    pub session: Box<dyn Session>,
    pub updates: Receiver<LinkUpdate>,
}

pub type Connector = Box<dyn Fn(Credentials) -> Connection>;

impl Session for BrokerLink {
    fn submit(&self, request: OrderRequest) {
        OrderEntry::submit(self, request);
    }

    fn query(&self, message: ClientMessage) {
        BrokerLink::query(self, message);
    }
}

pub fn broker() -> Connector {
    Box::new(|credentials| {
        let (tx, updates) = mpsc::channel();
        Connection {
            session: Box::new(BrokerLink::start(credentials, tx)),
            updates,
        }
    })
}
