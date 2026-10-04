//! Dateisystem-/Finder-Tools. Alle Pfade werden in einer Sandbox aufgelöst:
//! nur freigegebene Wurzelordner, keine geschützten Pfade, keine
//! Symlink-/`..`-Ausbrüche. Endgültiges Löschen gibt es nicht – nur Papierkorb.

use async_trait::async_trait;
use jarvis_context::{rank_chunks, truncate_middle, chunk_text};
use jarvis_permissions::{Access, CallFacts, Capability, Confirmation, Integration, RiskLevel, ToolSpec};
use jarvis_runtime::{Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FsSandbox {
    roots: Vec<PathBuf>,
    protected: Vec<PathBuf>,
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

impl FsSandbox {
    /// `roots`: freigegebene Ordner. `protected`: zusätzlich gesperrte Pfade
    /// (z. B. JARVIS-Datenordner mit Policy, Memory und Audit-Log).
    pub fn new(roots: Vec<PathBuf>, protected: Vec<PathBuf>) -> Self {
        let canon = |v: Vec<PathBuf>| v.into_iter().filter_map(|p| p.canonicalize().ok().or(Some(p))).collect();
        let mut protected: Vec<PathBuf> = canon(protected);
        if let Some(h) = dirs::home_dir() {
            for p in [".ssh", ".gnupg", ".aws", ".config/gcloud", "Library/Keychains", "Library/Application Support/JARVIS"] {
                protected.push(h.join(p));
            }
        }
        Self { roots: canon(roots), protected }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn protected(&self) -> &[PathBuf] {
        &self.protected
    }

    /// Löst einen Pfad sicher auf. Für noch nicht existierende Ziele wird der
    /// Elternordner aufgelöst.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, ToolError> {
        if raw.trim().is_empty() || raw.contains('\0') {
            return Err(ToolError::InvalidArgs("leerer Pfad".into()));
        }
        let p = expand_home(raw.trim());
        let p = if p.is_relative() {
            self.roots.first().ok_or_else(|| ToolError::Forbidden("keine Ordner freigegeben".into()))?.join(p)
        } else {
            p
        };
        let resolved = if p.exists() {
            p.canonicalize().map_err(|e| ToolError::Failed(e.to_string()))?
        } else {
            let name = p.file_name().ok_or_else(|| ToolError::InvalidArgs("ungültiger Pfad".into()))?;
            if p.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(ToolError::Forbidden("'..' ist nicht erlaubt".into()));
            }
            let parent = p.parent().ok_or_else(|| ToolError::InvalidArgs("kein Elternordner".into()))?;
            parent
                .canonicalize()
                .map_err(|_| ToolError::NotFound(format!("Ordner {} existiert nicht", parent.display())))?
                .join(name)
        };
        if !self.roots.iter().any(|r| resolved.starts_with(r)) {
            return Err(ToolError::Forbidden(format!("{} liegt außerhalb der freigegebenen Ordner", resolved.display())));
        }
        if self.protected.iter().any(|p| resolved.starts_with(p)) {
            return Err(ToolError::Forbidden(format!("{} ist geschützt", resolved.display())));
        }
        Ok(resolved)
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| ToolError::InvalidArgs(format!("'{key}' fehlt")))
}

fn arg_usize(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key).and_then(Value::as_u64).map(|v| v as usize).unwrap_or(default).min(max)
}

fn modified_secs(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// Öffnet Dateien bzw. zeigt sie im Finder (austauschbar für Tests).
pub trait Opener: Send + Sync {
    fn open(&self, path: &Path, reveal: bool) -> Result<(), String>;
}

pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&self, path: &Path, reveal: bool) -> Result<(), String> {
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            if reveal {
                c.arg("-R");
            }
            c
        } else {
            std::process::Command::new("xdg-open")
        };
        let target = if reveal && !cfg!(target_os = "macos") { path.parent().unwrap_or(path) } else { path };
        cmd.arg(target).status().map_err(|e| e.to_string()).and_then(|s| if s.success() { Ok(()) } else { Err(format!("Exit {s}")) })
    }
}

macro_rules! fs_tool {
    ($ty:ident, $name:literal, $desc:literal, $access:expr, $risk:expr, $confirm:expr, [$($cap:expr),*]) => {
        pub struct $ty {
            pub sandbox: Arc<FsSandbox>,
        }
        impl $ty {
            pub const SPEC: ToolSpec = ToolSpec {
                name: $name,
                description: $desc,
                integration: Integration::Filesystem,
                access: $access,
                risk: $risk,
                confirmation: $confirm,
                capabilities: &[$($cap),*],
                services: &[],
            };
        }
    };
}

fs_tool!(FsList, "fs_list", "Listet den Inhalt eines Ordners.", Access::Read, RiskLevel::Low, Confirmation::Never, [Capability::FsRead]);
fs_tool!(FsSearch, "fs_search", "Sucht Dateien per Namensmuster (Glob) und optional nach Textinhalt.", Access::Read, RiskLevel::Low, Confirmation::Never, [Capability::FsRead]);
fs_tool!(FsRead, "fs_read", "Liest eine Textdatei; mit 'query' nur die relevanten Abschnitte.", Access::Read, RiskLevel::Low, Confirmation::Never, [Capability::FsRead]);
fs_tool!(FsCreate, "fs_create", "Erstellt eine neue Textdatei (Überschreiben nur mit Bestätigung).", Access::Write, RiskLevel::Medium, Confirmation::WhenRisky, [Capability::FsWrite]);
fs_tool!(FsRename, "fs_rename", "Benennt eine Datei oder einen Ordner um.", Access::Write, RiskLevel::Medium, Confirmation::WhenRisky, [Capability::FsWrite]);
fs_tool!(FsMove, "fs_move", "Verschiebt eine Datei oder einen Ordner.", Access::Write, RiskLevel::Medium, Confirmation::Always, [Capability::FsWrite]);
fs_tool!(FsCopy, "fs_copy", "Kopiert eine Datei.", Access::Write, RiskLevel::Low, Confirmation::WhenRisky, [Capability::FsRead, Capability::FsWrite]);
fs_tool!(FsTrash, "fs_trash", "Legt eine Datei oder einen Ordner in den Papierkorb.", Access::Destructive, RiskLevel::High, Confirmation::Always, [Capability::FsTrash]);

pub struct FsOpen {
    pub sandbox: Arc<FsSandbox>,
    pub opener: Arc<dyn Opener>,
    pub reveal: bool,
}

impl FsOpen {
    pub const SPEC_OPEN: ToolSpec = ToolSpec {
        name: "fs_open",
        description: "Öffnet eine Datei mit der Standard-App (keine Programme/Skripte).",
        integration: Integration::Filesystem,
        access: Access::Write,
        risk: RiskLevel::Medium,
        confirmation: Confirmation::Always,
        capabilities: &[Capability::OpenWithSystem],
        services: &[],
    };
    pub const SPEC_REVEAL: ToolSpec = ToolSpec {
        name: "fs_reveal",
        description: "Zeigt eine Datei im Finder an.",
        integration: Integration::Filesystem,
        access: Access::Write,
        risk: RiskLevel::Low,
        confirmation: Confirmation::WhenRisky,
        capabilities: &[Capability::OpenWithSystem],
        services: &[],
    };
}

fn is_executable_like(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ["app", "command", "sh", "zsh", "bash", "tool", "pkg", "dmg", "scpt", "applescript", "workflow", "terminal", "jar", "py", "rb", "pl", "exe", "bat"]
        .contains(&ext.as_str())
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(p) {
            return m.is_file() && m.permissions().mode() & 0o111 != 0;
        }
    }
    false
}

fn path_facts(paths: Vec<PathBuf>, overwrites: bool, preview: String) -> CallFacts {
    CallFacts { paths, overwrites, preview }
}

#[async_trait]
impl Tool for FsList {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer"}},"required":["path"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        Ok(path_facts(vec![self.sandbox.resolve(arg_str(a, "path")?)?], false, String::new()))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let dir = self.sandbox.resolve(arg_str(&a, "path")?)?;
        let limit = arg_usize(&a, "limit", 100, 500);
        let mut entries = vec![];
        let rd = std::fs::read_dir(&dir).map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut total = 0;
        for e in rd.flatten() {
            total += 1;
            if entries.len() >= limit {
                continue;
            }
            let m = e.metadata().ok();
            entries.push(json!({
                "name": e.file_name().to_string_lossy(),
                "dir": m.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                "size": m.as_ref().map(|m| m.len()).unwrap_or(0),
                "modified": m.as_ref().map(modified_secs).unwrap_or(0),
            }));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let text = format!("{} ({} Einträge, {} gezeigt)\n{}", dir.display(), total, entries.len(), serde_json::to_string(&entries).unwrap());
        Ok(ToolOutput::with_data(text, json!({"path": dir, "entries": entries, "total": total})))
    }
}

#[async_trait]
impl Tool for FsSearch {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "root":{"type":"string","description":"Startordner (Standard: alle freigegebenen)"},
            "pattern":{"type":"string","description":"Glob, z. B. *.pdf oder *rechnung*"},
            "contains":{"type":"string","description":"optionaler Text im Inhalt"},
            "limit":{"type":"integer"}},"required":["pattern"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let roots = match a.get("root").and_then(Value::as_str) {
            Some(r) => vec![self.sandbox.resolve(r)?],
            None => self.sandbox.roots().to_vec(),
        };
        Ok(path_facts(roots, false, String::new()))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let roots = self.facts(&a)?.paths;
        let pat = arg_str(&a, "pattern")?;
        let glob = globset::GlobBuilder::new(pat)
            .case_insensitive(true)
            .literal_separator(false)
            .build()
            .map_err(|e| ToolError::InvalidArgs(e.to_string()))?
            .compile_matcher();
        let contains = a.get("contains").and_then(Value::as_str).map(str::to_lowercase);
        let limit = arg_usize(&a, "limit", 50, 300);
        let sandbox = self.sandbox.clone();
        let hits = tokio::task::spawn_blocking(move || {
            let mut hits = vec![];
            let mut scanned = 0usize;
            'outer: for root in roots {
                for e in walkdir::WalkDir::new(&root).max_depth(12).follow_links(false).into_iter().filter_entry(|e| {
                    let n = e.file_name().to_string_lossy();
                    !(n.starts_with('.') && e.depth() > 0) && n != "node_modules" && n != "target"
                        && !sandbox.protected().iter().any(|p| e.path().starts_with(p))
                }) {
                    let Ok(e) = e else { continue };
                    scanned += 1;
                    if scanned > 200_000 {
                        break 'outer;
                    }
                    if !e.file_type().is_file() || !glob.is_match(e.file_name()) {
                        continue;
                    }
                    if let Some(c) = &contains {
                        let ok = e.metadata().map(|m| m.len() <= 1024 * 1024).unwrap_or(false)
                            && std::fs::read_to_string(e.path()).map(|t| t.to_lowercase().contains(c)).unwrap_or(false);
                        if !ok {
                            continue;
                        }
                    }
                    hits.push(json!({"path": e.path(), "size": e.metadata().map(|m| m.len()).unwrap_or(0)}));
                    if hits.len() >= limit {
                        break 'outer;
                    }
                }
            }
            hits
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::with_data(format!("{} Treffer\n{}", hits.len(), serde_json::to_string(&hits).unwrap()), json!(hits)))
    }
}

#[async_trait]
impl Tool for FsRead {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"},"max_tokens":{"type":"integer"}},"required":["path"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        Ok(path_facts(vec![self.sandbox.resolve(arg_str(a, "path")?)?], false, String::new()))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let p = self.sandbox.resolve(arg_str(&a, "path")?)?;
        let meta = std::fs::metadata(&p).map_err(|_| ToolError::NotFound(p.display().to_string()))?;
        if !meta.is_file() {
            return Err(ToolError::InvalidArgs("kein reguläre Datei".into()));
        }
        if meta.len() > MAX_READ_BYTES {
            return Err(ToolError::Failed("Datei zu groß (> 5 MB)".into()));
        }
        let bytes = std::fs::read(&p).map_err(|e| ToolError::Failed(e.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| ToolError::Failed("keine Textdatei (PDF/Office folgen später)".into()))?;
        let max = arg_usize(&a, "max_tokens", 1500, 6000);
        let out = match a.get("query").and_then(Value::as_str) {
            Some(q) => {
                let chunks: Vec<(String, String)> = chunk_text(&text, 250).into_iter().enumerate().map(|(i, c)| (format!("#{i}"), c)).collect();
                let picked = rank_chunks(q, &chunks, max);
                if picked.is_empty() {
                    truncate_middle(&text, max)
                } else {
                    picked.iter().map(|s| format!("[{}] {}", s.source, s.text)).collect::<Vec<_>>().join("\n")
                }
            }
            None => truncate_middle(&text, max),
        };
        Ok(ToolOutput::with_data(out, json!({"path": p, "bytes": meta.len()})))
    }
}

#[async_trait]
impl Tool for FsCreate {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"overwrite":{"type":"boolean"}},"required":["path","content"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let p = self.sandbox.resolve(arg_str(a, "path")?)?;
        let exists = p.exists();
        let preview = if exists { format!("Datei {} ÜBERSCHREIBEN", p.display()) } else { format!("Datei {} erstellen", p.display()) };
        Ok(path_facts(vec![p], exists, preview))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let p = self.sandbox.resolve(arg_str(&a, "path")?)?;
        let content = arg_str(&a, "content")?;
        let overwrite = a.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
        if p.exists() && !overwrite {
            return Err(ToolError::Failed(format!("{} existiert bereits (overwrite=true nötig)", p.display())));
        }
        if p.is_dir() {
            return Err(ToolError::InvalidArgs("Ziel ist ein Ordner".into()));
        }
        std::fs::write(&p, content).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::with_data(format!("Erstellt: {}", p.display()), json!({"path": p})))
    }
}

fn target_in_same_dir(src: &Path, new_name: &str) -> Result<PathBuf, ToolError> {
    if new_name.is_empty() || new_name.contains('/') || new_name.contains('\\') || new_name == "." || new_name == ".." {
        return Err(ToolError::InvalidArgs("ungültiger neuer Name".into()));
    }
    Ok(src.parent().ok_or_else(|| ToolError::InvalidArgs("kein Elternordner".into()))?.join(new_name))
}

#[async_trait]
impl Tool for FsRename {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"new_name":{"type":"string"}},"required":["path","new_name"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let src = self.sandbox.resolve(arg_str(a, "path")?)?;
        let dst = self.sandbox.resolve(target_in_same_dir(&src, arg_str(a, "new_name")?)?.to_str().unwrap_or(""))?;
        let ow = dst.exists();
        Ok(path_facts(vec![src.clone(), dst.clone()], ow, format!("{} → {}", src.display(), dst.display())))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let f = self.facts(&a)?;
        if f.overwrites {
            return Err(ToolError::Failed("Ziel existiert bereits".into()));
        }
        std::fs::rename(&f.paths[0], &f.paths[1]).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::with_data(format!("Umbenannt: {}", f.preview), json!({"from": f.paths[0], "to": f.paths[1]})))
    }
}

fn dest_path(sandbox: &FsSandbox, src: &Path, dest: &str) -> Result<PathBuf, ToolError> {
    let d = sandbox.resolve(dest)?;
    let d = if d.is_dir() { d.join(src.file_name().ok_or_else(|| ToolError::InvalidArgs("Quelle ohne Namen".into()))?) } else { d };
    sandbox.resolve(d.to_str().unwrap_or(""))
}

#[async_trait]
impl Tool for FsMove {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"destination":{"type":"string"}},"required":["path","destination"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let src = self.sandbox.resolve(arg_str(a, "path")?)?;
        let dst = dest_path(&self.sandbox, &src, arg_str(a, "destination")?)?;
        let ow = dst.exists();
        Ok(path_facts(vec![src.clone(), dst.clone()], ow, format!("Verschieben {} → {}", src.display(), dst.display())))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let f = self.facts(&a)?;
        if f.overwrites {
            return Err(ToolError::Failed("Ziel existiert bereits".into()));
        }
        if f.paths[1].starts_with(&f.paths[0]) {
            return Err(ToolError::InvalidArgs("Ordner kann nicht in sich selbst verschoben werden".into()));
        }
        std::fs::rename(&f.paths[0], &f.paths[1]).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::with_data(f.preview.clone(), json!({"from": f.paths[0], "to": f.paths[1]})))
    }
}

#[async_trait]
impl Tool for FsCopy {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"destination":{"type":"string"},"overwrite":{"type":"boolean"}},"required":["path","destination"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let src = self.sandbox.resolve(arg_str(a, "path")?)?;
        let dst = dest_path(&self.sandbox, &src, arg_str(a, "destination")?)?;
        let ow = dst.exists();
        Ok(path_facts(vec![src.clone(), dst.clone()], ow, format!("Kopieren {} → {}{}", src.display(), dst.display(), if ow { " (ÜBERSCHREIBT)" } else { "" })))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let f = self.facts(&a)?;
        if f.overwrites && !a.get("overwrite").and_then(Value::as_bool).unwrap_or(false) {
            return Err(ToolError::Failed("Ziel existiert (overwrite=true nötig)".into()));
        }
        if !f.paths[0].is_file() {
            return Err(ToolError::InvalidArgs("nur Dateien können kopiert werden".into()));
        }
        std::fs::copy(&f.paths[0], &f.paths[1]).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::with_data(f.preview.clone(), json!({"from": f.paths[0], "to": f.paths[1]})))
    }
}

#[async_trait]
impl Tool for FsTrash {
    fn spec(&self) -> &ToolSpec {
        &Self::SPEC
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let p = self.sandbox.resolve(arg_str(a, "path")?)?;
        if self.sandbox.roots().contains(&p) {
            return Err(ToolError::Forbidden("freigegebene Wurzelordner können nicht entfernt werden".into()));
        }
        Ok(path_facts(vec![p.clone()], true, format!("{} in den Papierkorb legen", p.display())))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let f = self.facts(&a)?;
        trash::delete(&f.paths[0]).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(ToolOutput::with_data(format!("Im Papierkorb: {}", f.paths[0].display()), json!({"path": f.paths[0]})))
    }
}

#[async_trait]
impl Tool for FsOpen {
    fn spec(&self) -> &ToolSpec {
        if self.reveal {
            &Self::SPEC_REVEAL
        } else {
            &Self::SPEC_OPEN
        }
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        let p = self.sandbox.resolve(arg_str(a, "path")?)?;
        if !p.exists() {
            return Err(ToolError::NotFound(p.display().to_string()));
        }
        if !self.reveal && is_executable_like(&p) {
            return Err(ToolError::Forbidden("Programme und Skripte werden nicht geöffnet".into()));
        }
        let verb = if self.reveal { "Im Finder zeigen" } else { "Öffnen" };
        Ok(path_facts(vec![p.clone()], false, format!("{verb}: {}", p.display())))
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let f = self.facts(&a)?;
        self.opener.open(&f.paths[0], self.reveal).map_err(ToolError::Failed)?;
        Ok(ToolOutput::text(f.preview))
    }
}

/// Alle Dateisystem-Tools.
pub fn tools(sandbox: Arc<FsSandbox>, opener: Arc<dyn Opener>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FsList { sandbox: sandbox.clone() }),
        Arc::new(FsSearch { sandbox: sandbox.clone() }),
        Arc::new(FsRead { sandbox: sandbox.clone() }),
        Arc::new(FsCreate { sandbox: sandbox.clone() }),
        Arc::new(FsRename { sandbox: sandbox.clone() }),
        Arc::new(FsMove { sandbox: sandbox.clone() }),
        Arc::new(FsCopy { sandbox: sandbox.clone() }),
        Arc::new(FsTrash { sandbox: sandbox.clone() }),
        Arc::new(FsOpen { sandbox: sandbox.clone(), opener: opener.clone(), reveal: false }),
        Arc::new(FsOpen { sandbox, opener, reveal: true }),
    ]
}
