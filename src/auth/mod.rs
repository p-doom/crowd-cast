//! Google OAuth authentication for crowd-cast.
//!
//! Provides optional Google Sign-In via the PKCE OAuth flow.
//! Tokens are stored locally in `auth.json` and sent as Bearer
//! tokens with presign requests. Auth is optional — the app works
//! without it, but authenticated uploads get UUID→email mapping
//! in DynamoDB for the dashboard.

mod oauth;

pub use oauth::AuthManager;
