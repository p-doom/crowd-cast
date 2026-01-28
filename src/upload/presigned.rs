//! Pre-signed URL upload implementation
//!
//! Supports streaming uploads to minimize RAM usage for large video files.

use anyhow::{Context, Result};
use reqwest::{Body, Client};
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tracing::{debug, info};

use crate::config::Config;
use crate::data::CompletedChunk;

/// Request to Lambda endpoint for pre-signed URLs
#[derive(Debug, Serialize)]
struct PresignRequest {
    session_id: String,
    chunk_id: String,
    video_content_type: String,
    input_content_type: String,
}

/// Response from Lambda endpoint with pre-signed URLs
#[derive(Debug, Deserialize)]
struct PresignResponse {
    video_url: String,
    input_url: String,
}

/// Uploader for completed chunks
///
/// Uses streaming uploads to avoid loading entire video files into RAM.
#[derive(Clone)]
pub struct Uploader {
    client: Client,
    lambda_endpoint: Option<String>,
}

impl Uploader {
    /// Create a new uploader
    pub fn new(config: &Config) -> Self {
        Self {
            client: Client::new(),
            lambda_endpoint: config.upload.lambda_endpoint.clone(),
        }
    }

    /// Upload a completed chunk using streaming for video files
    ///
    /// This method streams video files directly from disk to the network,
    /// avoiding the need to load the entire file into RAM. This is critical
    /// for segments that can be several hundred MB.
    pub async fn upload(&self, chunk: &CompletedChunk) -> Result<()> {
        let endpoint = self.lambda_endpoint.as_ref()
            .context("Lambda endpoint not configured")?;

        info!(
            "Uploading chunk {} for session {}",
            chunk.chunk_id, chunk.session_id
        );

        // 1. Get pre-signed URLs from Lambda
        let presign_request = PresignRequest {
            session_id: chunk.session_id.clone(),
            chunk_id: chunk.chunk_id.clone(),
            video_content_type: "video/mp4".to_string(),
            input_content_type: "application/msgpack".to_string(),
        };

        let presign_response: PresignResponse = self.client
            .post(endpoint)
            .json(&presign_request)
            .send()
            .await
            .context("Failed to request pre-signed URLs")?
            .json()
            .await
            .context("Failed to parse pre-signed URL response")?;

        debug!("Got pre-signed URLs for chunk {}", chunk.chunk_id);

        // 2. Upload video file using streaming (if path is available)
        if let Some(ref video_path) = chunk.video_path {
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

            self.client
                .put(&presign_response.video_url)
                .header("Content-Type", "video/mp4")
                .header("Content-Length", file_size)
                .body(body)
                .send()
                .await
                .context("Failed to upload video file")?
                .error_for_status()
                .context("Video upload returned error status")?;

            info!(
                "Uploaded video for chunk {} ({:.2} MB)",
                chunk.chunk_id,
                file_size as f64 / (1024.0 * 1024.0)
            );
        }

        // 3. Upload input log (small enough to fit in RAM)
        let input_bytes = rmp_serde::to_vec(&chunk.events)
            .context("Failed to serialize input events")?;

        self.client
            .put(&presign_response.input_url)
            .header("Content-Type", "application/msgpack")
            .body(input_bytes)
            .send()
            .await
            .context("Failed to upload input log")?
            .error_for_status()
            .context("Input log upload returned error status")?;

        info!(
            "Uploaded input log for chunk {} ({} events)",
            chunk.chunk_id,
            chunk.events.len()
        );

        Ok(())
    }

    /// Check if uploader is configured
    pub fn is_configured(&self) -> bool {
        self.lambda_endpoint.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_presign_request_serialization() {
        let request = PresignRequest {
            session_id: "test-session".to_string(),
            chunk_id: "0".to_string(),
            video_content_type: "video/mp4".to_string(),
            input_content_type: "application/msgpack".to_string(),
        };
        
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("test-session"));
        assert!(json.contains("video/mp4"));
    }
}
