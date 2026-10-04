//! OAuth für Microsoft 365 (Device-Code-Flow, öffentlicher Client, kein
//! Client-Secret). Es werden ausschließlich Lese-Scopes angefragt – selbst
//! ein Programmfehler kann daher keine Mail senden oder Teams-Nachricht posten.

use crate::http::{HttpError, TokenProvider};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Delegierte Microsoft-Graph-Berechtigungen (nur lesen).
pub const GRAPH_SCOPES: &[&str] = &["User.Read", "Mail.Read", "Chat.Read", "offline_access"];

/// Gmail: ausschließlich `gmail.readonly`.
pub const GMAIL_SCOPES: &[&str] = &["https://www.googleapis.com/auth/gmail.readonly"];

/// Prüft, dass eine Scope-Liste nichts Schreibendes enthält.
pub fn scopes_are_read_only(scopes: &[&str]) -> bool {
    scopes.iter().all(|s| {
        let l = s.to_ascii_lowercase();
        !(l.contains("write") || l.contains("send") || l.contains("manage") || l.contains("modify") || l.contains("compose")
            || l.contains("insert") || l.ends_with("mail.google.com/") || l.contains(".all") && !l.contains("read"))
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "five")]
    pub interval: u64,
    #[serde(default)]
    pub message: String,
}

fn five() -> u64 {
    5
}

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

struct Cached {
    access: String,
    refresh: Option<String>,
    valid_until: Instant,
}

/// Microsoft-Identity-Plattform, Device-Code-Flow.
pub struct MsDeviceCodeFlow {
    authority: String,
    client_id: String,
    http: reqwest::Client,
    cache: Mutex<Option<Cached>>,
}

impl MsDeviceCodeFlow {
    /// `tenant`: z. B. "organizations" oder die Tenant-ID der Schule.
    pub fn new(client_id: impl Into<String>, tenant: &str) -> Self {
        Self::with_authority(client_id, format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0"))
    }

    pub fn with_authority(client_id: impl Into<String>, authority: impl Into<String>) -> Self {
        assert!(scopes_are_read_only(GRAPH_SCOPES));
        Self {
            authority: authority.into(),
            client_id: client_id.into(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("HTTP-Client"),
            cache: Mutex::new(None),
        }
    }

    fn scope(&self) -> String {
        GRAPH_SCOPES.join(" ")
    }

    /// Schritt 1: Code anfordern. Die UI zeigt `user_code` und `verification_uri`.
    pub async fn start(&self) -> Result<DeviceCode, HttpError> {
        let r = self
            .http
            .post(format!("{}/devicecode", self.authority))
            .form(&[("client_id", self.client_id.as_str()), ("scope", self.scope().as_str())])
            .send()
            .await
            .map_err(|e| HttpError::Network(e.to_string()))?;
        if !r.status().is_success() {
            return Err(HttpError::Auth(r.text().await.unwrap_or_default()));
        }
        r.json().await.map_err(|e| HttpError::Auth(e.to_string()))
    }

    /// Schritt 2: warten, bis der Benutzer im Browser zugestimmt hat.
    pub async fn complete(&self, code: &DeviceCode) -> Result<Option<String>, HttpError> {
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = code.interval.max(1);
        while Instant::now() < deadline {
            let r = self
                .http
                .post(format!("{}/token", self.authority))
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", self.client_id.as_str()),
                    ("device_code", code.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(|e| HttpError::Network(e.to_string()))?;
            if r.status().is_success() {
                let t: TokenResponse = r.json().await.map_err(|e| HttpError::Auth(e.to_string()))?;
                let refresh = t.refresh_token.clone();
                self.store(t);
                return Ok(refresh);
            }
            let v: serde_json::Value = r.json().await.unwrap_or_default();
            match v["error"].as_str().unwrap_or("") {
                "authorization_pending" => {}
                "slow_down" => interval += 5,
                other => return Err(HttpError::Auth(format!("{other}: {}", v["error_description"].as_str().unwrap_or("")))),
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
        Err(HttpError::Auth("Zeit für die Anmeldung abgelaufen".into()))
    }

    /// Mit gespeichertem Refresh-Token (aus dem macOS-Schlüsselbund) starten.
    pub fn with_refresh_token(self, refresh: String) -> Self {
        *self.cache.lock().unwrap() = Some(Cached { access: String::new(), refresh: Some(refresh), valid_until: Instant::now() });
        self
    }

    fn store(&self, t: TokenResponse) {
        let mut c = self.cache.lock().unwrap();
        let refresh = t.refresh_token.or_else(|| c.as_ref().and_then(|c| c.refresh.clone()));
        *c = Some(Cached {
            access: t.access_token,
            refresh,
            valid_until: Instant::now() + Duration::from_secs(t.expires_in.saturating_sub(60)),
        });
    }

    async fn refresh(&self, refresh: &str) -> Result<String, HttpError> {
        let r = self
            .http
            .post(format!("{}/token", self.authority))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.client_id.as_str()),
                ("refresh_token", refresh),
                ("scope", self.scope().as_str()),
            ])
            .send()
            .await
            .map_err(|e| HttpError::Network(e.to_string()))?;
        if !r.status().is_success() {
            return Err(HttpError::Auth("Refresh fehlgeschlagen – bitte neu anmelden".into()));
        }
        let t: TokenResponse = r.json().await.map_err(|e| HttpError::Auth(e.to_string()))?;
        let a = t.access_token.clone();
        self.store(t);
        Ok(a)
    }
}

#[async_trait]
impl TokenProvider for MsDeviceCodeFlow {
    async fn token(&self) -> Result<String, HttpError> {
        let (valid, refresh) = {
            let c = self.cache.lock().unwrap();
            match c.as_ref() {
                Some(c) if c.valid_until > Instant::now() && !c.access.is_empty() => (Some(c.access.clone()), None),
                Some(c) => (None, c.refresh.clone()),
                None => (None, None),
            }
        };
        if let Some(a) = valid {
            return Ok(a);
        }
        match refresh {
            Some(r) => self.refresh(&r).await,
            None => Err(HttpError::Auth("nicht angemeldet".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn requested_scopes_are_read_only() {
        assert!(scopes_are_read_only(GRAPH_SCOPES));
        assert!(scopes_are_read_only(GMAIL_SCOPES));
        for bad in ["Mail.Send", "Mail.ReadWrite", "Chat.ReadWrite", "ChatMessage.Send", "https://mail.google.com/", "https://www.googleapis.com/auth/gmail.modify", "https://www.googleapis.com/auth/gmail.send"] {
            assert!(!scopes_are_read_only(&[bad]), "{bad}");
        }
    }

    #[tokio::test]
    async fn device_code_flow_requests_only_read_scopes() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/devicecode"))
            .and(body_string_contains("Mail.Read"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dc", "user_code": "ABCD", "verification_uri": "https://microsoft.com/devicelogin", "expires_in": 30, "interval": 1
            })))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "AT", "refresh_token": "RT", "expires_in": 3600
            })))
            .mount(&s)
            .await;
        let f = MsDeviceCodeFlow::with_authority("cid", s.uri());
        let dc = f.start().await.unwrap();
        assert_eq!(dc.user_code, "ABCD");
        assert_eq!(f.complete(&dc).await.unwrap().as_deref(), Some("RT"));
        assert_eq!(f.token().await.unwrap(), "AT");

        let reqs = s.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&reqs[0].body).to_string();
        for forbidden in ["Send", "ReadWrite"] {
            assert!(!body.contains(forbidden), "{body}");
        }
    }
}
