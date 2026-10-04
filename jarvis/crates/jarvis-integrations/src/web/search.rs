//! Suchanbieter. Reihenfolge: SearXNG (falls konfiguriert) → Brave Search API
//! (falls API-Key) → DuckDuckGo-HTML (ohne Key, als Fallback).

use crate::http::{HttpError, WebClient};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[async_trait]
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchHit>, HttpError>;
}

fn enc(q: &str) -> String {
    url::form_urlencoded::byte_serialize(q.as_bytes()).collect()
}

/// Brave Search API (API-Key nötig, kostenloses Kontingent laut Anbieter).
pub struct Brave {
    pub client: WebClient,
    pub api_key: String,
    pub endpoint: String,
}

impl Brave {
    pub fn new(client: WebClient, api_key: String) -> Self {
        Self { client, api_key, endpoint: "https://api.search.brave.com/res/v1/web/search".into() }
    }
}

#[async_trait]
impl SearchProvider for Brave {
    fn id(&self) -> &'static str {
        "brave"
    }
    async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchHit>, HttpError> {
        let url = format!("{}?q={}&count={}&search_lang=de", self.endpoint, enc(query), count.min(20));
        let b = self
            .client
            .get_text(&url, &[("Accept", "application/json"), ("X-Subscription-Token", &self.api_key)])
            .await?;
        let v: Value = serde_json::from_str(&b.body).map_err(|e| HttpError::Network(e.to_string()))?;
        Ok(v["web"]["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| SearchHit {
                        title: r["title"].as_str().unwrap_or_default().into(),
                        url: r["url"].as_str().unwrap_or_default().into(),
                        snippet: strip_tags(r["description"].as_str().unwrap_or_default()),
                    })
                    .filter(|h| !h.url.is_empty())
                    .take(count)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Selbst gehostete SearXNG-Instanz (JSON-API muss aktiviert sein).
pub struct Searxng {
    pub client: WebClient,
    pub base_url: String,
}

#[async_trait]
impl SearchProvider for Searxng {
    fn id(&self) -> &'static str {
        "searxng"
    }
    async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchHit>, HttpError> {
        let url = format!("{}/search?q={}&format=json&language=de", self.base_url.trim_end_matches('/'), enc(query));
        let b = self.client.get_text(&url, &[("Accept", "application/json")]).await?;
        let v: Value = serde_json::from_str(&b.body).map_err(|e| HttpError::Network(e.to_string()))?;
        Ok(v["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| SearchHit {
                        title: r["title"].as_str().unwrap_or_default().into(),
                        url: r["url"].as_str().unwrap_or_default().into(),
                        snippet: r["content"].as_str().unwrap_or_default().into(),
                    })
                    .filter(|h| !h.url.is_empty())
                    .take(count)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// DuckDuckGo HTML-Version (ohne Key). Fallback; kann abgeschaltet werden.
pub struct DuckDuckGo {
    pub client: WebClient,
    pub endpoint: String,
}

impl DuckDuckGo {
    pub fn new(client: WebClient) -> Self {
        Self { client, endpoint: "https://html.duckduckgo.com/html/".into() }
    }
}

fn strip_tags(s: &str) -> String {
    let frag = scraper::Html::parse_fragment(s);
    frag.root_element().text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse_ddg_html(html: &str, count: usize) -> Vec<SearchHit> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let res = Selector::parse(".result").unwrap();
    let a = Selector::parse("a.result__a").unwrap();
    let sn = Selector::parse(".result__snippet").unwrap();
    let mut out = vec![];
    for r in doc.select(&res) {
        let Some(link) = r.select(&a).next() else { continue };
        let href = link.value().attr("href").unwrap_or("");
        // DDG verpackt Ziele als //duckduckgo.com/l/?uddg=<url>
        let url = url::Url::parse(&format!("https:{href}"))
            .ok()
            .and_then(|u| u.query_pairs().find(|(k, _)| k == "uddg").map(|(_, v)| v.into_owned()))
            .unwrap_or_else(|| href.to_string());
        if !url.starts_with("http") || url.contains("duckduckgo.com/y.js") {
            continue;
        }
        out.push(SearchHit {
            title: link.text().collect::<String>().trim().to_string(),
            url,
            snippet: r.select(&sn).next().map(|s| s.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default(),
        });
        if out.len() >= count {
            break;
        }
    }
    out
}

#[async_trait]
impl SearchProvider for DuckDuckGo {
    fn id(&self) -> &'static str {
        "duckduckgo"
    }
    async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchHit>, HttpError> {
        let b = self.client.get_text(&format!("{}?q={}&kl=de-de", self.endpoint, enc(query)), &[]).await?;
        Ok(parse_ddg_html(&b.body, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddg_parsing() {
        let html = r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftauri.app%2Fde%2F&rut=x">Tauri <b>2</b></a>
            <a class="result__snippet">Baue kleine, schnelle Apps.</a></div>
            <div class="result"><a class="result__a" href="https://duckduckgo.com/y.js?ad=1">Anzeige</a></div>"#;
        let h = parse_ddg_html(html, 5);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].url, "https://tauri.app/de/");
        assert_eq!(h[0].title, "Tauri 2");
        assert_eq!(h[0].snippet, "Baue kleine, schnelle Apps.");
    }
}
