use async_trait::async_trait;
use jarvis_integrations::fs::{self, FsSandbox, Opener};
use jarvis_permissions::*;
use jarvis_runtime::gateway::DenyAllConfirmer;
use jarvis_runtime::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct NullAudit;
impl AuditSink for NullAudit {
    fn record(&self, _: &AuditEvent) {}
}

struct Yes(Mutex<Vec<String>>);
#[async_trait]
impl Confirmer for Yes {
    async fn confirm(&self, _: &ToolSpec, _: &CallFacts, reason: &str) -> bool {
        self.0.lock().unwrap().push(reason.into());
        true
    }
}

#[derive(Default)]
struct RecordingOpener(Mutex<Vec<(PathBuf, bool)>>);
impl Opener for RecordingOpener {
    fn open(&self, p: &Path, reveal: bool) -> Result<(), String> {
        self.0.lock().unwrap().push((p.to_path_buf(), reveal));
        Ok(())
    }
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    protected: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("Dokumente");
    let outside = base.join("Privat");
    let protected = root.join("JARVIS-Daten");
    for d in [&root, &outside, &protected, &root.join("Schule")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let filler = "Mathe: Kapitel 3 lernen, Aufgaben zu Gleichungen rechnen. ".repeat(40);
    std::fs::write(root.join("Schule/notizen.txt"), format!("{filler}\n\nEnglisch: Vokabeln Unit 4.")).unwrap();
    std::fs::write(root.join("Schule/rechnung.pdf"), "x").unwrap();
    std::fs::write(outside.join("tagebuch.txt"), "geheim").unwrap();
    std::fs::write(protected.join("policy.db"), "x").unwrap();
    Env { _dir: dir, root, outside, protected }
}

fn gateway(e: &Env, confirmer: Arc<dyn Confirmer>, opener: Arc<RecordingOpener>) -> ToolGateway {
    let sb = Arc::new(FsSandbox::new(vec![e.root.clone()], vec![e.protected.clone()]));
    let mut reg = ToolRegistry::new();
    for t in fs::tools(sb, opener) {
        reg.register(t).unwrap();
    }
    ToolGateway::new(reg, PolicyEngine::new(vec![e.protected.clone()]), ServiceManager::new(), confirmer, Arc::new(NullAudit))
}

fn p(x: &Path) -> String {
    x.to_string_lossy().into_owned()
}

#[tokio::test]
async fn read_list_search_without_confirmation() {
    let e = env();
    let g = gateway(&e, Arc::new(DenyAllConfirmer), Arc::default());
    let out = g.invoke("fs_list", json!({"path": p(&e.root.join("Schule"))}), Origin::Agent).await.unwrap();
    assert!(out.text.contains("notizen.txt"));
    let out = g.invoke("fs_search", json!({"pattern": "*.PDF"}), Origin::Agent).await.unwrap();
    assert!(out.text.contains("rechnung.pdf"));
    let out = g.invoke("fs_search", json!({"pattern": "*", "contains": "vokabeln"}), Origin::Agent).await.unwrap();
    assert!(out.text.contains("notizen.txt") && out.text.starts_with("1 Treffer"), "{}", out.text);
    let out = g.invoke("fs_read", json!({"path": p(&e.root.join("Schule/notizen.txt")), "query": "Englisch Vokabeln"}), Origin::Agent).await.unwrap();
    assert!(out.text.contains("Unit 4"));
    assert!(!out.text.contains("Kapitel 3"), "nur relevanter Abschnitt: {}", out.text);
}

#[tokio::test]
async fn sandbox_escapes_are_blocked() {
    let e = env();
    let g = gateway(&e, Arc::new(DenyAllConfirmer), Arc::default());
    let escapes = [
        p(&e.outside.join("tagebuch.txt")),
        format!("{}/../Privat/tagebuch.txt", p(&e.root)),
        p(&e.protected.join("policy.db")),
        "/etc/passwd".into(),
    ];
    for path in &escapes {
        assert!(g.invoke("fs_read", json!({"path": path}), Origin::Agent).await.is_err(), "{path}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&e.outside, e.root.join("link")).unwrap();
        assert!(g.invoke("fs_read", json!({"path": p(&e.root.join("link/tagebuch.txt"))}), Origin::Agent).await.is_err());
    }
    assert!(g.invoke("fs_create", json!({"path": p(&e.protected.join("neu.txt")), "content": "x"}), Origin::User).await.is_err());
    assert!(g.invoke("fs_search", json!({"root": p(&e.outside), "pattern": "*"}), Origin::Agent).await.is_err());
    // Gesperrte Ordner tauchen auch in Suchergebnissen nicht auf.
    let out = g.invoke("fs_search", json!({"pattern": "policy.db"}), Origin::Agent).await.unwrap();
    assert!(out.text.starts_with("0 Treffer"), "{}", out.text);
}

#[tokio::test]
async fn create_overwrite_needs_confirmation() {
    let e = env();
    let f = e.root.join("neu.txt");
    let g = gateway(&e, Arc::new(DenyAllConfirmer), Arc::default());
    g.invoke("fs_create", json!({"path": p(&f), "content": "eins"}), Origin::Agent).await.unwrap();
    let r = g.invoke("fs_create", json!({"path": p(&f), "content": "zwei", "overwrite": true}), Origin::Agent).await;
    assert!(matches!(r, Err(GatewayError::NotConfirmed(_))));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "eins");

    let yes = Arc::new(Yes(Mutex::default()));
    let g = gateway(&e, yes.clone(), Arc::default());
    g.invoke("fs_create", json!({"path": p(&f), "content": "zwei", "overwrite": true}), Origin::Agent).await.unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "zwei");
    assert!(yes.0.lock().unwrap()[0].contains("ÜBERSCHREIBEN"));
}

#[tokio::test]
async fn trash_and_move_always_need_confirmation() {
    let e = env();
    let f = e.root.join("Schule/notizen.txt");
    let g = gateway(&e, Arc::new(DenyAllConfirmer), Arc::default());
    for (tool, args) in [
        ("fs_trash", json!({"path": p(&f)})),
        ("fs_move", json!({"path": p(&f), "destination": p(&e.root)})),
    ] {
        for origin in [Origin::Agent, Origin::User] {
            assert!(matches!(g.invoke(tool, args.clone(), origin).await, Err(GatewayError::NotConfirmed(_))), "{tool}");
        }
    }
    assert!(f.exists());
    assert!(g.invoke("fs_delete_permanent", json!({"path": p(&f)}), Origin::User).await.is_err());

    let g = gateway(&e, Arc::new(Yes(Mutex::default())), Arc::default());
    g.invoke("fs_move", json!({"path": p(&f), "destination": p(&e.root)}), Origin::Agent).await.unwrap();
    assert!(e.root.join("notizen.txt").exists() && !f.exists());
    // Wurzelordner selbst darf nicht entfernt werden.
    assert!(g.invoke("fs_trash", json!({"path": p(&e.root)}), Origin::User).await.is_err());
}

#[tokio::test]
async fn rename_copy_and_open() {
    let e = env();
    let opener = Arc::new(RecordingOpener::default());
    let g = gateway(&e, Arc::new(DenyAllConfirmer), opener.clone());
    let f = e.root.join("Schule/notizen.txt");
    g.invoke("fs_rename", json!({"path": p(&f), "new_name": "mathe.txt"}), Origin::Agent).await.unwrap();
    let f = e.root.join("Schule/mathe.txt");
    assert!(f.exists());
    assert!(g.invoke("fs_rename", json!({"path": p(&f), "new_name": "../../x.txt"}), Origin::Agent).await.is_err());
    g.invoke("fs_copy", json!({"path": p(&f), "destination": p(&e.root)}), Origin::Agent).await.unwrap();
    assert!(e.root.join("mathe.txt").exists());

    g.invoke("fs_reveal", json!({"path": p(&f)}), Origin::Agent).await.unwrap();
    assert!(opener.0.lock().unwrap()[0].1, "reveal");
    // Öffnen braucht immer eine Bestätigung …
    assert!(matches!(g.invoke("fs_open", json!({"path": p(&f)}), Origin::Agent).await, Err(GatewayError::NotConfirmed(_))));
    // … und Skripte werden nie geöffnet.
    std::fs::write(e.root.join("x.command"), "rm -rf ~").unwrap();
    let g = gateway(&e, Arc::new(Yes(Mutex::default())), opener.clone());
    assert!(g.invoke("fs_open", json!({"path": p(&e.root.join("x.command"))}), Origin::User).await.is_err());
    g.invoke("fs_open", json!({"path": p(&f)}), Origin::User).await.unwrap();
    assert_eq!(opener.0.lock().unwrap().len(), 2);
}
