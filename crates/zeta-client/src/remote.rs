//! `NodeClient`: a running node driven over its authenticated HTTP API.

use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder, Url};
use serde_json::Value;
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::stream::StreamInfo;
use zeta_core::types::StreamId;

/// Longer than the server's 30 s operation timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("node {url} unreachable: {detail}")]
    Unreachable { url: String, detail: String },
    #[error("node {url} rejected the secret")]
    Unauthorized { url: String },
    #[error("node {url} answered HTTP {status}: {body}")]
    Http {
        url: String,
        status: u16,
        body: String,
    },
    #[error("bad node url {0:?}: expected http://host:port")]
    BadUrl(String),
    #[error("decoding node response: {0}")]
    Decode(String),
}

#[derive(Clone, Debug)]
pub struct NodeClient {
    url: String,
    secret: String,
    http: Client,
}

impl NodeClient {
    pub fn new(url: &str, secret: &str) -> Result<Self, ClientError> {
        let bad = || ClientError::BadUrl(url.to_string());
        let base = url.strip_suffix('/').unwrap_or(url);
        let parsed = Url::parse(base).map_err(|_| bad())?;
        if parsed.scheme() != "http" || parsed.host_str().is_none() {
            return Err(bad());
        }
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ClientError::Unreachable {
                url: base.to_string(),
                detail: e.to_string(),
            })?;
        Ok(Self {
            url: base.to_string(),
            secret: secret.to_string(),
            http,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn apply(&self, op: &Op) -> Result<Result<OpResult, OpError>, ClientError> {
        let value = serde_json::to_value(op).map_err(|e| ClientError::Decode(e.to_string()))?;
        match self.apply_value(value).await? {
            Ok(v) => serde_json::from_value(v)
                .map(Ok)
                .map_err(|e| ClientError::Decode(e.to_string())),
            Err(v) => serde_json::from_value(v)
                .map(Err)
                .map_err(|e| ClientError::Decode(e.to_string())),
        }
    }

    pub async fn apply_value(&self, op: Value) -> Result<Result<Value, Value>, ClientError> {
        let req = self
            .http
            .request(Method::POST, self.endpoint("/op"))
            .json(&op);
        let (status, body) = self.send(req).await?;
        self.map_status(status, &body)
    }

    pub async fn schema(&self) -> Result<Value, ClientError> {
        self.get_json("/schema").await
    }

    pub async fn streams(&self) -> Result<Vec<StreamInfo>, ClientError> {
        let v = self.get_json("/streams").await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    pub async fn tail(
        &self,
        stream: &StreamId,
        max_frames: u32,
        timeout_ms: u32,
    ) -> Result<Result<Value, Value>, ClientError> {
        let mut url = Url::parse(&self.url).map_err(|_| ClientError::BadUrl(self.url.clone()))?;
        url.path_segments_mut()
            .map_err(|()| ClientError::BadUrl(self.url.clone()))?
            .extend(["streams", stream.0.as_str(), "tail"]);
        url.query_pairs_mut()
            .append_pair("max_frames", &max_frames.to_string())
            .append_pair("timeout_ms", &timeout_ms.to_string());
        let (status, body) = self.send(self.http.get(url)).await?;
        self.map_status(status, &body)
    }

    pub async fn agents_md(&self) -> Result<String, ClientError> {
        let (status, body) = self
            .send(self.http.get(self.endpoint("/agents-md")))
            .await?;
        self.require_ok(status, &body)?;
        String::from_utf8(body).map_err(|e| ClientError::Decode(e.to_string()))
    }

    pub async fn health(&self) -> Result<Value, ClientError> {
        self.get_json("/health").await
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    async fn get_json(&self, path: &str) -> Result<Value, ClientError> {
        let (status, body) = self.send(self.http.get(self.endpoint(path))).await?;
        self.require_ok(status, &body)?;
        serde_json::from_slice(&body).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Add the bearer header, perform the request, return status and body.
    async fn send(&self, req: RequestBuilder) -> Result<(u16, Vec<u8>), ClientError> {
        let unreachable = |e: reqwest::Error| ClientError::Unreachable {
            url: self.url.clone(),
            detail: e.to_string(),
        };
        let resp = req
            .bearer_auth(&self.secret)
            .send()
            .await
            .map_err(unreachable)?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(unreachable)?;
        Ok((status, body.to_vec()))
    }

    /// 2xx is the body, 401 is `Unauthorized`, any other status with a JSON
    /// `error` object is the operation error, anything else is `Http`.
    fn map_status(&self, status: u16, body: &[u8]) -> Result<Result<Value, Value>, ClientError> {
        if (200..300).contains(&status) {
            return serde_json::from_slice(body)
                .map(Ok)
                .map_err(|e| ClientError::Decode(e.to_string()));
        }
        if status == 401 {
            return Err(ClientError::Unauthorized {
                url: self.url.clone(),
            });
        }
        match serde_json::from_slice::<Value>(body) {
            Ok(v) if v.get("error").is_some() => Ok(Err(v)),
            _ => Err(self.http_error(status, body)),
        }
    }

    /// For endpoints with no operation-error channel: any non-2xx is an error.
    fn require_ok(&self, status: u16, body: &[u8]) -> Result<(), ClientError> {
        if (200..300).contains(&status) {
            Ok(())
        } else if status == 401 {
            Err(ClientError::Unauthorized {
                url: self.url.clone(),
            })
        } else {
            Err(self.http_error(status, body))
        }
    }

    fn http_error(&self, status: u16, body: &[u8]) -> ClientError {
        ClientError::Http {
            url: self.url.clone(),
            status,
            body: String::from_utf8_lossy(body).into_owned(),
        }
    }
}
