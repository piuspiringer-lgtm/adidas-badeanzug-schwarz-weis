//! Microsoft Teams: **nur lesen und durchsuchen** (Microsoft Graph).
//!
//! Einziger POST ist `/search/query` (Microsoft Search, liest nur).
//! Senden, Bearbeiten, Löschen oder Reagieren existiert im Code nicht.

use crate::http::{HttpError, ReadOnlyApi};
use crate::mail::html_to_text;
use async_trait::async_trait;
use jarvis_context::truncate_middle;
use jarvis_permissions::{Access, CallFacts, Capability, Confirmation, Integration, RiskLevel, ToolSpec};
use jarvis_runtime::{Tool, ToolError, ToolOutput};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Einzige erlaubte POST-Pfade für Graph (Suche).
pub const GRAPH_ALLOWED_POST: &[&str] = &["/search/query"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Chat {
    pub id: String,
    pub topic: String,
    pub kind: String,
    pub updated: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatMessage {
    pub from: String,
    pub date: String,
    pub text: String,
    pub chat_id: String,
}

pub struct TeamsReader {
    pub api: ReadOnlyApi,
}

fn safe_id(id: &str) -> Result<&str, HttpError> {
    if id.is_empty() || id.len() > 512 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.@".contains(c)) {
        return Err(HttpError::Blocked("ungültige Chat-ID".into()));
    }
    Ok(id)
}

impl TeamsReader {
    pub async fn chats(&self, limit: usize) -> Result<Vec<Chat>, HttpError> {
        let v = self.api.get(&format!("/me/chats?$top={}", limit.min(50)), &[]).await?;
        Ok(v["value"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| Chat {
                        id: c["id"].as_str().unwrap_or_default().into(),
                        topic: c["topic"].as_str().unwrap_or("(ohne Titel)").into(),
                        kind: c["chatType"].as_str().unwrap_or_default().into(),
                        updated: c["lastUpdatedDateTime"].as_str().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn messages(&self, chat_id: &str, limit: usize) -> Result<Vec<ChatMessage>, HttpError> {
        let v = self.api.get(&format!("/me/chats/{}/messages?$top={}", safe_id(chat_id)?, limit.min(50)), &[]).await?;
        Ok(v["value"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|m| m["messageType"].as_str().unwrap_or("message") == "message")
                    .map(|m| ChatMessage {
                        from: m["from"]["user"]["displayName"].as_str().unwrap_or("?").into(),
                        date: m["createdDateTime"].as_str().unwrap_or_default().into(),
                        text: html_to_text(m["body"]["content"].as_str().unwrap_or_default()),
                        chat_id: chat_id.into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ChatMessage>, HttpError> {
        let body = json!({"requests": [{
            "entityTypes": ["chatMessage"],
            "query": {"queryString": query},
            "from": 0,
            "size": limit.min(25)
        }]});
        let v = self.api.post_search("/search/query", &body).await?;
        let mut out = vec![];
        for hc in v["value"].as_array().into_iter().flatten() {
            for c in hc["hitsContainers"].as_array().into_iter().flatten() {
                for h in c["hits"].as_array().into_iter().flatten() {
                    let r = &h["resource"];
                    out.push(ChatMessage {
                        from: r["from"]["emailAddress"]["name"].as_str().unwrap_or("?").into(),
                        date: r["createdDateTime"].as_str().unwrap_or_default().into(),
                        text: html_to_text(h["summary"].as_str().unwrap_or_default()),
                        chat_id: r["chatId"].as_str().unwrap_or_default().into(),
                    });
                }
            }
        }
        Ok(out)
    }
}

const CAPS: &[Capability] = &[Capability::TeamsRead, Capability::NetworkGet];

macro_rules! spec {
    ($name:literal, $desc:literal) => {
        ToolSpec {
            name: $name,
            description: $desc,
            integration: Integration::Teams,
            access: Access::Read,
            risk: RiskLevel::Low,
            confirmation: Confirmation::Never,
            capabilities: CAPS,
            services: &[],
        }
    };
}

static CHATS: ToolSpec = spec!("teams_chats", "Listet Teams-Chats (nur lesen).");
static MESSAGES: ToolSpec = spec!("teams_messages", "Liest Nachrichten eines Teams-Chats (nur lesen).");
static SEARCH: ToolSpec = spec!("teams_search", "Durchsucht Teams-Nachrichten (nur lesen).");

pub struct TeamsTool {
    reader: Arc<TeamsReader>,
    spec: &'static ToolSpec,
}

fn msgs_text(v: &[ChatMessage]) -> String {
    v.iter().map(|m| format!("- {} | {}: {}", m.date, m.from, truncate_middle(&m.text, 200))).collect::<Vec<_>>().join("\n")
}

#[async_trait]
impl Tool for TeamsTool {
    fn spec(&self) -> &ToolSpec {
        self.spec
    }
    fn parameters(&self) -> Value {
        match self.spec.name {
            "teams_messages" => json!({"type":"object","properties":{"chat_id":{"type":"string"},"limit":{"type":"integer"}},"required":["chat_id"]}),
            "teams_search" => json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}),
            _ => json!({"type":"object","properties":{"limit":{"type":"integer"}}}),
        }
    }
    fn facts(&self, _a: &Value) -> Result<CallFacts, ToolError> {
        Ok(CallFacts { preview: self.spec.name.into(), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
        let e = |e: HttpError| ToolError::Failed(e.to_string());
        match self.spec.name {
            "teams_chats" => {
                let c = self.reader.chats(limit).await.map_err(e)?;
                let t = c.iter().map(|c| format!("- [{}] {} ({}, {})", c.id, c.topic, c.kind, c.updated)).collect::<Vec<_>>().join("\n");
                Ok(ToolOutput::with_data(t, json!(c)))
            }
            "teams_messages" => {
                let id = a.get("chat_id").and_then(Value::as_str).ok_or_else(|| ToolError::InvalidArgs("'chat_id' fehlt".into()))?;
                let m = self.reader.messages(id, limit).await.map_err(e)?;
                Ok(ToolOutput::with_data(msgs_text(&m), json!(m)))
            }
            "teams_search" => {
                let q = a.get("query").and_then(Value::as_str).ok_or_else(|| ToolError::InvalidArgs("'query' fehlt".into()))?;
                let m = self.reader.search(q, limit).await.map_err(e)?;
                Ok(ToolOutput::with_data(msgs_text(&m), json!(m)))
            }
            _ => Err(ToolError::Forbidden("unbekannte Teams-Operation".into())),
        }
    }
}

pub fn tools(reader: Arc<TeamsReader>) -> Vec<Arc<dyn Tool>> {
    [&CHATS, &MESSAGES, &SEARCH]
        .into_iter()
        .map(|spec| Arc::new(TeamsTool { reader: reader.clone(), spec }) as Arc<dyn Tool>)
        .collect()
}
