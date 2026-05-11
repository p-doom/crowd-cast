//! Pre-signed URL upload implementation
//!
//! Supports streaming uploads to minimize RAM usage for large video files.

use anyhow::{Context, Result};
use reqwest::{Body, Client};
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, warn};

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::auth::AuthManager;
use crate::config::Config;
use crate::data::CompletedChunk;

/// Request to Lambda endpoint for pre-signed URLs
#[derive(Debug, Serialize)]
struct PresignRequest {
    #[serde(rename = "fileName")]
    file_name: String,
    version: String,
    #[serde(rename = "userId")]
    user_id: String,
}

/// Response from Lambda endpoint with pre-signed URLs
#[derive(Debug, Deserialize)]
struct PresignResponse {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
    key: String,
    #[serde(rename = "contentType")]
    content_type: String,
}

/// Uploader for completed chunks
///
/// Uses streaming uploads to avoid loading entire video files into RAM.
#[derive(Clone)]
pub struct Uploader {
    client: Client,
    auth: Option<Arc<Mutex<AuthManager>>>,
}

impl Uploader {
    /// Create a new uploader
    pub fn new(config: &Config, auth: Option<Arc<Mutex<AuthManager>>>) -> Self {
        let _ = config;
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            auth,
        }
    }

    /// Get a valid auth token if authenticated, or None.
    async fn get_auth_token(&self) -> Option<String> {
        let auth = self.auth.as_ref()?;
        let mut mgr = auth.lock().await;
        mgr.get_valid_token().await
    }

    fn compile_time_endpoint() -> Option<&'static str> {
        option_env!("CROWD_CAST_API_GATEWAY_URL")
    }

    fn user_id_path() -> Option<std::path::PathBuf> {
        directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .map(|p| p.data_dir().join("user_id"))
    }

    fn compute_user_id() -> String {
        if let Some(path) = Self::user_id_path() {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let id = contents.trim().to_string();
                if !id.is_empty() {
                    return id;
                }
            }

            let id = uuid::Uuid::new_v4().to_string();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, &id) {
                warn!("Failed to persist user ID to {:?}: {}", path, e);
            }
            return id;
        }

        uuid::Uuid::new_v4().to_string()
    }

    async fn request_presigned_url(
        &self,
        endpoint: &str,
        file_name: &str,
        version: &str,
        user_id: &str,
        auth_token: Option<&str>,
    ) -> Result<PresignResponse> {
        let presign_request = PresignRequest {
            file_name: file_name.to_string(),
            version: version.to_string(),
            user_id: user_id.to_string(),
        };

        let mut req = self
            .client
            .post(endpoint)
            .json(&presign_request)
            .timeout(std::time::Duration::from_secs(30));

        if let Some(token) = auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }

        let presign_response: PresignResponse = req
            .send()
            .await
            .context("Failed to request pre-signed URL")?
            .json()
            .await
            .context("Failed to parse pre-signed URL response")?;

        Ok(presign_response)
    }

    /// Upload a completed chunk using streaming for video files
    ///
    /// This method streams video files directly from disk to the network,
    /// avoiding the need to load the entire file into RAM. This is critical
    /// for segments that can be several hundred MB.
    pub async fn upload(&self, chunk: &CompletedChunk) -> Result<()> {
        let endpoint = Self::compile_time_endpoint()
            .context("Lambda endpoint not configured at compile time")?;

        info!(
            "Uploading chunk {} for session {}",
            chunk.chunk_id, chunk.session_id
        );

        let version = option_env!("CARGO_PKG_VERSION").unwrap_or("0.0.1");
        let user_id = Self::compute_user_id();
        let auth_token = self.get_auth_token().await;
        let auth_token_ref = auth_token.as_deref();

        // 1. Get pre-signed URL for video (if path is available)
        let mut video_presign: Option<PresignResponse> = None;
        let mut video_file_name: Option<String> = None;

        if let Some(ref video_path) = chunk.video_path {
            let video_file = video_path
                .file_name()
                .and_then(|name| name.to_str())
                .context("Failed to get video filename")?;
            let file_name = format!("recordings/{}", video_file);
            let presign_response = self
                .request_presigned_url(endpoint, &file_name, version, &user_id, auth_token_ref)
                .await?;
            debug!(
                "Got pre-signed URL for video chunk {} (key: {})",
                chunk.chunk_id, presign_response.key
            );
            video_presign = Some(presign_response);
            video_file_name = Some(file_name);
        }

        // 2. Get pre-signed URL for keylogs
        let keylog_file_name = format!("keylogs/input_{}.msgpack", chunk.chunk_id);
        let keylog_presign = self
            .request_presigned_url(endpoint, &keylog_file_name, version, &user_id, auth_token_ref)
            .await?;
        debug!(
            "Got pre-signed URL for keylogs chunk {} (key: {})",
            chunk.chunk_id, keylog_presign.key
        );

        // 3. Upload video file using streaming (if path is available)
        if let Some(ref video_path) = chunk.video_path {
            let presign = video_presign
                .as_ref()
                .context("Missing video pre-signed URL")?;

            // Get file size for Content-Length header
            let metadata = tokio::fs::metadata(video_path)
                .await
                .with_context(|| format!("Failed to get video file metadata: {:?}", video_path))?;
            let file_size = metadata.len();

            // Open file and create streaming body
            let file = File::open(video_path)
                .await
                .with_context(|| format!("Failed to open video file: {:?}", video_path))?;

            // Use ReaderStream to stream the file without loading it all into RAM
            let stream = ReaderStream::new(file);
            let body = Body::wrap_stream(stream);

            let content_type = if presign.content_type.is_empty() {
                "video/mp4"
            } else {
                presign.content_type.as_str()
            };

            let response = self
                .client
                .put(&presign.upload_url)
                .header("Content-Type", content_type)
                .header("Content-Length", file_size)
                .timeout(std::time::Duration::from_secs(600))
                .body(body)
                .send()
                .await
                .context("Failed to send video upload request")?;

            if !response.status().is_success() {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();
                let preview = &body_text[..body_text.len().min(500)];
                error!(
                    "Video upload failed for chunk {}: HTTP {} — {}",
                    chunk.chunk_id, status, preview
                );
                anyhow::bail!("Video upload returned HTTP {}", status);
            }

            info!(
                "Uploaded video for chunk {} ({:.2} MB)",
                chunk.chunk_id,
                file_size as f64 / (1024.0 * 1024.0)
            );
        }

        // 4. Upload input log (small enough to fit in RAM)
        let input_bytes =
            rmp_serde::to_vec(&chunk.events).context("Failed to serialize input events")?;

        let keylog_content_type = if keylog_presign.content_type.is_empty() {
            "application/msgpack"
        } else {
            keylog_presign.content_type.as_str()
        };

        let response = self
            .client
            .put(&keylog_presign.upload_url)
            .header("Content-Type", keylog_content_type)
            .timeout(std::time::Duration::from_secs(30))
            .body(input_bytes)
            .send()
            .await
            .context("Failed to send keylog upload request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            let preview = &body_text[..body_text.len().min(500)];
            error!(
                "Keylog upload failed for chunk {}: HTTP {} — {}",
                chunk.chunk_id, status, preview
            );
            anyhow::bail!("Keylog upload returned HTTP {}", status);
        }

        info!(
            "Uploaded input log for chunk {} ({} events)",
            chunk.chunk_id,
            chunk.events.len()
        );

        if let Some(file_name) = video_file_name {
            debug!("Uploaded video file: {}", file_name);
        }
        debug!("Uploaded keylog file: {}", keylog_file_name);

        Ok(())
    }

    /// Check if uploader is configured
    pub fn is_configured(&self) -> bool {
        Self::compile_time_endpoint().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presign_request_serialization() {
        let request = PresignRequest {
            file_name: "recordings/test.mp4".to_string(),
            version: "0.0.1".to_string(),
            user_id: "test-user".to_string(),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("recordings/test.mp4"));
        assert!(json.contains("0.0.1"));
        assert!(json.contains("test-user"));
    }
}
