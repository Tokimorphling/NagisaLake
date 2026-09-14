//! Client for SGLang-compatible diffusion HTTP APIs.
//!
//! The worker submits an allowlist-rendered JSON body to one endpoint and
//! receives images back. The wire contract follows the OpenAI images schema
//! that SGLang and compatible servers expose: `POST {base_url}{generate_path}`
//! returns `{"data": [{"b64_json" | "url"}, ...]}`. Templates may carry any
//! extra fields the target server understands (model, size, seed, steps, ...);
//! this client only fixes the transport, the envelope, and the output parsing.

use data_encoding::BASE64;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Connection settings for one SGLang-compatible diffusion server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SgLangConfig {
    /// Root of the diffusion server, without a trailing slash.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Path that accepts the generation request.
    #[serde(default = "default_generate_path")]
    pub generate_path: String,
    /// Optional bearer token for servers behind authentication.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whole-request ceiling. Diffusion renders can take minutes; this bounds
    /// a hung server rather than a slow one.
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

impl Default for SgLangConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            generate_path: default_generate_path(),
            api_key: None,
            timeout_seconds: default_timeout_seconds(),
        }
    }
}

fn default_base_url() -> String {
    "http://127.0.0.1:30000".into()
}

fn default_generate_path() -> String {
    "/v1/images/generations".into()
}

const fn default_timeout_seconds() -> u64 {
    600
}

/// One rendered image, already downloaded or decoded.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    /// Sniffed from magic bytes; defaults to `image/png`.
    pub content_type: &'static str,
}

#[derive(Debug, Error)]
pub enum SgLangError {
    #[error("invalid sg_lang configuration: {0}")]
    InvalidConfig(String),
    #[error("sg_lang request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("sg_lang returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("sg_lang response was not valid JSON: {0}")]
    InvalidResponse(#[from] serde_json::Error),
    #[error("sg_lang response carried no images")]
    EmptyResponse,
    #[error("sg_lang image exceeds the {limit} byte output limit")]
    ImageTooLarge { limit: u64 },
    #[error("base64 image payload was invalid: {0}")]
    InvalidBase64(#[from] data_encoding::DecodeError),
    #[error("cancelled")]
    Cancelled,
}

/// Bearer-authenticated HTTP client bound to one diffusion server.
#[derive(Clone)]
pub struct SgLangClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<String>,
}

impl std::fmt::Debug for SgLangClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SgLangClient")
            .field("url", &self.url)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

impl SgLangClient {
    pub fn new(config: &SgLangConfig) -> Result<Self, SgLangError> {
        let base_url = config.base_url.trim_end_matches('/');
        if base_url.is_empty() {
            return Err(SgLangError::InvalidConfig("base_url is empty".into()));
        }
        let generate_path = if config.generate_path.starts_with('/') {
            config.generate_path.clone()
        } else {
            format!("/{}", config.generate_path)
        };
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .build()
            .map_err(|error| SgLangError::InvalidConfig(error.to_string()))?;
        Ok(Self {
            http,
            url: format!("{base_url}{generate_path}"),
            api_key: config.api_key.clone().filter(|key| !key.is_empty()),
        })
    }

    /// Submits one rendered request body and collects every returned image.
    ///
    /// Each image is capped at `max_image_bytes` after decoding or download,
    /// matching the runtime's per-output artifact limit. Cancellation aborts
    /// the request immediately; dropping the in-flight HTTP future closes it.
    pub async fn generate(
        &self,
        body: JsonValue,
        max_image_bytes: u64,
        cancellation: &CancellationToken,
    ) -> Result<Vec<GeneratedImage>, SgLangError> {
        let mut request = self.http.post(&self.url).json(&body);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(SgLangError::Cancelled),
            response = request.send() => response?,
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SgLangError::Status {
                status: status.as_u16(),
                body: body.chars().take(512).collect(),
            });
        }
        let payload: GenerationResponse = response.json().await?;
        if payload.data.is_empty() {
            return Err(SgLangError::EmptyResponse);
        }
        let mut images = Vec::with_capacity(payload.data.len());
        for item in payload.data {
            let bytes = match item {
                DataItem::B64 { b64_json } => {
                    let decoded = BASE64.decode(b64_json.trim().as_bytes())?;
                    if decoded.len() as u64 > max_image_bytes {
                        return Err(SgLangError::ImageTooLarge {
                            limit: max_image_bytes,
                        });
                    }
                    decoded
                }
                DataItem::Url { url } => self.download(url, max_image_bytes, cancellation).await?,
            };
            let content_type = sniff_content_type(&bytes);
            tracing::debug!(bytes = bytes.len(), content_type, "decoded sg_lang image");
            images.push(GeneratedImage {
                bytes,
                content_type,
            });
        }
        Ok(images)
    }

    async fn download(
        &self,
        url: String,
        max_image_bytes: u64,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, SgLangError> {
        let mut response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(SgLangError::Cancelled),
            response = self.http.get(&url).send() => response?,
        };
        if !response.status().is_success() {
            return Err(SgLangError::Status {
                status: response.status().as_u16(),
                body: String::new(),
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(SgLangError::Cancelled),
            chunk = response.chunk() => chunk?,
        } {
            if bytes.len() as u64 + chunk.len() as u64 > max_image_bytes {
                return Err(SgLangError::ImageTooLarge {
                    limit: max_image_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(SgLangError::EmptyResponse);
        }
        Ok(bytes)
    }
}

/// Envelope shared by OpenAI-style image endpoints. Unknown fields are kept so
/// servers that attach extra metadata do not break parsing.
#[derive(Debug, Deserialize)]
struct GenerationResponse {
    #[serde(default)]
    data: Vec<DataItem>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DataItem {
    B64 { b64_json: String },
    Url { url: String },
}

fn sniff_content_type(bytes: &[u8]) -> &'static str {
    match bytes.first() {
        Some(0x89) if bytes.starts_with(&[0x89, b'P', b'N', b'G']) => "image/png",
        Some(0xFF) if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) => "image/jpeg",
        Some(0x47) if bytes.starts_with(b"GIF8") => "image/gif",
        Some(0x52) if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" => {
            "image/webp"
        }
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_targets_a_local_server() {
        let config = SgLangConfig::default();
        assert_eq!(config.base_url, "http://127.0.0.1:30000");
        assert_eq!(config.generate_path, "/v1/images/generations");
        assert!(config.api_key.is_none());
    }

    #[test]
    fn client_build_rejects_an_empty_base_url() {
        let config = SgLangConfig {
            base_url: String::new(),
            ..SgLangConfig::default()
        };
        assert!(matches!(
            SgLangClient::new(&config),
            Err(SgLangError::InvalidConfig(_))
        ));
    }

    #[test]
    fn trailing_slashes_are_normalized() {
        let config = SgLangConfig {
            base_url: "http://127.0.0.1:30000/".into(),
            generate_path: "v1/images/generations".into(),
            ..SgLangConfig::default()
        };
        assert_eq!(
            SgLangClient::new(&config).unwrap().url,
            "http://127.0.0.1:30000/v1/images/generations"
        );
    }

    #[test]
    fn image_magic_is_detected() {
        assert_eq!(sniff_content_type(b"\x89PNG\r\n"), "image/png");
        assert_eq!(sniff_content_type(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff_content_type(b"GIF89a"), "image/gif");
        assert_eq!(
            sniff_content_type(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            "image/webp"
        );
        assert_eq!(sniff_content_type(b"\x00\x00"), "image/png");
    }

    #[tokio::test]
    async fn cancelled_generation_returns_cancelled() {
        let client = SgLangClient::new(&SgLangConfig::default()).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = client
            .generate(serde_json::json!({"prompt": "test"}), 1024, &cancellation)
            .await;
        assert!(matches!(result, Err(SgLangError::Cancelled)));
    }
}
