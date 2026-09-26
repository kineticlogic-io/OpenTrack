//! Serving the control plane over TLS (`OT_TLS_CERT`, `OT_TLS_KEY`). With
//! `OT_TLS_CLIENT_CA`, clients may also present a certificate that CA
//! signed; its common name then signs the caller in as the account the
//! security settings map it to (browsers, without one, sign in as usual).

use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::auth::PeerCert;

pub async fn serve(
    listener: TcpListener,
    app: Router,
    acceptor: tokio_rustls::TlsAcceptor,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    tokio::pin!(shutdown);
    loop {
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => return Ok(()),
        };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        tokio::spawn(async move {
            let tls =
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(t)) => t,
                    Ok(Err(e)) => {
                        tracing::debug!(%peer, error = %e, "TLS handshake failed");
                        return;
                    }
                    Err(_) => return,
                };
            let cert = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .and_then(|c| ot_source::tls::common_name(c))
                .map(|common_name| PeerCert { common_name });
            let svc = app.map_request(move |mut r: Request<hyper::body::Incoming>| {
                r.extensions_mut().insert(ConnectInfo(peer));
                if let Some(c) = &cert {
                    r.extensions_mut().insert(c.clone());
                }
                r
            });
            let io = TokioIo::new(tls);
            if let Err(e) = Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, TowerToHyperService::new(svc))
                .await
            {
                tracing::debug!(%peer, error = %e, "connection ended");
            }
        });
    }
}
