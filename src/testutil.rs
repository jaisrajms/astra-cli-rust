//! Test-only helpers: spawn an in-process mock tonic server and hand back a
//! dialable `http://` endpoint for the CLI's channel builder.

use std::convert::Infallible;
use std::net::SocketAddr;

use hyper::{Request, Response};
use tonic::body::BoxBody;
use tonic::server::NamedService;
use tonic::transport::Server;
use tower::Service;

/// Bind an ephemeral port and serve `service` in the background, returning the
/// address to connect to.
pub async fn spawn<S>(service: S) -> SocketAddr
where
    S: Service<Request<BoxBody>, Response = Response<BoxBody>, Error = Infallible>
        + NamedService
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut builder = Server::builder();
    let router = builder.add_service(service);
    tokio::spawn(async move {
        router.serve(addr).await.unwrap();
    });
    addr
}

/// Convenience: connect a channel to an address returned by [`spawn`].
pub fn channel(addr: SocketAddr) -> tonic::transport::Channel {
    crate::endpoint::connect(&format!("http://{addr}")).expect("connect to mock server")
}
