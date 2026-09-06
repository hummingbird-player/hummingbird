use super::super::{
    backend::{BackendError, BackendErrorKind, BackendResult},
    credentials::Secret,
    http::{HttpRequest, HttpTransport, MAX_JSON_REQUEST_BYTES, status_error},
};
use serde_json::Value;
use std::sync::Arc;

pub enum Authentication {
    Token {
        username: String,
        password: Arc<Secret>,
    },
    ApiKey(Arc<Secret>),
}
impl std::fmt::Debug for Authentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Authentication([REDACTED])")
    }
}

pub struct SubsonicClient {
    base: url::Url,
    authentication: Authentication,
    pub(super) transport: Arc<dyn HttpTransport>,
}
impl SubsonicClient {
    pub fn new(
        base: &str,
        allow_http: bool,
        authentication: Authentication,
        transport: Arc<dyn HttpTransport>,
    ) -> BackendResult<Self> {
        let mut base = url::Url::parse(base).map_err(|_| malformed())?;
        if !(base.scheme() == "https" || (base.scheme() == "http" && allow_http))
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(malformed());
        }
        // push path segments; Url::join("/rest/...") would discard proxy subpaths.
        base.path_segments_mut()
            .map_err(|_| malformed())?
            .pop_if_empty()
            .push("rest")
            .push("");
        Ok(Self {
            base,
            authentication,
            transport,
        })
    }
    pub fn uses_api_key(&self) -> bool {
        matches!(self.authentication, Authentication::ApiKey(_))
    }
    /// Kept private to the adapter. Authenticated URLs must never leave host memory.
    pub(super) fn request(
        &self,
        endpoint: &str,
        parameters: &[(&str, String)],
        max_bytes: u64,
    ) -> BackendResult<HttpRequest> {
        if !endpoint.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(malformed());
        }
        let mut url = self
            .base
            .join(&format!("{endpoint}.view"))
            .map_err(|_| malformed())?;
        let mut query = url.query_pairs_mut();
        query
            .append_pair("v", "1.16.1")
            .append_pair("c", "Hummingbird")
            .append_pair("f", "json");
        match &self.authentication {
            Authentication::Token { username, password } => {
                let salt = format!("{:032x}", rand::random::<u128>());
                let mut digest = md5::Context::new();
                digest.consume(password.expose());
                digest.consume(salt.as_bytes());
                query
                    .append_pair("u", username)
                    .append_pair("t", &format!("{:x}", digest.finalize()))
                    .append_pair("s", &salt);
            }
            Authentication::ApiKey(key) => {
                let key = std::str::from_utf8(key.expose())
                    .map_err(|_| BackendError::new(BackendErrorKind::Authentication))?;
                query.append_pair("apiKey", key);
            }
        }
        for (key, value) in parameters {
            // Callers cannot override authentication or the wire contract.
            if matches!(*key, "u" | "p" | "t" | "s" | "apiKey" | "v" | "c" | "f") {
                return Err(malformed());
            }
            query.append_pair(key, value);
        }
        drop(query);
        Ok(HttpRequest {
            url,
            max_bytes,
            range: None,
            if_range: None,
            json_body: None,
        })
    }
    pub async fn json(
        &self,
        endpoint: &str,
        parameters: &[(&str, String)],
    ) -> BackendResult<Value> {
        let request = self.request(endpoint, parameters, 8 * 1024 * 1024)?;
        let response = self.transport.execute(request).await?;
        if let Some(error) = status_error(&response) {
            return Err(error);
        }
        decode_envelope(&response.body)
    }

    /// Some advertised extensions require JSON POST while retaining the common
    /// authentication/query contract. The body and URL remain host-private.
    pub(super) async fn post_json(
        &self,
        endpoint: &str,
        parameters: &[(&str, String)],
        body: &Value,
    ) -> BackendResult<Value> {
        let body = serde_json::to_vec(body).map_err(|_| malformed())?;
        if body.len() > MAX_JSON_REQUEST_BYTES {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let mut request = self.request(endpoint, parameters, 8 * 1024 * 1024)?;
        request.json_body = Some(body);
        let response = self.transport.execute(request).await?;
        if let Some(error) = status_error(&response) {
            return Err(error);
        }
        decode_envelope(&response.body)
    }
}
pub(super) fn malformed() -> BackendError {
    BackendError::new(BackendErrorKind::MalformedResponse)
}

pub fn decode_envelope(body: &[u8]) -> BackendResult<Value> {
    let mut value: Value = serde_json::from_slice(body).map_err(|_| malformed())?;
    // Bandcamp falls through to a private API error document for Subsonic
    // endpoints it does not implement. Treat only that exact, known response as
    // endpoint absence so optional protocol fallbacks can continue.
    if value.get("error").and_then(Value::as_bool) == Some(true)
        && value.get("error_message").and_then(Value::as_str) == Some("bad version")
        && value.as_object().is_some_and(|object| object.len() == 2)
    {
        return Err(BackendError::unsupported());
    }
    let response = value
        .get_mut("subsonic-response")
        .ok_or_else(malformed)?
        .take();
    match response.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(response),
        Some("failed") => {
            let code = response
                .pointer("/error/code")
                .and_then(Value::as_u64)
                .ok_or_else(malformed)?;
            Err(api_error(code))
        }
        _ => Err(malformed()),
    }
}
pub(super) fn api_error(code: u64) -> BackendError {
    BackendError::new(match code {
        20 | 30 => BackendErrorKind::Unsupported,
        40..=44 => BackendErrorKind::Authentication,
        50 | 60 => BackendErrorKind::Forbidden,
        70 => BackendErrorKind::NotFound,
        _ => BackendErrorKind::MalformedResponse,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::http::HttpResponse;
    use async_trait::async_trait;
    struct Fixture;
    #[async_trait]
    impl HttpTransport for Fixture {
        async fn execute(&self, _: HttpRequest) -> BackendResult<HttpResponse> {
            Ok(HttpResponse { status: 200, content_type: Some("application/json".into()), retry_after_ms: None,
                body: br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"secret URL must not escape"}}}"#.to_vec() })
        }
    }
    #[tokio::test]
    async fn token_auth_keeps_proxy_subpath_and_redacts_api_errors() {
        let auth = Authentication::Token {
            username: "user".into(),
            password: Arc::new(Secret::new(b"password".to_vec())),
        };
        let client = SubsonicClient::new(
            "https://example.test/music/proxy/",
            false,
            auth,
            Arc::new(Fixture),
        )
        .unwrap();
        let first = client.request("ping", &[], 1024).unwrap();
        let second = client.request("ping", &[], 1024).unwrap();
        assert_eq!(first.url.path(), "/music/proxy/rest/ping.view");
        let params: std::collections::HashMap<_, _> = first.url.query_pairs().collect();
        assert_eq!(params["u"], "user");
        assert_eq!(
            params["t"],
            format!("{:x}", md5::compute(format!("password{}", params["s"])))
        );
        assert_ne!(first.url.query(), second.url.query());
        assert!(!format!("{first:?}").contains("user"));
        let error = client.json("ping", &[]).await.unwrap_err();
        assert_eq!(error.kind, BackendErrorKind::Authentication);
        assert!(!format!("{error:?}").contains("secret"));
    }
    #[test]
    fn api_keys_have_no_username_and_http_requires_opt_in() {
        let make = |base, allow_http| {
            SubsonicClient::new(
                base,
                allow_http,
                Authentication::ApiKey(Arc::new(Secret::new(b"api-key".to_vec()))),
                Arc::new(Fixture),
            )
        };
        assert!(make("http://example.test", false).is_err());
        assert!(make("https://user:password@example.test", false).is_err());
        assert!(make("https://example.test?password=secret", false).is_err());
        let client = make("http://example.test/proxy", true).unwrap();
        let request = client.request("ping", &[], 1024).unwrap();
        let params: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        assert_eq!(params["apiKey"], "api-key");
        assert!(!params.contains_key("u"));
        assert!(!params.contains_key("t"));
        assert!(
            client
                .request("ping", &[("u", "override".into())], 1024)
                .is_err()
        );
    }

    #[test]
    fn bandcamp_private_api_fallthrough_is_an_unsupported_endpoint() {
        let error =
            decode_envelope(br#"{"error":true,"error_message":"bad version"}"#).unwrap_err();
        assert_eq!(error.kind, BackendErrorKind::Unsupported);
        assert_eq!(
            decode_envelope(br#"{"error":true,"error_message":"different"}"#)
                .unwrap_err()
                .kind,
            BackendErrorKind::MalformedResponse
        );
    }
}
