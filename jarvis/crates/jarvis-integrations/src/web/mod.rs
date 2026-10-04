//! Web-Recherche mit Fokus auf Token-Effizienz:
//!
//! 1. Zuerst bereits recherchiertes Wissen (SQLite-FTS) prüfen.
//! 2. Dann Suchergebnisse (Titel + Snippet) nutzen – nicht ganze Seiten.
//! 3. Nur die besten Seiten bei Bedarf laden, Haupttext extrahieren,
//!    in Abschnitte teilen, cachen.
//! 4. Abschnitte per BM25 bewerten, Duplikate entfernen und nur so viel
//!    ans Modell geben, wie das Token-Budget erlaubt – mit Quellenangabe.

pub mod extract;
pub mod search;

use crate::http::{HttpError, WebClient};
use async_trait::async_trait;
use jarvis_context::{chunk_text, estimate_tokens, rank_chunks, truncate_middle, Snippet};
use jarvis_memory::Memory;
use jarvis_permissions::{Access, CallFacts, Capability, Confirmation, Integration, RiskLevel, ToolSpec};
use jarvis_runtime::{Tool, ToolError, ToolOutput};
use search::{SearchHit, SearchProvider};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub const SEARCH_TTL_SECS: i64 = 6 * 3600;
pub const PAGE_TTL_SECS: i64 = 24 * 3600;
pub const KNOWLEDGE_TTL_SECS: i64 = 7 * 24 * 3600;
const CHUNK_TOKENS: usize = 220;

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
    pub links: Vec<extract::Link>,
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResearchResult {
    pub query: String,
    pub snippets: Vec<Snippet>,
    pub sources: Vec<String>,
    pub used_cache_only: bool,
    pub pages_fetched: usize,
    pub tokens: usize,
}

impl ResearchResult {
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (i, sn) in self.snippets.iter().enumerate() {
            s.push_str(&format!("[{}] {}\n", i + 1, sn.text));
        }
        s.push_str("\nQuellen:\n");
        for (i, sn) in self.snippets.iter().enumerate() {
            s.push_str(&format!("[{}] {}\n", i + 1, sn.source));
        }
        s
    }
}

pub struct Research {
    client: WebClient,
    providers: Vec<Arc<dyn SearchProvider>>,
    memory: Memory,
}

impl Research {
    pub fn new(client: WebClient, providers: Vec<Arc<dyn SearchProvider>>, memory: Memory) -> Self {
        Self { client, providers, memory }
    }

    pub fn provider_ids(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// Suche mit Cache; probiert Anbieter der Reihe nach.
    pub async fn search(&self, query: &str, count: usize) -> Result<(Vec<SearchHit>, bool), HttpError> {
        let mut last_err = HttpError::Blocked("kein Suchanbieter konfiguriert".into());
        for p in &self.providers {
            if let Ok(Some(cached)) = self.memory.cached_search(query, p.id(), SEARCH_TTL_SECS) {
                if let Ok(h) = serde_json::from_str::<Vec<SearchHit>>(&cached) {
                    return Ok((h.into_iter().take(count).collect(), true));
                }
            }
            match p.search(query, count).await {
                Ok(h) if !h.is_empty() => {
                    let _ = self.memory.cache_search(query, p.id(), &serde_json::to_string(&h).unwrap());
                    return Ok((h, false));
                }
                Ok(_) => last_err = HttpError::Network(format!("{}: keine Treffer", p.id())),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// Lädt eine Seite (oder nimmt sie aus dem Cache) und speichert ihre Abschnitte.
    pub async fn fetch(&self, url: &str) -> Result<Page, HttpError> {
        if let Ok(Some(c)) = self.memory.cached_page(url, PAGE_TTL_SECS) {
            return Ok(Page { url: c.url, title: c.title, text: c.text, links: vec![], from_cache: true });
        }
        let b = self.client.get_text(url, &[("Accept", "text/html,text/plain;q=0.9,*/*;q=0.1")]).await?;
        let ct = b.content_type.as_str();
        let head = b.body.trim_start().chars().take(512).collect::<String>().to_ascii_lowercase();
        let looks_html = head.starts_with("<!doctype html") || head.contains("<html") || head.contains("<body");
        let (title, text, links) = if ct.contains("html") || ct.is_empty() || (ct.starts_with("text/plain") && looks_html) {
            let e = extract::extract(&b.body, &b.final_url);
            (e.title, e.text, e.links)
        } else if ct.starts_with("text/") || ct.contains("json") {
            (String::new(), b.body.clone(), vec![])
        } else {
            return Err(HttpError::Blocked(format!("Inhaltstyp '{ct}' wird nicht verarbeitet")));
        };
        let chunks = chunk_text(&text, CHUNK_TOKENS);
        let _ = self.memory.cache_page(url, &title, &text, &chunks);
        Ok(Page { url: url.into(), title, text, links, from_cache: false })
    }

    /// Token-effiziente Recherche.
    pub async fn research(&self, query: &str, budget_tokens: usize, max_pages: usize) -> Result<ResearchResult, HttpError> {
        // 1) Vorhandenes Wissen
        let known: Vec<(String, String)> = self
            .memory
            .search_knowledge(query, KNOWLEDGE_TTL_SECS, 30)
            .unwrap_or_default()
            .into_iter()
            .map(|k| (k.url, k.text))
            .collect();
        let from_memory = rank_chunks(query, &known, budget_tokens);
        let distinct_sources = from_memory.iter().map(|s| &s.source).collect::<std::collections::HashSet<_>>().len();
        if from_memory.len() >= 3 && distinct_sources >= 2 {
            return Ok(Self::finish(query, from_memory, true, 0));
        }

        // 2) Suchergebnisse (billig)
        let (hits, _) = self.search(query, 8).await?;
        let mut candidates: Vec<(String, String)> = known;

        // 3) Nur die relevantesten Seiten vollständig laden
        let ranked_hits = rank_chunks(query, &hits.iter().map(|h| (h.url.clone(), format!("{} {}", h.title, h.snippet))).collect::<Vec<_>>(), usize::MAX);
        let mut order: Vec<String> = ranked_hits.into_iter().map(|s| s.source).collect();
        for h in &hits {
            if !order.contains(&h.url) {
                order.push(h.url.clone());
            }
        }
        let mut fetched = 0;
        let mut loaded: Vec<String> = vec![];
        for url in order.into_iter().take(max_pages) {
            if let Ok(p) = self.fetch(&url).await {
                fetched += usize::from(!p.from_cache);
                candidates.extend(chunk_text(&p.text, CHUNK_TOKENS).into_iter().map(|c| (url.clone(), c)));
                loaded.push(url);
            }
        }
        // Such-Snippets nur für Seiten, deren Volltext nicht geladen wurde.
        candidates.extend(
            hits.iter().filter(|h| !loaded.contains(&h.url)).map(|h| (h.url.clone(), format!("{}: {}", h.title, h.snippet))),
        );
        let snippets = rank_chunks(query, &candidates, budget_tokens);
        Ok(Self::finish(query, snippets, false, fetched))
    }

    fn finish(query: &str, snippets: Vec<Snippet>, cache_only: bool, fetched: usize) -> ResearchResult {
        let mut sources: Vec<String> = vec![];
        for s in &snippets {
            if !sources.contains(&s.source) {
                sources.push(s.source.clone());
            }
        }
        let tokens = snippets.iter().map(|s| estimate_tokens(&s.text)).sum();
        ResearchResult { query: query.into(), snippets, sources, used_cache_only: cache_only, pages_fetched: fetched, tokens }
    }

    /// Links einer Seite nach Relevanz zu `about` sortiert (ohne sie zu laden).
    pub async fn links(&self, url: &str, about: Option<&str>, limit: usize) -> Result<Vec<extract::Link>, HttpError> {
        // Für Links immer frisch laden (der Cache speichert nur Text).
        let b = self.client.get_text(url, &[("Accept", "text/html")]).await?;
        let e = extract::extract(&b.body, &b.final_url);
        let mut links = e.links;
        if let Some(q) = about {
            let ranked = rank_chunks(q, &links.iter().map(|l| (l.url.clone(), format!("{} {}", l.text, l.url))).collect::<Vec<_>>(), usize::MAX);
            let order: Vec<String> = ranked.into_iter().map(|s| s.source).collect();
            links.sort_by_key(|l| order.iter().position(|u| *u == l.url).unwrap_or(usize::MAX));
        }
        links.truncate(limit);
        Ok(links)
    }
}

fn arg_str<'a>(a: &'a Value, k: &str) -> Result<&'a str, ToolError> {
    a.get(k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or_else(|| ToolError::InvalidArgs(format!("'{k}' fehlt")))
}

fn check_query(q: &str) -> Result<(), ToolError> {
    if q.chars().count() > 300 {
        return Err(ToolError::InvalidArgs("Suchanfrage zu lang (max. 300 Zeichen)".into()));
    }
    Ok(())
}

fn http_err(e: HttpError) -> ToolError {
    match e {
        HttpError::Blocked(m) => ToolError::Forbidden(m),
        e => ToolError::Failed(e.to_string()),
    }
}

const WEB_CAPS: &[Capability] = &[Capability::NetworkGet, Capability::MemoryRead, Capability::MemoryWrite];

macro_rules! web_spec {
    ($name:literal, $desc:literal) => {
        ToolSpec {
            name: $name,
            description: $desc,
            integration: Integration::Web,
            // Cache-Schreiben ist interne Buchführung, kein Benutzerdaten-Schreiben.
            access: Access::Write,
            risk: RiskLevel::Low,
            confirmation: Confirmation::Never,
            capabilities: WEB_CAPS,
            services: &[],
        }
    };
}

pub struct WebSearch(pub Arc<Research>);
pub struct WebFetch(pub Arc<Research>);
pub struct WebResearch(pub Arc<Research>);
pub struct WebLinks(pub Arc<Research>);

static SEARCH_SPEC: ToolSpec = web_spec!("web_search", "Websuche: liefert Titel, URL und Kurztext (keine ganzen Seiten).");
static FETCH_SPEC: ToolSpec = web_spec!("web_fetch", "Lädt eine Webseite (GET). Mit 'query' nur relevante Abschnitte.");
static RESEARCH_SPEC: ToolSpec = web_spec!("web_research", "Recherchiert eine Frage über mehrere Quellen, nutzt Cache, liefert belegte Kernaussagen.");
static LINKS_SPEC: ToolSpec = web_spec!("web_links", "Listet Links einer Seite, sortiert nach Relevanz zu 'about'.");

#[async_trait]
impl Tool for WebSearch {
    fn spec(&self) -> &ToolSpec {
        &SEARCH_SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"query":{"type":"string"},"count":{"type":"integer"}},"required":["query"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let q = arg_str(a, "query")?;
        check_query(q)?;
        Ok(CallFacts { preview: format!("Websuche: {q}"), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let q = arg_str(&a, "query")?;
        let n = a.get("count").and_then(Value::as_u64).unwrap_or(6).min(10) as usize;
        let (hits, cached) = self.0.search(q, n).await.map_err(http_err)?;
        let text = hits.iter().enumerate().map(|(i, h)| format!("{}. {} — {}\n   {}", i + 1, h.title, h.url, h.snippet)).collect::<Vec<_>>().join("\n");
        Ok(ToolOutput::with_data(text, json!({"hits": hits, "cached": cached})))
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn spec(&self) -> &ToolSpec {
        &FETCH_SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"url":{"type":"string"},"query":{"type":"string"},"max_tokens":{"type":"integer"}},"required":["url"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let u = arg_str(a, "url")?;
        crate::http::check_url(u, false).map_err(http_err)?;
        Ok(CallFacts { preview: format!("Seite laden: {u}"), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let u = arg_str(&a, "url")?;
        let max = a.get("max_tokens").and_then(Value::as_u64).unwrap_or(1200).min(4000) as usize;
        let p = self.0.fetch(u).await.map_err(http_err)?;
        let body = match a.get("query").and_then(Value::as_str) {
            Some(q) => {
                let chunks: Vec<(String, String)> = chunk_text(&p.text, CHUNK_TOKENS).into_iter().map(|c| (p.url.clone(), c)).collect();
                let sn = rank_chunks(q, &chunks, max);
                if sn.is_empty() { truncate_middle(&p.text, max) } else { sn.into_iter().map(|s| s.text).collect::<Vec<_>>().join("\n…\n") }
            }
            None => truncate_middle(&p.text, max),
        };
        let text = format!("# {}\n{}\n\n{}", p.title, p.url, body);
        Ok(ToolOutput::with_data(text, json!({"url": p.url, "title": p.title, "from_cache": p.from_cache, "links": p.links.len()})))
    }
}

#[async_trait]
impl Tool for WebResearch {
    fn spec(&self) -> &ToolSpec {
        &RESEARCH_SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"query":{"type":"string"},"max_sources":{"type":"integer"},"max_tokens":{"type":"integer"}},"required":["query"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let q = arg_str(a, "query")?;
        check_query(q)?;
        Ok(CallFacts { preview: format!("Recherche: {q}"), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let q = arg_str(&a, "query")?;
        let pages = a.get("max_sources").and_then(Value::as_u64).unwrap_or(3).min(6) as usize;
        let budget = a.get("max_tokens").and_then(Value::as_u64).unwrap_or(1500).min(4000) as usize;
        let r = self.0.research(q, budget, pages).await.map_err(http_err)?;
        Ok(ToolOutput::with_data(r.render(), serde_json::to_value(&r).unwrap()))
    }
}

#[async_trait]
impl Tool for WebLinks {
    fn spec(&self) -> &ToolSpec {
        &LINKS_SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"url":{"type":"string"},"about":{"type":"string"},"limit":{"type":"integer"}},"required":["url"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let u = arg_str(a, "url")?;
        crate::http::check_url(u, false).map_err(http_err)?;
        Ok(CallFacts { preview: format!("Links von {u}"), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let u = arg_str(&a, "url")?;
        let n = a.get("limit").and_then(Value::as_u64).unwrap_or(15).min(50) as usize;
        let links = self.0.links(u, a.get("about").and_then(Value::as_str), n).await.map_err(http_err)?;
        let text = links.iter().map(|l| format!("- {} — {}", l.text, l.url)).collect::<Vec<_>>().join("\n");
        Ok(ToolOutput::with_data(text, json!(links)))
    }
}

pub fn tools(r: Arc<Research>) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(WebSearch(r.clone())), Arc::new(WebFetch(r.clone())), Arc::new(WebResearch(r.clone())), Arc::new(WebLinks(r))]
}
