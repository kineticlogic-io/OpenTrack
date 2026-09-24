//! Client for the peat-node sidecar (`peat.sidecar.v1.PeatSidecar`).
//!
//! Documents are JSON blobs in named collections, synced across the mesh by
//! CRDT. OpenTrack only needs document put/get/delete and node status.

use std::future::Future;
use std::time::Duration;

use tonic::transport::{Channel, Endpoint};

#[allow(clippy::all, clippy::pedantic)]
pub mod pb {
    tonic::include_proto!("peat.sidecar.v1");
}

use pb::peat_sidecar_client::PeatSidecarClient;

#[derive(Debug, thiserror::Error)]
pub enum PeatError {
    #[error("invalid peat-node address {0:?}: {1}")]
    Address(String, String),
    #[error("peat-node {op} {collection}/{doc_id} failed: {status}")]
    Rpc {
        op: &'static str,
        collection: String,
        doc_id: String,
        status: Box<tonic::Status>,
    },
    #[error("peat-node status failed: {0}")]
    Status(Box<tonic::Status>),
}

impl PeatError {
    /// Whether retrying could help. Validation failures (the node rejecting a
    /// document) are permanent; connectivity and overload are not.
    pub fn is_transient(&self) -> bool {
        let code = match self {
            PeatError::Rpc { status, .. } | PeatError::Status(status) => status.code(),
            PeatError::Address(..) => return false,
        };
        matches!(
            code,
            tonic::Code::Unavailable
                | tonic::Code::DeadlineExceeded
                | tonic::Code::ResourceExhausted
                | tonic::Code::Aborted
                | tonic::Code::Unknown
                // tonic reports its own client-side deadline as Cancelled.
                | tonic::Code::Cancelled
        )
    }
}

/// Where documents go. The peat writer is generic over this so it can be
/// tested without a live node.
pub trait DocumentSink: Send + Sync {
    fn put(
        &self,
        collection: &str,
        doc_id: &str,
        json: &str,
    ) -> impl Future<Output = Result<(), PeatError>> + Send;

    fn delete(
        &self,
        collection: &str,
        doc_id: &str,
    ) -> impl Future<Output = Result<(), PeatError>> + Send;
}

#[derive(Debug, Clone)]
pub struct NodeStatus {
    pub node_id: String,
    pub endpoint_addr: String,
    pub sync_active: bool,
    pub connected_peers: u32,
}

#[derive(Clone)]
pub struct PeatClient {
    client: PeatSidecarClient<Channel>,
    rpc_timeout: Duration,
    retries: u32,
}

impl PeatClient {
    /// Create a client that dials lazily: it never fails at startup because
    /// the sidecar is slow to boot; individual calls retry instead.
    pub fn connect_lazy(addr: &str) -> Result<Self, PeatError> {
        let uri = if addr.contains("://") {
            addr.to_owned()
        } else {
            format!("http://{addr}")
        };
        let endpoint = Endpoint::from_shared(uri.clone())
            .map_err(|e| PeatError::Address(uri.clone(), e.to_string()))?
            .connect_timeout(Duration::from_secs(5))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_while_idle(true);
        Ok(Self {
            client: PeatSidecarClient::new(endpoint.connect_lazy()),
            rpc_timeout: Duration::from_secs(5),
            retries: 3,
        })
    }

    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    pub async fn status(&self) -> Result<NodeStatus, PeatError> {
        let mut req = tonic::Request::new(pb::GetStatusRequest {});
        req.set_timeout(self.rpc_timeout);
        let s = self
            .client
            .clone()
            .get_status(req)
            .await
            .map_err(|s| PeatError::Status(Box::new(s)))?
            .into_inner();
        Ok(NodeStatus {
            node_id: s.node_id,
            endpoint_addr: s.endpoint_addr,
            sync_active: s.sync_active,
            connected_peers: s.connected_peers,
        })
    }

    pub async fn get(&self, collection: &str, doc_id: &str) -> Result<Option<String>, PeatError> {
        self.retry("get", collection, doc_id, || {
            let mut client = self.client.clone();
            let mut req = tonic::Request::new(pb::GetDocumentRequest {
                collection: collection.to_owned(),
                doc_id: doc_id.to_owned(),
            });
            req.set_timeout(self.rpc_timeout);
            async move {
                client
                    .get_document(req)
                    .await
                    .map(|r| r.into_inner().json_data)
            }
        })
        .await
    }

    async fn retry<T, F, Fut>(
        &self,
        op: &'static str,
        collection: &str,
        doc_id: &str,
        mut call: F,
    ) -> Result<T, PeatError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, tonic::Status>>,
    {
        let mut attempt = 0;
        loop {
            match call().await {
                Ok(v) => return Ok(v),
                Err(status) => {
                    let err = PeatError::Rpc {
                        op,
                        collection: collection.to_owned(),
                        doc_id: doc_id.to_owned(),
                        status: Box::new(status),
                    };
                    if attempt >= self.retries || !err.is_transient() {
                        return Err(err);
                    }
                    let delay = Duration::from_millis(250 * 2u64.pow(attempt));
                    tracing::warn!(%err, attempt = attempt + 1, ?delay, "retrying");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }
}

impl DocumentSink for PeatClient {
    async fn put(&self, collection: &str, doc_id: &str, json: &str) -> Result<(), PeatError> {
        self.retry("put", collection, doc_id, || {
            let mut client = self.client.clone();
            let mut req = tonic::Request::new(pb::PutDocumentRequest {
                collection: collection.to_owned(),
                doc_id: doc_id.to_owned(),
                json_data: json.to_owned(),
            });
            req.set_timeout(self.rpc_timeout);
            async move { client.put_document(req).await.map(|_| ()) }
        })
        .await
    }

    /// Delete a document.
    ///
    /// Observed against peat-node (2026-09-24): `DeleteDocument` applies the
    /// delete but never sends its response, so every call hits its deadline.
    /// A timed-out delete is therefore confirmed with `GetDocument`: if the
    /// document is gone the delete succeeded; otherwise the timeout stands
    /// and the caller retries.
    async fn delete(&self, collection: &str, doc_id: &str) -> Result<(), PeatError> {
        let mut client = self.client.clone();
        let mut req = tonic::Request::new(pb::DeleteDocumentRequest {
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
        });
        req.set_timeout(self.rpc_timeout);
        let status = match client.delete_document(req).await {
            Ok(_) => return Ok(()),
            Err(status) => status,
        };
        let err = PeatError::Rpc {
            op: "delete",
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
            status: Box::new(status),
        };
        if !err.is_transient() {
            return Err(err);
        }
        match self.get(collection, doc_id).await {
            Ok(None) => {
                tracing::debug!(%collection, %doc_id, "delete unanswered but confirmed by read-back");
                Ok(())
            }
            _ => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_errors_are_permanent() {
        let err = |code| PeatError::Rpc {
            op: "put",
            collection: "tracks".into(),
            doc_id: "x".into(),
            status: Box::new(tonic::Status::new(code, "")),
        };
        assert!(err(tonic::Code::Unavailable).is_transient());
        assert!(!err(tonic::Code::InvalidArgument).is_transient());
    }

    #[tokio::test]
    async fn lazy_connect_accepts_bare_host_port() {
        assert!(PeatClient::connect_lazy("127.0.0.1:50051").is_ok());
        assert!(PeatClient::connect_lazy("http://127.0.0.1:50051").is_ok());
    }
}
