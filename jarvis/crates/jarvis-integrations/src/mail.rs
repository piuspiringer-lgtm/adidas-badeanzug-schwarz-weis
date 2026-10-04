//! E-Mail: **nur suchen, lesen, zusammenfassen**.
//!
//! Der Trait [`MailReader`] hat ausschließlich Lese-Methoden. Die Anbieter
//! nutzen [`ReadOnlyApi`], das technisch nur GET kann. Senden, Antworten,
//! Löschen oder Bearbeiten existieren im Code nicht.

use crate::http::{HttpError, ReadOnlyApi};
use async_trait::async_trait;
use base64::Engine;
use jarvis_context::truncate_middle;
use jarvis_permissions::{Access, CallFacts, Capability, Confirmation, Integration, RiskLevel, ToolSpec};
use jarvis_runtime::{Tool, ToolError, ToolOutput};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MailSummary {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailMessage {
    pub summary: MailSummary,
    pub body: String,
}

/// Ausschließlich lesender Zugriff auf ein Postfach.
#[async_trait]
pub trait MailReader: Send + Sync {
    fn provider(&self) -> &'static str;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MailSummary>, HttpError>;
    async fn recent(&self, limit: usize) -> Result<Vec<MailSummary>, HttpError>;
    async fn get(&self, id: &str) -> Result<MailMessage, HttpError>;
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn safe_id(id: &str) -> Result<&str, HttpError> {
    if id.is_empty() || id.len() > 512 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "-_=+".contains(c)) {
        return Err(HttpError::Blocked("ungültige Nachrichten-ID".into()));
    }
    Ok(id)
}

pub(crate) fn html_to_text(html: &str) -> String {
    let doc = scraper::Html::parse_fragment(html);
    doc.root_element().text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Outlook / Microsoft 365 über Microsoft Graph.
pub struct GraphMail {
    pub api: ReadOnlyApi,
}

const GRAPH_SELECT: &str = "id,subject,from,receivedDateTime,bodyPreview";

fn graph_summary(m: &Value) -> MailSummary {
    MailSummary {
        id: m["id"].as_str().unwrap_or_default().into(),
        from: format!(
            "{} <{}>",
            m["from"]["emailAddress"]["name"].as_str().unwrap_or(""),
            m["from"]["emailAddress"]["address"].as_str().unwrap_or("")
        ),
        subject: m["subject"].as_str().unwrap_or_default().into(),
        date: m["receivedDateTime"].as_str().unwrap_or_default().into(),
        preview: m["bodyPreview"].as_str().unwrap_or_default().into(),
    }
}

#[async_trait]
impl MailReader for GraphMail {
    fn provider(&self) -> &'static str {
        "outlook"
    }
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MailSummary>, HttpError> {
        let q = query.replace('"', "");
        let v = self.api.get(&format!("/me/messages?$search={}&$top={}&$select={GRAPH_SELECT}", enc(&format!("\"{q}\"")), limit.min(25)), &[]).await?;
        Ok(v["value"].as_array().map(|a| a.iter().map(graph_summary).collect()).unwrap_or_default())
    }
    async fn recent(&self, limit: usize) -> Result<Vec<MailSummary>, HttpError> {
        let v = self.api.get(&format!("/me/mailFolders/inbox/messages?$top={}&$orderby=receivedDateTime%20desc&$select={GRAPH_SELECT}", limit.min(25)), &[]).await?;
        Ok(v["value"].as_array().map(|a| a.iter().map(graph_summary).collect()).unwrap_or_default())
    }
    async fn get(&self, id: &str) -> Result<MailMessage, HttpError> {
        let v = self
            .api
            .get(&format!("/me/messages/{}?$select={GRAPH_SELECT},body", safe_id(id)?), &[("Prefer", "outlook.body-content-type=\"text\"")])
            .await?;
        let body = v["body"]["content"].as_str().unwrap_or_default();
        let body = if v["body"]["contentType"].as_str() == Some("html") { html_to_text(body) } else { body.to_string() };
        Ok(MailMessage { summary: graph_summary(&v), body })
    }
}

/// Gmail über die Gmail-API mit Scope `gmail.readonly`.
pub struct Gmail {
    pub api: ReadOnlyApi,
}

fn header(m: &Value, name: &str) -> String {
    m["payload"]["headers"]
        .as_array()
        .and_then(|h| h.iter().find(|x| x["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))))
        .and_then(|x| x["value"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn gmail_body(part: &Value) -> Option<String> {
    let mime = part["mimeType"].as_str().unwrap_or("");
    if mime == "text/plain" || mime == "text/html" {
        let data = part["body"]["data"].as_str()?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data.trim_end_matches('=')).ok()?;
        let s = String::from_utf8_lossy(&bytes).into_owned();
        return Some(if mime == "text/html" { html_to_text(&s) } else { s });
    }
    let parts = part["parts"].as_array()?;
    parts
        .iter()
        .find(|p| p["mimeType"] == "text/plain")
        .and_then(gmail_body)
        .or_else(|| parts.iter().find_map(gmail_body))
}

impl Gmail {
    async fn summaries(&self, q: Option<&str>, limit: usize) -> Result<Vec<MailSummary>, HttpError> {
        let mut path = format!("/gmail/v1/users/me/messages?maxResults={}", limit.min(25));
        if let Some(q) = q {
            path.push_str(&format!("&q={}", enc(q)));
        }
        let list = self.api.get(&path, &[]).await?;
        let mut out = vec![];
        for m in list["messages"].as_array().cloned().unwrap_or_default() {
            let id = m["id"].as_str().unwrap_or_default();
            let v = self
                .api
                .get(&format!("/gmail/v1/users/me/messages/{}?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date", safe_id(id)?), &[])
                .await?;
            out.push(MailSummary {
                id: id.into(),
                from: header(&v, "From"),
                subject: header(&v, "Subject"),
                date: header(&v, "Date"),
                preview: v["snippet"].as_str().unwrap_or_default().into(),
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl MailReader for Gmail {
    fn provider(&self) -> &'static str {
        "gmail"
    }
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MailSummary>, HttpError> {
        self.summaries(Some(query), limit).await
    }
    async fn recent(&self, limit: usize) -> Result<Vec<MailSummary>, HttpError> {
        self.summaries(Some("in:inbox"), limit).await
    }
    async fn get(&self, id: &str) -> Result<MailMessage, HttpError> {
        let v = self.api.get(&format!("/gmail/v1/users/me/messages/{}?format=full", safe_id(id)?), &[]).await?;
        Ok(MailMessage {
            summary: MailSummary {
                id: id.into(),
                from: header(&v, "From"),
                subject: header(&v, "Subject"),
                date: header(&v, "Date"),
                preview: v["snippet"].as_str().unwrap_or_default().into(),
            },
            body: gmail_body(&v["payload"]).unwrap_or_default(),
        })
    }
}

// ---------------- Tools ----------------

const MAIL_CAPS: &[Capability] = &[Capability::MailRead, Capability::NetworkGet];

macro_rules! mail_spec {
    ($name:literal, $desc:literal) => {
        ToolSpec {
            name: $name,
            description: $desc,
            integration: Integration::Email,
            access: Access::Read,
            risk: RiskLevel::Low,
            confirmation: Confirmation::Never,
            capabilities: MAIL_CAPS,
            services: &[],
        }
    };
}

static SEARCH: ToolSpec = mail_spec!("email_search", "Durchsucht E-Mails (nur lesen).");
static READ: ToolSpec = mail_spec!("email_read", "Liest eine E-Mail anhand ihrer ID (nur lesen).");
static RECENT: ToolSpec = mail_spec!("email_recent", "Listet die neuesten E-Mails im Posteingang (nur lesen).");
static DIGEST: ToolSpec = mail_spec!("email_digest", "Liefert gekürzte Inhalte passender E-Mails zum Zusammenfassen (nur lesen).");

fn list_text(v: &[MailSummary]) -> String {
    v.iter().map(|m| format!("- [{}] {} | {} | {}\n  {}", m.id, m.date, m.from, m.subject, m.preview)).collect::<Vec<_>>().join("\n")
}

fn err(e: HttpError) -> ToolError {
    ToolError::Failed(e.to_string())
}

pub struct MailTool {
    reader: Arc<dyn MailReader>,
    spec: &'static ToolSpec,
}

#[async_trait]
impl Tool for MailTool {
    fn spec(&self) -> &ToolSpec {
        self.spec
    }
    fn parameters(&self) -> Value {
        match self.spec.name {
            "email_read" => json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}),
            "email_recent" => json!({"type":"object","properties":{"limit":{"type":"integer"}}}),
            _ => json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}),
        }
    }
    fn facts(&self, _a: &Value) -> Result<CallFacts, ToolError> {
        Ok(CallFacts { preview: format!("{} ({})", self.spec.name, self.reader.provider()), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(10).min(25) as usize;
        let q = || a.get("query").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| ToolError::InvalidArgs("'query' fehlt".into()));
        match self.spec.name {
            "email_search" => {
                let r = self.reader.search(q()?, limit).await.map_err(err)?;
                Ok(ToolOutput::with_data(list_text(&r), json!(r)))
            }
            "email_recent" => {
                let r = self.reader.recent(limit).await.map_err(err)?;
                Ok(ToolOutput::with_data(list_text(&r), json!(r)))
            }
            "email_read" => {
                let id = a.get("id").and_then(Value::as_str).ok_or_else(|| ToolError::InvalidArgs("'id' fehlt".into()))?;
                let m = self.reader.get(id).await.map_err(err)?;
                let s = &m.summary;
                Ok(ToolOutput::with_data(
                    format!("Von: {}\nDatum: {}\nBetreff: {}\n\n{}", s.from, s.date, s.subject, truncate_middle(&m.body, 1500)),
                    json!(m),
                ))
            }
            "email_digest" => {
                let list = self.reader.search(q()?, limit.min(8)).await.map_err(err)?;
                let per = (2400 / list.len().max(1)).max(150);
                let mut out = String::new();
                for s in &list {
                    let body = self.reader.get(&s.id).await.map(|m| m.body).unwrap_or_else(|_| s.preview.clone());
                    out.push_str(&format!("### {} — {} ({})\n{}\n\n", s.subject, s.from, s.date, truncate_middle(&body, per)));
                }
                Ok(ToolOutput::with_data(out, json!({"count": list.len()})))
            }
            _ => Err(ToolError::Forbidden("unbekannte Mail-Operation".into())),
        }
    }
}

pub fn tools(reader: Arc<dyn MailReader>) -> Vec<Arc<dyn Tool>> {
    [&SEARCH, &READ, &RECENT, &DIGEST]
        .into_iter()
        .map(|spec| Arc::new(MailTool { reader: reader.clone(), spec }) as Arc<dyn Tool>)
        .collect()
}
