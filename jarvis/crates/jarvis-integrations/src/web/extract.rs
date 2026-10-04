//! HTML → lesbarer Haupttext + Links. Navigation, Werbung, Skripte usw.
//! werden verworfen, damit nur Inhalt beim Modell ankommt.

use scraper::{ElementRef, Html, Selector};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Link {
    pub url: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Extracted {
    pub title: String,
    pub text: String,
    pub links: Vec<Link>,
}

const SKIP: &[&str] = &["script", "style", "noscript", "nav", "header", "footer", "aside", "form", "svg", "iframe", "button", "template"];
const BLOCKS: &str = "h1, h2, h3, h4, h5, h6, p, li, pre, blockquote, td, th, dt, dd, figcaption";

fn inside_skipped(el: &ElementRef) -> bool {
    el.ancestors().filter_map(ElementRef::wrap).any(|a| {
        let n = a.value().name();
        SKIP.contains(&n)
            || a.value().attr("role").is_some_and(|r| matches!(r, "navigation" | "banner" | "contentinfo" | "complementary"))
            || a.value().attr("aria-hidden") == Some("true")
            || a.value().classes().any(|c| {
                let c = c.to_ascii_lowercase();
                c.contains("cookie") || c.contains("advert") || c.contains("newsletter") || c == "ad" || c.contains("sidebar")
            })
    })
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn extract(html: &str, base_url: &str) -> Extracted {
    let doc = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|s| doc.select(&s).next())
        .map(|t| clean(&t.text().collect::<String>()))
        .unwrap_or_default();

    let root_sel = Selector::parse("article, main, [role=main]").unwrap();
    let roots: Vec<ElementRef> = {
        let r: Vec<_> = doc.select(&root_sel).collect();
        if r.is_empty() {
            doc.select(&Selector::parse("body").unwrap()).collect()
        } else {
            r
        }
    };
    let block = Selector::parse(BLOCKS).unwrap();
    let mut parts: Vec<String> = Vec::new();
    for root in &roots {
        for el in root.select(&block) {
            if inside_skipped(&el) {
                continue;
            }
            // Verschachtelte Blöcke (li > p) nicht doppelt ausgeben.
            if el.ancestors().filter_map(ElementRef::wrap).take_while(|a| a != root).any(|a| block.matches(&a)) {
                continue;
            }
            let t = clean(&el.text().collect::<String>());
            if t.chars().count() < 3 {
                continue;
            }
            let t = match el.value().name() {
                "h1" | "h2" | "h3" => format!("\n## {t}"),
                "li" => format!("- {t}"),
                _ => t,
            };
            if parts.last() != Some(&t) {
                parts.push(t);
            }
        }
    }
    if parts.is_empty() {
        if let Some(r) = roots.first() {
            parts.push(clean(&r.text().collect::<String>()));
        }
    }

    let base = url::Url::parse(base_url).ok();
    let a_sel = Selector::parse("a[href]").unwrap();
    let mut links: Vec<Link> = Vec::new();
    for root in &roots {
        for a in root.select(&a_sel) {
            if inside_skipped(&a) {
                continue;
            }
            let href = a.value().attr("href").unwrap_or("");
            let Some(abs) = base.as_ref().and_then(|b| b.join(href).ok()).or_else(|| url::Url::parse(href).ok()) else { continue };
            if !matches!(abs.scheme(), "http" | "https") {
                continue;
            }
            let mut abs = abs;
            abs.set_fragment(None);
            let text = clean(&a.text().collect::<String>());
            let u = abs.to_string();
            if text.is_empty() || base.as_ref().is_some_and(|b| b.as_str() == u) || links.iter().any(|l| l.url == u) {
                continue;
            }
            links.push(Link { url: u, text });
        }
    }
    Extracted { title, text: parts.join("\n").trim().to_string(), links }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><head><title> Testseite </title><script>var x=1;</script></head>
    <body><nav><a href="/home">Home</a><p>Menü</p></nav>
    <div class="cookie-banner"><p>Wir nutzen Cookies</p></div>
    <main><h1>Überschrift</h1><p>Erster   Absatz mit <b>Inhalt</b>.</p>
    <ul><li>Punkt eins</li><li>Punkt zwei</li></ul>
    <p>Mehr unter <a href="/doku#teil">der Doku</a> und <a href="javascript:alert(1)">böse</a>.</p></main>
    <footer><p>Impressum</p></footer></body></html>"#;

    #[test]
    fn extracts_main_content_only() {
        let e = extract(PAGE, "https://example.org/start");
        assert_eq!(e.title, "Testseite");
        assert!(e.text.contains("## Überschrift"));
        assert!(e.text.contains("Erster Absatz mit Inhalt."));
        assert!(e.text.contains("- Punkt zwei"));
        for bad in ["Menü", "Cookies", "Impressum", "var x"] {
            assert!(!e.text.contains(bad), "{bad}");
        }
        assert_eq!(e.links, vec![Link { url: "https://example.org/doku".into(), text: "der Doku".into() }]);
    }
}
