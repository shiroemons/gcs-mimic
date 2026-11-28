//! The transport: the listeners, the connections they accept, and the routing that hands each
//! request to the API that serves it.
//!
//! [`accept`] runs the accept loop and starts a task for every connection. [`connection`] serves
//! an HTTP/1 connection and [`grpc_listener`] a gRPC one. [`deadline`] holds the limits on each
//! phase of a connection and the replies once they expire. [`slots`] hands a full server's idle
//! connection slots to waiting clients, and [`probe`] answers the health probes. [`routing`]
//! decides which API answers a request.

use crate::{config::Config, service::state::State};
use slots::SlotReclaim;
use std::sync::Arc;

mod accept;
mod admin;
mod batch;
mod browser;
mod connection;
mod deadline;
// Crate-visible for the test helper that the gRPC handler tests reset a panicking stream with.
pub(crate) mod grpc_listener;
mod handshake;
mod host;
mod ingress;
mod probe;
// Crate-visible for the routed test driver that the JSON handler tests send requests through.
pub(crate) mod routing;
mod slots;
mod transport;
#[cfg(unix)]
pub(crate) mod unix;

pub(crate) use accept::GrpcListener;
pub use accept::{Listeners, Stop, TcpListeners, serve};

/// What every connection task shares with the server that accepted it.
#[derive(Clone)]
struct Shared {
    state: Arc<State>,
    config: Arc<Config>,
    slots: Arc<SlotReclaim>,
}

#[cfg(test)]
mod catalog_wait_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod xml_read_tests;
