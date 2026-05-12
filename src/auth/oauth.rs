//! Google OAuth PKCE flow for desktop apps.
//!
//! Flow: open browser → Google consent → redirect to localhost → exchange code
//! for tokens → store in auth.json. Tokens are refreshed transparently before
//! expiry.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

/// Stored auth state, persisted to auth.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthState {
    pub google_sub: String,
    pub email: String,
    pub name: String,
    pub id_token: String,
    pub refresh_token: String,
    /// ISO 8601 timestamp when the ID token expires.
    pub token_expiry: String,
}

/// Manages authentication state: login, token refresh, persistence.
pub struct AuthManager {
    /// Cached auth state (None if not logged in).
    state: Option<AuthState>,
    /// Google OAuth client ID (compile-time).
    client_id: String,
    /// Google OAuth client secret (compile-time, not confidential for desktop apps).
    client_secret: String,
}

impl AuthManager {
    /// Create a new AuthManager, loading persisted state if available.
    pub fn new(client_id: &str, client_secret: &str) -> Self {
        let state = Self::auth_path()
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .and_then(|s| serde_json::from_str::<AuthState>(&s).ok());

        if let Some(ref s) = state {
            info!("Loaded auth state for {}", s.email);
        }

        Self {
            state,
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        }
    }

    /// Path to auth.json in the data directory.
    fn auth_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .map(|p| p.data_dir().join("auth.json"))
    }

    /// Whether the user is authenticated (has a refresh token).
    pub fn is_authenticated(&self) -> bool {
        self.state.is_some()
    }

    /// Get the user's email for display.
    pub fn email(&self) -> Option<&str> {
        self.state.as_ref().map(|s| s.email.as_str())
    }

    /// Get a valid ID token, refreshing if necessary.
    /// Returns None if not authenticated or refresh fails.
    pub async fn get_valid_token(&mut self) -> Option<String> {
        let state = self.state.as_ref()?;

        // Check if token expires within 5 minutes
        let expiry = chrono::DateTime::parse_from_rfc3339(&state.token_expiry).ok()?;
        let now = chrono::Utc::now();
        let buffer = chrono::Duration::minutes(5);

        if expiry > now + buffer {
            return Some(state.id_token.clone());
        }

        // Token expired or expiring soon — refresh
        info!("ID token expiring soon, refreshing...");
        match self.refresh_token().await {
            Ok(()) => self.state.as_ref().map(|s| s.id_token.clone()),
            Err(e) => {
                warn!("Token refresh failed: {}", e);
                None
            }
        }
    }

    /// Run the full OAuth PKCE login flow.
    /// Opens the browser, waits for the callback, exchanges the code for tokens.
    pub async fn login(&mut self) -> Result<AuthState> {
        info!("Starting Google OAuth login flow...");

        // Generate PKCE code verifier + challenge
        let code_verifier = generate_code_verifier();
        let code_challenge = generate_code_challenge(&code_verifier);

        // Bind a localhost listener on a random port
        let listener = TcpListener::bind("127.0.0.1:0")
            .context("Failed to bind localhost listener for OAuth callback")?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{}", port);

        // Build the Google authorization URL
        let auth_url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?\
             client_id={}&\
             redirect_uri={}&\
             response_type=code&\
             scope=openid%20email%20profile&\
             code_challenge={}&\
             code_challenge_method=S256&\
             access_type=offline&\
             prompt=consent",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(&code_challenge),
        );

        // Open browser
        info!("Opening browser for Google sign-in...");
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&auth_url).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(&auth_url)
                .spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", &auth_url])
                .spawn();
        }

        // Wait for the callback (blocking)
        info!("Waiting for OAuth callback on port {}...", port);
        let auth_code = Self::wait_for_callback(listener)?;

        // Exchange authorization code for tokens
        info!("Exchanging authorization code for tokens...");
        let token_response = Self::exchange_code(
            &self.client_id,
            &self.client_secret,
            &auth_code,
            &redirect_uri,
            &code_verifier,
        )
        .await?;

        // Parse ID token to extract claims
        let claims = decode_id_token_claims(&token_response.id_token)?;

        let expiry = chrono::Utc::now()
            + chrono::Duration::seconds(token_response.expires_in as i64);

        let auth_state = AuthState {
            google_sub: claims.sub,
            email: claims.email.clone(),
            name: claims.name,
            id_token: token_response.id_token,
            refresh_token: token_response
                .refresh_token
                .unwrap_or_else(|| {
                    self.state
                        .as_ref()
                        .map(|s| s.refresh_token.clone())
                        .unwrap_or_default()
                }),
            token_expiry: expiry.to_rfc3339(),
        };

        // Persist
        self.save(&auth_state)?;
        self.state = Some(auth_state.clone());

        info!("Logged in as {}", claims.email);
        Ok(auth_state)
    }

    /// Refresh the ID token using the stored refresh token.
    async fn refresh_token(&mut self) -> Result<()> {
        let state = self
            .state
            .as_ref()
            .context("Not authenticated")?;

        let client = reqwest::Client::new();
        let resp = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &state.refresh_token),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .send()
            .await
            .context("Token refresh request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed: HTTP {} — {}", status, &body[..body.len().min(200)]);
        }

        let token_resp: TokenResponse = resp.json().await
            .context("Failed to parse refresh response")?;

        let expiry = chrono::Utc::now()
            + chrono::Duration::seconds(token_resp.expires_in as i64);

        let mut new_state = state.clone();
        new_state.id_token = token_resp.id_token;
        new_state.token_expiry = expiry.to_rfc3339();
        if let Some(rt) = token_resp.refresh_token {
            new_state.refresh_token = rt;
        }

        self.save(&new_state)?;
        self.state = Some(new_state);

        debug!("Token refreshed successfully");
        Ok(())
    }

    /// Log out: delete auth.json and clear state.
    pub fn logout(&mut self) {
        if let Some(path) = Self::auth_path() {
            let _ = std::fs::remove_file(&path);
        }
        self.state = None;
        info!("Logged out");
    }

    /// Save auth state to disk.
    fn save(&self, state: &AuthState) -> Result<()> {
        let Some(path) = Self::auth_path() else {
            anyhow::bail!("Could not determine auth file path");
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(state)?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write auth state to {:?}", path))?;

        // Set file permissions to owner-only on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    /// Wait for the OAuth callback on the localhost listener.
    /// Returns the authorization code from the query string.
    fn wait_for_callback(listener: TcpListener) -> Result<String> {
        // Set a timeout so we don't block forever
        listener.set_nonblocking(false)?;

        let (mut stream, _) = listener
            .accept()
            .context("Failed to accept OAuth callback connection")?;

        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf)?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the GET request to extract the code parameter
        let first_line = request.lines().next().unwrap_or("");
        let path = first_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("");

        let code = extract_query_param(path, "code")
            .context("No 'code' parameter in OAuth callback")?;

        // Send a success response to the browser
        let html = r#"<html><body style="font-family:system-ui;text-align:center;padding-top:80px;">
            <h2>Signed in successfully!</h2>
            <p>You can close this tab and return to CrowdCast.</p>
            </body></html>"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();

        Ok(code)
    }

    /// Exchange the authorization code for tokens.
    async fn exchange_code(
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse> {
        let client = reqwest::Client::new();
        let resp = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await
            .context("Token exchange request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed: HTTP {} — {}", status, &body[..body.len().min(500)]);
        }

        resp.json::<TokenResponse>()
            .await
            .context("Failed to parse token exchange response")
    }
}

// ---------------------------------------------------------------------------
// Token exchange response
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

// ---------------------------------------------------------------------------
// JWT claims (decoded without verification — Lambda verifies)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    sub: String,
    email: String,
    #[serde(default)]
    name: String,
}

fn decode_id_token_claims(id_token: &str) -> Result<IdTokenClaims> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        anyhow::bail!("Invalid ID token format");
    }

    // Decode the payload (second part), base64url
    use base64::Engine;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .context("Failed to base64-decode ID token payload")?;

    serde_json::from_slice::<IdTokenClaims>(&payload_bytes)
        .context("Failed to parse ID token claims")
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

fn generate_code_verifier() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
}

fn generate_code_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(verifier.as_bytes());
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

// ---------------------------------------------------------------------------
// URL query parsing
// ---------------------------------------------------------------------------

fn extract_query_param(path: &str, param: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(key), Some(value)) = (kv.next(), kv.next()) {
            if key == param {
                return Some(urlencoding::decode(value).unwrap_or_default().into_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_challenge() {
        let verifier = "test_verifier_string_here";
        let challenge = generate_code_challenge(verifier);
        // Should be a base64url string, no padding
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
        assert!(!challenge.is_empty());
    }

    #[test]
    fn test_extract_query_param() {
        assert_eq!(
            extract_query_param("/?code=abc123&state=xyz", "code"),
            Some("abc123".to_string())
        );
        assert_eq!(
            extract_query_param("/?code=abc123&state=xyz", "state"),
            Some("xyz".to_string())
        );
        assert_eq!(
            extract_query_param("/?code=abc123", "missing"),
            None
        );
        assert_eq!(extract_query_param("/noquery", "code"), None);
    }

    #[test]
    fn test_decode_id_token_claims() {
        // Build a fake JWT with a known payload
        use base64::Engine;
        let payload = r#"{"sub":"12345","email":"test@example.com","name":"Test User"}"#;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let fake_jwt = format!("header.{}.signature", encoded);

        let claims = decode_id_token_claims(&fake_jwt).unwrap();
        assert_eq!(claims.sub, "12345");
        assert_eq!(claims.email, "test@example.com");
        assert_eq!(claims.name, "Test User");
    }
}
