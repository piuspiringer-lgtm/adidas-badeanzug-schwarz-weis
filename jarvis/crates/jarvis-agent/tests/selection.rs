//! Regressionstests für die Tool-Auswahl mit den echten Tool-Definitionen.

use async_trait::async_trait;
use jarvis_agent::select::select_tools;
use jarvis_integrations::fs::{self, FsSandbox, SystemOpener};
use jarvis_integrations::http::HttpError;
use jarvis_integrations::mail::{self, MailMessage, MailReader, MailSummary};
use jarvis_memory::Memory;
use jarvis_permissions::Access;
use jarvis_runtime::tool::ToolInfo;
use std::sync::Arc;

struct NoMail;
#[async_trait]
impl MailReader for NoMail {
    fn provider(&self) -> &'static str {
        "test"
    }
    async fn search(&self, _: &str, _: usize) -> Result<Vec<MailSummary>, HttpError> {
        Ok(vec![])
    }
    async fn recent(&self, _: usize) -> Result<Vec<MailSummary>, HttpError> {
        Ok(vec![])
    }
    async fn get(&self, _: &str) -> Result<MailMessage, HttpError> {
        Err(HttpError::Blocked("test".into()))
    }
}

fn all_tools(mem: &Memory) -> Vec<ToolInfo> {
    let sb = Arc::new(FsSandbox::new(vec![], vec![]));
    fs::tools(sb, Arc::new(SystemOpener))
        .into_iter()
        .chain(mail::tools(Arc::new(NoMail)))
        .chain(jarvis_integrations::memory::tools(mem.clone()))
        .map(|t| ToolInfo { spec: t.spec().clone(), parameters: t.parameters() })
        .collect()
}

fn pick(q: &str) -> Vec<&'static str> {
    let mem = Memory::in_memory().unwrap();
    let all = all_tools(&mem);
    select_tools(&all, q, &mem, 5).tools.iter().map(|t| t.spec.name).collect()
}

/// Regression (Mac-Test): früher 6 Tools inkl. fs_move/fs_rename/fs_reveal.
#[test]
fn pdf_search_offers_only_fs_search() {
    assert_eq!(pick("Finde meine PDFs im Ordner Dokumente"), vec!["fs_search"]);
    assert_eq!(pick("Wo liegt meine Rechnung?"), vec!["fs_search"]);
}

#[test]
fn smalltalk_offers_nothing() {
    assert!(pick("Hallo JARVIS, wie geht es dir?").is_empty());
}

#[test]
fn compound_requests_still_get_the_tools_they_need() {
    let p = pick("Verschiebe alle PDFs aus Downloads in den Ordner Schule");
    assert!(p.contains(&"fs_move") && p.contains(&"fs_search"), "{p:?}");
    let p = pick("Lösche die Datei alt.txt");
    assert!(p.contains(&"fs_trash") && p.contains(&"fs_search"), "{p:?}");
    let p = pick("Lies die Datei notizen.txt");
    assert_eq!(p, vec!["fs_read", "fs_search"]);
    let p = pick("Benenne rechnung.pdf in rechnung_2026.pdf um");
    assert!(p.contains(&"fs_rename"), "{p:?}");
    let p = pick("Erstelle eine Notiz einkauf.txt mit Milch und Brot");
    assert!(p.contains(&"fs_create"), "{p:?}");
    let p = pick("Fasse meine wichtigen Mails zusammen");
    assert!(p.contains(&"email_digest") && p.contains(&"email_search"), "{p:?}");
}

#[test]
fn write_tools_never_come_in_through_descriptions_or_file_names() {
    let mem = Memory::in_memory().unwrap();
    let all = all_tools(&mem);
    for q in [
        "Finde meine PDFs im Ordner Dokumente",
        "Lies die Datei notizen.txt",
        "Kopiere zeugnis.pdf auf den Schreibtisch",
        "Zeig mir den Ordner Schule",
        "Was steht in meinen Mails?",
    ] {
        let sel = select_tools(&all, q, &mem, 5);
        let mutating: Vec<&str> = sel
            .tools
            .iter()
            .filter(|t| t.spec.access != Access::Read && !q.to_lowercase().contains("kopier"))
            .map(|t| t.spec.name)
            .collect();
        assert!(mutating.is_empty(), "{q}: {mutating:?}");
        assert!(!sel.tools.iter().any(|t| t.spec.name == "fs_trash" || t.spec.name == "fs_create" && !q.contains("Erstelle")), "{q}");
    }
}

#[test]
fn memory_remember_only_on_explicit_request() {
    assert!(!pick("Meine Lieblingsfarbe ist blau").contains(&"memory_remember"));
    assert!(pick("Merk dir: meine Lieblingsfarbe ist blau").contains(&"memory_remember"));
}
