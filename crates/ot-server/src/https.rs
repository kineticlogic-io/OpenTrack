//! Serving the control plane over TLS (`OT_TLS_CERT`, `OT_TLS_KEY`). With
//! `OT_TLS_CLIENT_CA`, clients may also present a certificate that CA
//! signed; its common name then signs the caller in as the account the
//! security settings map it to (browsers, without one, sign in as usual).

use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::auth::PeerCert;

/// The TLS acceptor new connections use; replaced when the client CRLs
/// change, without dropping connections already made.
#[derive(Clone)]
pub struct Acceptor(Arc<RwLock<tokio_rustls::TlsAcceptor>>);

impl Acceptor {
    pub fn new(a: tokio_rustls::TlsAcceptor) -> Self {
        Self(Arc::new(RwLock::new(a)))
    }

    fn current(&self) -> tokio_rustls::TlsAcceptor {
        self.0
            .read()
            .map_or_else(|e| e.into_inner().clone(), |a| a.clone())
    }

    fn set(&self, a: tokio_rustls::TlsAcceptor) {
        match self.0.write() {
            Ok(mut g) => *g = a,
            Err(e) => *e.into_inner() = a,
        }
    }
}

/// Rebuild the acceptor whenever a client CRL file changes (checked each
/// minute). A list that fails to load leaves the previous one in force.
pub async fn reload_on_crl_change(tls: ot_source::tls::ServerTls, acceptor: Acceptor) {
    let mut stamp = tls.crl_stamp();
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.tick().await;
    loop {
        tick.tick().await;
        let now = tls.crl_stamp();
        if now == stamp {
            continue;
        }
        match tls.acceptor() {
            Ok(a) => {
                acceptor.set(a);
                stamp = now;
                tracing::info!("client certificate revocation lists reloaded");
            }
            Err(e) => {
                tracing::error!(
                    error = format!("{e:#}"),
                    "client CRLs changed but did not load; the previous ones stay"
                );
                stamp = now;
            }
        }
    }
}

pub async fn serve(
    listener: TcpListener,
    app: Router,
    acceptor: Acceptor,
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
        let (acceptor, app) = (acceptor.current(), app.clone());
        tokio::spawn(async move {
            let tls =
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(t)) => t,
                    Ok(Err(e)) => {
                        // A refused client certificate is worth an operator's eye.
                        let text = e.to_string();
                        if text.contains("certificate") {
                            tracing::warn!(%peer, error = %text, "client certificate refused");
                        } else {
                            tracing::debug!(%peer, error = %text, "TLS handshake failed");
                        }
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
