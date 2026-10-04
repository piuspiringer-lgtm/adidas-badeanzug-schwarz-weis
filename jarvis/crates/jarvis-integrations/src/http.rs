//! HTTP-Clients mit eingebauten Sicherheitsgrenzen.
//!
//! * [`WebClient`]: öffentliches Web, **nur GET**, blockiert localhost und
//!   private Netze (auch nach DNS-Auflösung und bei Redirects), begrenzt
//!   Antwortgröße und URL-Länge.
//! * [`ReadOnlyApi`]: feste API-Basis (Graph, Gmail), GET plus eine
//!   **fest einkompilierte** Liste von POST-Pfaden, die nur suchen/lesen.
//!   Es gibt keine Methode für PUT/PATCH/DELETE oder beliebige POSTs.

use async_trait::async_trait;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_URL_LEN: usize = 2048;
pub const MAX_BODY_BYTES: usize = 3 * 1024 * 1024;
pub const USER_AGENT: &str = concat!("JARVIS/", env!("CARGO_PKG_VERSION"), " (lokaler Assistent)");

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("URL abgelehnt: {0}")]
    Blocked(String),
    #[error("Netzwerkfehler: {0}")]
    Network(String),
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Antwort zu groß")]
    TooLarge,
    #[error("Authentifizierung: {0}")]
    Auth(String),
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] >= 224)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// DNS-Resolver, der nur öffentliche Adressen zurückgibt (Schutz vor SSRF
/// und DNS-Rebinding auf Router, Ollama oder andere lokale Dienste).
struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} löst nur auf private/lokale Adressen auf").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

pub fn check_url(raw: &str, allow_private: bool) -> Result<url::Url, HttpError> {
    if raw.len() > MAX_URL_LEN {
        return Err(HttpError::Blocked("URL zu lang".into()));
    }
    let u = url::Url::parse(raw).map_err(|e| HttpError::Blocked(e.to_string()))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(HttpError::Blocked(format!("Schema '{}' nicht erlaubt", u.scheme())));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(HttpError::Blocked("Zugangsdaten in URL nicht erlaubt".into()));
    }
    if !allow_private {
        match u.host() {
            Some(url::Host::Ipv4(ip)) if !is_public_ip(IpAddr::V4(ip)) => {
                return Err(HttpError::Blocked("private/lokale Adresse".into()))
            }
            Some(url::Host::Ipv6(ip)) if !is_public_ip(IpAddr::V6(ip)) => {
                return Err(HttpError::Blocked("private/lokale Adresse".into()))
            }
            Some(url::Host::Domain(d)) if d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local") => {
                return Err(HttpError::Blocked("lokaler Hostname".into()))
            }
            None => return Err(HttpError::Blocked("kein Host".into())),
            _ => {}
        }
    }
    Ok(u)
}

#[derive(Debug, Clone)]
pub struct FetchedBody {
    pub final_url: String,
    pub content_type: String,
    pub body: String,
}

/// GET-only-Client für das öffentliche Web.
#[derive(Clone)]
pub struct WebClient {
    http: reqwest::Client,
    allow_private: bool,
}

impl WebClient {
    pub fn new() -> Self {
        Self::build(false)
    }

    /// Erlaubt lokale Ziele – nur für vom Benutzer selbst konfigurierte
    /// lokale Dienste (z. B. SearXNG) und Tests. Nie für LLM-gesteuerte URLs.
    pub fn new_allowing_private() -> Self {
        Self::build(true)
    }

    fn build(allow_private: bool) -> Self {
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("zu viele Weiterleitungen")
            } else if check_url(attempt.url().as_str(), allow_private).is_err() {
                attempt.error("Weiterleitung auf blockierte Adresse")
            } else {
                attempt.follow()
            }
        });
        let mut b = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(redirect)
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20));
        // Kein System-Proxy: Sonst würde der Proxy die Namen auflösen und der
        // Schutz vor lokalen Zielen (PublicOnlyResolver) wäre umgangen.
        b = b.no_proxy();
        if !allow_private {
            b = b.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        Self { http: b.build().expect("HTTP-Client"), allow_private }
    }

    pub async fn get_text(&self, raw_url: &str, headers: &[(&str, &str)]) -> Result<FetchedBody, HttpError> {
        let u = check_url(raw_url, self.allow_private)?;
        let mut req = self.http.get(u);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let mut resp = req.send().await.map_err(|e| HttpError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if resp.content_length().is_some_and(|l| l as usize > MAX_BODY_BYTES) {
            return Err(HttpError::TooLarge);
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| HttpError::Network(e.to_string()))? {
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_BODY_BYTES {
                return Err(HttpError::TooLarge);
            }
        }
        let body = String::from_utf8_lossy(&buf).into_owned();
        if status >= 400 {
            return Err(HttpError::Status { status, body: body.chars().take(300).collect() });
        }
        Ok(FetchedBody { final_url, content_type, body })
    }
}

impl Default for WebClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Liefert Zugriffstoken (OAuth). Implementierungen: [`StaticToken`],
/// [`crate::oauth::MsDeviceCodeFlow`].
#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn token(&self) -> Result<String, HttpError>;
}

pub struct StaticToken(pub String);

#[async_trait]
impl TokenProvider for StaticToken {
    async fn token(&self) -> Result<String, HttpError> {
        Ok(self.0.clone())
    }
}

/// Read-only-Zugang zu einer festen API. Nur GET und erlaubte Such-POSTs.
#[derive(Clone)]
pub struct ReadOnlyApi {
    base: String,
    http: reqwest::Client,
    token: Arc<dyn TokenProvider>,
    allowed_post_paths: &'static [&'static str],
}

fn check_path(path: &str) -> Result<(), HttpError> {
    if !path.starts_with('/') || path.contains("..") || path.contains("://") || path.contains('#') || path.contains('\\') {
        return Err(HttpError::Blocked(format!("ungültiger Pfad '{path}'")));
    }
    Ok(())
}

impl ReadOnlyApi {
    pub fn new(base: impl Into<String>, token: Arc<dyn TokenProvider>, allowed_post_paths: &'static [&'static str]) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP-Client");
        Self { base: base.into().trim_end_matches('/').to_string(), http, token, allowed_post_paths }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, HttpError> {
        let t = self.token.token().await?;
        let resp = req.bearer_auth(t).send().await.map_err(|e| HttpError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| HttpError::Network(e.to_string()))?;
        if text.len() > MAX_BODY_BYTES {
            return Err(HttpError::TooLarge);
        }
        if status >= 400 {
            return Err(HttpError::Status { status, body: text.chars().take(300).collect() });
        }
        serde_json::from_str(&text).map_err(|e| HttpError::Network(format!("ungültiges JSON: {e}")))
    }

    /// GET `path` (inkl. Query) relativ zur Basis.
    pub async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Result<Value, HttpError> {
        check_path(path)?;
        let mut req = self.http.get(format!("{}{path}", self.base));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.send(req).await
    }

    /// POST ausschließlich an fest erlaubte Such-Endpunkte.
    pub async fn post_search(&self, path: &str, body: &Value) -> Result<Value, HttpError> {
        check_path(path)?;
        if !self.allowed_post_paths.contains(&path) {
            return Err(HttpError::Blocked(format!("POST auf '{path}' ist nicht erlaubt")));
        }
        self.send(self.http.post(format!("{}{path}", self.base)).json(body)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_checks() {
        for bad in [
            "http://127.0.0.1:11434/api/tags",
            "http://localhost/",
            "http://192.168.0.1/",
            "http://10.1.2.3/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "file:///etc/passwd",
            "ftp://example.org/",
            "http://user:pw@example.org/",
            "http://printer.local/",
        ] {
            assert!(check_url(bad, false).is_err(), "{bad}");
        }
        assert!(check_url("https://example.org/a?b=c", false).is_ok());
        assert!(check_url(&format!("https://example.org/{}", "a".repeat(3000)), false).is_err());
    }

    #[test]
    fn api_paths() {
        assert!(check_path("/me/messages?$top=5").is_ok());
        assert!(check_path("me/messages").is_err());
        assert!(check_path("/../admin").is_err());
        assert!(check_path("/x?u=http://evil").is_err());
    }

    #[tokio::test]
    async fn web_client_blocks_localhost_even_via_dns() {
        let c = WebClient::new();
        assert!(matches!(c.get_text("http://127.0.0.1:1/", &[]).await, Err(HttpError::Blocked(_))));
    }

    #[tokio::test]
    async fn read_only_api_rejects_non_allowlisted_post() {
        let api = ReadOnlyApi::new("http://127.0.0.1:9", Arc::new(StaticToken("t".into())), &["/search/query"]);
        assert!(matches!(
            api.post_search("/me/sendMail", &serde_json::json!({})).await,
            Err(HttpError::Blocked(_))
        ));
    }
}
