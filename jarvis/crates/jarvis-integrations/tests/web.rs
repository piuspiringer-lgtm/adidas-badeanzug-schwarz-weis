use jarvis_integrations::http::WebClient;
use jarvis_integrations::web::search::{DuckDuckGo, SearchProvider};
use jarvis_integrations::web::{self, Research};
use jarvis_memory::Memory;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn page(title: &str, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(
        format!("<html><head><title>{title}</title></head><body><nav>Menü Login</nav><main>{body}</main><footer>Impressum</footer></body></html>"),
        "text/html; charset=utf-8",
    )
}

async fn site() -> MockServer {
    let s = MockServer::start().await;
    let base = s.uri();
    let serp = format!(
        r#"<div class="result"><a class="result__a" href="{base}/ollama">Ollama Speicher</a><a class="result__snippet">Wie Ollama Modelle entlädt.</a></div>
           <div class="result"><a class="result__a" href="{base}/kopie">Kopie</a><a class="result__snippet">Gespiegelter Artikel.</a></div>
           <div class="result"><a class="result__a" href="{base}/kuchen">Kuchenrezepte</a><a class="result__snippet">Apfelstrudel.</a></div>"#
    );
    Mock::given(method("GET")).and(path("/html/")).respond_with(ResponseTemplate::new(200).set_body_string(serp)).mount(&s).await;
    let art = "<p>Ollama entlädt Modelle nach Ablauf von keep_alive automatisch aus dem Speicher.</p><p>Mit OLLAMA_MAX_LOADED_MODELS=1 bleibt höchstens ein Modell geladen.</p><p><a href=\"/ollama/faq\">Ollama FAQ zum Speicher</a> und <a href=\"/blog\">Blog</a></p>";
    Mock::given(method("GET")).and(path("/ollama")).respond_with(page("Ollama", art)).mount(&s).await;
    // Gespiegelte Seite mit identischem Inhalt → Duplikat.
    Mock::given(method("GET")).and(path("/kopie")).respond_with(page("Kopie", art)).mount(&s).await;
    Mock::given(method("GET")).and(path("/kuchen")).respond_with(page("Kuchen", "<p>Apfelstrudel braucht Äpfel und Zimt.</p>")).mount(&s).await;
    Mock::given(method("GET")).and(path("/riesig")).respond_with(page("Riesig", &"<p>Füllwort Text.</p>".repeat(20_000))).mount(&s).await;
    s
}

fn research(s: &MockServer, mem: Memory) -> Research {
    let client = WebClient::new_allowing_private();
    let mut ddg = DuckDuckGo::new(client.clone());
    ddg.endpoint = format!("{}/html/", s.uri());
    Research::new(client, vec![Arc::new(ddg) as Arc<dyn SearchProvider>], mem)
}

#[tokio::test]
async fn research_is_token_efficient_deduplicated_and_cached() {
    let s = site().await;
    let mem = Memory::in_memory().unwrap();
    let r = research(&s, mem.clone());
    let q = "Wie entlädt Ollama Modelle aus dem Speicher?";

    let first = r.research(q, 400, 3).await.unwrap();
    assert!(first.tokens <= 400, "Budget: {}", first.tokens);
    assert!(!first.used_cache_only);
    assert!(first.snippets[0].text.contains("keep_alive"), "{:?}", first.snippets);
    // Spiegel-Seite liefert keinen zweiten identischen Abschnitt.
    let n_keep = first.snippets.iter().filter(|s| s.text.contains("keep_alive automatisch")).count();
    assert_eq!(n_keep, 1, "{:?}", first.snippets);
    assert!(first.snippets.iter().all(|s| !s.text.contains("Impressum") && !s.text.contains("Menü")));
    assert!(first.snippets.iter().all(|s| !s.text.contains("Apfelstrudel braucht")), "irrelevantes nicht übernommen");
    assert!(first.render().contains("Quellen:"));

    // Zweiter Durchlauf: Suche und Seiten kommen aus dem Cache (Mocks erwarten je genau 1 Aufruf).
    let second = r.research("Ollama Modelle Speicher entladen keep_alive", 400, 3).await.unwrap();
    assert!(!second.snippets.is_empty());
    assert_eq!(second.pages_fetched, 0, "keine Seite erneut geladen");
    let third = r.research(q, 400, 3).await.unwrap();
    assert_eq!(third.pages_fetched, 0);
    let reqs = s.received_requests().await.unwrap();
    let count = |p: &str| reqs.iter().filter(|r| r.url.path() == p).count();
    assert_eq!(count("/html/"), 2, "2 verschiedene Anfragen; die wiederholte kam aus dem Cache");
    assert_eq!(count("/ollama"), 1, "Seite nur einmal geladen");
}

#[tokio::test]
async fn fetch_extracts_and_links_are_ranked() {
    let s = site().await;
    let r = research(&s, Memory::in_memory().unwrap());
    let p = r.fetch(&format!("{}/ollama", s.uri())).await.unwrap();
    assert_eq!(p.title, "Ollama");
    assert!(!p.text.contains("Menü"));
    let links = r.links(&format!("{}/ollama", s.uri()), Some("FAQ Speicher"), 5).await.unwrap();
    assert!(links[0].url.ends_with("/ollama/faq"), "{links:?}");
}

#[tokio::test]
async fn huge_pages_are_truncated_for_the_model() {
    let s = site().await;
    let mem = Memory::in_memory().unwrap();
    let r = Arc::new(research(&s, mem));
    let tools = web::tools(r);
    let fetch = tools.iter().find(|t| t.spec().name == "web_fetch").unwrap();
    let out = fetch.call(serde_json::json!({"url": format!("{}/riesig", s.uri()), "max_tokens": 300})).await.unwrap();
    assert!(jarvis_context::estimate_tokens(&out.text) < 400, "{}", out.text.len());
}

#[tokio::test]
async fn production_client_refuses_local_targets() {
    let s = site().await;
    let r = Research::new(WebClient::new(), vec![], Memory::in_memory().unwrap());
    assert!(r.fetch(&format!("{}/ollama", s.uri())).await.is_err());
    assert!(r.fetch("http://169.254.169.254/latest/meta-data").await.is_err());
}
