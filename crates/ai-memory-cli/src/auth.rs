//! Bearer-token authorization middleware for the HTTP server.
//!
//! When `[auth].bearer_token` (or the `AI_MEMORY_AUTH_TOKEN` env var)
//! is set, every request to `/mcp`, `/hook`, and `/handoff` must
//! carry an `Authorization: Bearer <token>` header that matches.
//!
//! When the token is *unset*, the middleware is a no-op — preserving
//! the zero-config local-development experience and keeping the
//! existing e2e + unit tests working.
//!
//! Comparison uses [`subtle::ConstantTimeEq`] so an attacker on the
//! same LAN cannot use response-time leaks to recover the token byte
//! by byte. The constant-time guarantee depends on both sides being
//! the same length; `subtle` returns a constant-cost `Choice::from(0)`
//! when lengths differ, which is the right thing here.
//!
//! Wire shape matches the MCP authorization spec
//! (modelcontextprotocol.io/specification/.../basic/authorization):
//! 401 responses include a `WWW-Authenticate: Bearer …` header so
//! conformant clients can detect missing/expired credentials.
//!
//! ## Why not OAuth
//!
//! The MCP spec mandates full OAuth 2.1 for HTTP-authenticated
//! servers. That's overkill for a single-user homelab and would
//! force every MCP client config to deal with authorization-server
//! discovery + PKCE + token refresh. A static bearer token is
//! wire-compatible with the spec's `Authorization: Bearer …` shape
//! (clients send the header the same way; they just don't run the
//! OAuth dance to obtain the token). Every supported client
//! (Claude Code, Codex, OpenCode, Cursor, Claude Desktop via
//! `mcp-remote`, Gemini CLI, OpenClaw) accepts a static
//! `Authorization` header in its config.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;
use tracing::debug;

/// Shared auth state. Cheap to clone — just an `Arc` wrapping the
/// optional configured token.
#[derive(Clone, Debug)]
pub struct AuthState {
    expected: Option<String>,
}

impl AuthState {
    /// Build state from the (optional) configured token. `None` means
    /// "auth disabled, accept everything".
    #[must_use]
    pub fn new(expected: Option<String>) -> Self {
        Self { expected }
    }

    /// True when a token is configured (i.e. the middleware is doing
    /// anything). Useful for the startup log line so the operator
    /// sees whether their server is open or closed.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.expected.is_some()
    }
}

/// axum middleware closure. Wire with
/// `axum::middleware::from_fn_with_state(state, require_bearer)`.
pub async fn require_bearer(
    State(state): State<Arc<AuthState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(expected) = state.expected.as_deref() else {
        return next.run(req).await;
    };

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            // Accept both "Bearer xxx" and "bearer xxx" (case-insensitive
            // scheme per RFC 7235 §2.1).
            let (scheme, value) = h.split_once(' ')?;
            if scheme.eq_ignore_ascii_case("Bearer") {
                Some(value.trim_start())
            } else {
                None
            }
        })
        .unwrap_or("");

    if provided.as_bytes().ct_eq(expected.as_bytes()).into() {
        next.run(req).await
    } else {
        debug!("auth rejected: invalid or missing bearer token");
        unauthorized()
    }
}

fn unauthorized() -> Response {
    let mut resp = (StatusCode::UNAUTHORIZED, "auth required\n").into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        // MCP spec wants this header; clients use it to disambiguate
        // "missing token" from "wrong token" and surface a helpful
        // diagnostic. The `realm` is informational.
        "Bearer realm=\"ai-memory\", error=\"invalid_token\""
            .parse()
            .expect("static header value is valid"),
    );
    resp
}

/// Generate a fresh random bearer token, hex-encoded.
///
/// `bytes` is the entropy budget; 32 bytes (256 bits) is plenty for
/// any conceivable threat model.
///
/// # Errors
/// Propagates failures from the OS RNG.
pub fn generate_token_hex(bytes: usize) -> Result<String, getrandom::Error> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf)?;
    Ok(hex_encode(&buf))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    fn router_with_auth(token: Option<&str>) -> Router {
        let state = Arc::new(AuthState::new(token.map(str::to_string)));
        Router::new()
            .route("/probe", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(state, require_bearer))
    }

    #[tokio::test]
    async fn no_token_configured_passes_anything_through() {
        let r = router_with_auth(None);
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_header_returns_401_with_www_authenticate() {
        let r = router_with_auth(Some("secret"));
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let www = resp.headers().get(header::WWW_AUTHENTICATE).unwrap();
        assert!(www.to_str().unwrap().contains("Bearer"));
    }

    #[tokio::test]
    async fn wrong_token_returns_401() {
        let r = router_with_auth(Some("the-right-one"));
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("Authorization", "Bearer the-wrong-one")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn right_token_returns_200() {
        let r = router_with_auth(Some("right-token"));
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("Authorization", "Bearer right-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn lowercase_scheme_is_accepted() {
        let r = router_with_auth(Some("right-token"));
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("Authorization", "bearer right-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_bearer_scheme_is_rejected() {
        let r = router_with_auth(Some("right-token"));
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("Authorization", "Basic dXNlcjpwYXNz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn generated_token_is_hex_and_correct_length() {
        let t = generate_token_hex(32).unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Distinct calls produce distinct tokens (modulo OS RNG bugs).
        let t2 = generate_token_hex(32).unwrap();
        assert_ne!(t, t2);
    }
}
