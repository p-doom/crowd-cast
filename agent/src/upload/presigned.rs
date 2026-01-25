//! Pre-signed URL upload implementation

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

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
pub struct Uploader {
    client: Client,
    lambda_endpoint: Option<String>,
    delete_after_upload: bool,
}

impl Uploader {
    /// Create a new uploader
    pub fn new(config: &Config) -> Self {
        Self {
            client: Client::new(),
            lambda_endpoint: config.upload.lambda_endpoint.clone(),
            delete_after_upload: config.upload.delete_after_upload,
        }
    }
    
    /// Upload a completed chunk
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
        
        // 2. Upload video file
        let video_bytes = tokio::fs::read(&chunk.video_path)
            .await
            .with_context(|| format!("Failed to read video file: {:?}", chunk.video_path))?;
        
        self.client
            .put(&presign_response.video_url)
            .header("Content-Type", "video/mp4")
            .body(video_bytes)
            .send()
            .await
            .context("Failed to upload video file")?
            .error_for_status()
            .context("Video upload returned error status")?;
        
        info!("Uploaded video for chunk {}", chunk.chunk_id);
        
        // 3. Upload input log
        let input_bytes = chunk.input_chunk.to_msgpack()
            .context("Failed to serialize input chunk")?;
        
        self.client
            .put(&presign_response.input_url)
            .header("Content-Type", "application/msgpack")
            .body(input_bytes)
            .send()
            .await
            .context("Failed to upload input log")?
            .error_for_status()
            .context("Input log upload returned error status")?;
        
        info!("Uploaded input log for chunk {} ({} events)", 
              chunk.chunk_id, chunk.input_chunk.events.len());
        
        // 4. Delete local files if configured
        if self.delete_after_upload {
            if let Err(e) = tokio::fs::remove_file(&chunk.video_path).await {
                error!("Failed to delete video file after upload: {}", e);
            } else {
                debug!("Deleted local video file: {:?}", chunk.video_path);
            }
        }
        
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
