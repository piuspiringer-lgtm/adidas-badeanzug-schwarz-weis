//! Tool-Auswahl: Statt alle Tools in jeden Prompt zu packen, wählt JARVIS
//! pro Anfrage wenige passende aus. Das spart Tokens, verbessert die
//! Trefferquote kleiner Modelle und begrenzt, was eine Prompt-Injection
//! überhaupt auslösen könnte: **Nur ausgewählte Tools sind ausführbar.**

use jarvis_memory::{Memory, ToolStat};
use jarvis_permissions::Access;
use jarvis_runtime::tool::ToolInfo;
use std::collections::{BTreeSet, HashMap};

const STOP: &[&str] = &[
    "der", "die", "das", "und", "oder", "ein", "eine", "einen", "einem", "einer", "ist", "sind", "in", "im", "zu", "mit",
    "von", "auf", "für", "an", "am", "als", "auch", "es", "ich", "du", "mir", "mich", "mein", "meine", "meinen", "meinem",
    "bitte", "kannst", "kann", "alle", "nach", "aus", "den", "dem", "des", "wie", "was", "wo", "bei", "jarvis", "mal",
    "hey", "hallo", "the", "and", "please", "dann", "noch", "da", "dort", "hier", "jetzt", "heute",
];

/// Stichwort-Stämme je Tool (deutsch/englisch). Präfix-Vergleich, damit
/// "verschiebe", "verschieben" und "verschoben" … passen.
fn keywords(tool: &str) -> &'static [&'static str] {
    match tool {
        "fs_search" => &["such", "find", "datei", "dokument", "pdf", "liegt", "wo", "file"],
        // "Ordner" allein ist meist nur die Ortsangabe ("im Ordner X") – kein Auflisten.
        "fs_list" => &["zeig", "list", "auflist", "verzeichnis", "inhalt", "enthält", "folder"],
        "fs_read" => &["lies", "lese", "inhalt", "steht", "read"],
        // Nur Handlungsverben: Dateinamen wie "notizen.txt" dürfen kein Schreib-Tool auslösen.
        "fs_create" => &["erstell", "schreib$", "schreibe", "anleg", "create", "write"],
        "fs_rename" => &["umbenenn", "benenn", "nenn", "rename", "name"],
        "fs_move" => &["verschieb", "verschob", "beweg", "ablege", "move", "räum", "sortier"],
        "fs_copy" => &["kopier", "duplizier", "sicherungskopie", "backup", "copy"],
        "fs_trash" => &["lösch", "losch", "papierkorb", "entfern", "wegwerf", "trash", "delete"],
        "fs_open" => &["öffn", "offn", "open"],
        "fs_reveal" => &["finder", "reveal"],
        "web_search" => &["internet", "web", "online", "google", "such", "aktuell", "news", "nachricht"],
        "web_research" => &["recherch", "vergleich", "quell", "informier", "herausfind", "research"],
        "web_fetch" => &["url", "http", "webseite", "seite", "artikel", "link"],
        "web_links" => &["links", "verlink", "unterseit"],
        "email_search" | "email_read" | "email_recent" => &["mail", "e-mail", "email", "postfach", "posteingang", "absender", "inbox"],
        "email_digest" => &["mail", "e-mail", "email", "postfach", "posteingang", "zusammenfass", "überblick"],
        "teams_chats" | "teams_messages" | "teams_search" => &["teams", "chat", "kanal"],
        "webuntis_timetable" | "webuntis_holidays" | "webuntis_search" => {
            &["stundenplan", "unterricht", "stunde", "schule", "fach", "lehrer", "entfall", "vertretung", "ferien", "untis", "morgen"]
        }
        "memory_remember" => &["merk", "remember", "vergiss", "speicher"],
        "memory_recall" => &["erinner", "weißt", "gemerkt", "recall", "kennst"],
        _ => &[],
    }
}

/// Lesende Helfer, die ein Tool braucht, um sein Ziel erst zu finden
/// (z. B. Datei suchen, bevor sie verschoben wird). Suchtools brauchen keine.
fn helpers(tool: &str) -> &'static [&'static str] {
    match tool {
        "fs_read" | "fs_open" | "fs_reveal" | "fs_create" => &["fs_search"],
        "fs_rename" | "fs_move" | "fs_copy" | "fs_trash" => &["fs_search", "fs_list"],
        "email_read" | "email_digest" => &["email_search"],
        "teams_messages" => &["teams_chats"],
        "web_links" => &["web_fetch"],
        _ => &[],
    }
}

/// Mindestpunktzahl: ein echter Stichwort-Treffer (2,0). Reine Beschreibungs-
/// Treffer (je 0,5) reichen nur bei mehreren Wörtern.
const MIN_SCORE: f64 = 2.0;
/// Tools deutlich unter dem besten Treffer werden nicht angeboten.
const RELATIVE_CUTOFF: f64 = 0.5;

pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '-')
        .map(|w| w.trim_matches('-').to_lowercase())
        .filter(|w| w.chars().count() >= 2 && !STOP.contains(&w.as_str()))
        .collect()
}

/// Wortstamm-Vergleich in **eine** Richtung: das Wort muss mit dem Stamm
/// beginnen ("verschiebe" → "verschieb", "pdfs" → "pdf"). Früher galt auch
/// die Gegenrichtung, wodurch z. B. "finde" fälschlich "finder" traf.
fn stem_match(token: &str, stem: &str) -> bool {
    // "wort$" = nur genau dieses Wort (z. B. "schreib$" ≠ "schreibtisch").
    if let Some(exact) = stem.strip_suffix('$') {
        return token == exact;
    }
    if stem.chars().count() < 3 {
        return token == stem;
    }
    token.starts_with(stem)
}

/// Stabiler Schlüssel für "Absicht" einer Anfrage (für Tool-Statistik und Workflows).
pub fn intent_key(request: &str) -> String {
    let mut t: Vec<String> = tokens(request).into_iter().collect::<BTreeSet<_>>().into_iter().collect();
    t.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    t.truncate(3);
    t.sort();
    if t.is_empty() {
        "allgemein".into()
    } else {
        t.join(" ")
    }
}

/// Ob der Benutzer ausdrücklich darum bittet, sich etwas zu merken.
pub fn explicit_memory_request(request: &str) -> bool {
    let r = request.to_lowercase();
    ["merk dir", "merke dir", "merk mir", "speicher dir", "speichere dir", "vergiss nicht", "remember"].iter().any(|k| r.contains(k))
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub tools: Vec<ToolInfo>,
    pub scores: Vec<(String, f64)>,
}

pub fn select_tools(all: &[ToolInfo], request: &str, memory: &Memory, top_k: usize) -> Selection {
    let q = tokens(request);
    let intent = intent_key(request);
    let stats: HashMap<String, ToolStat> = memory.tool_stats(&intent).unwrap_or_default().into_iter().map(|s| (s.tool.clone(), s)).collect();
    let workflow_tools: BTreeSet<String> = memory
        .find_workflows(request, 2)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.score() >= 0.6)
        .flat_map(|w| w.steps.into_iter().map(|s| s.tool))
        .collect();
    let remember_ok = explicit_memory_request(request);

    let mut scored: Vec<(f64, &ToolInfo)> = all
        .iter()
        .filter(|t| t.spec.name != "memory_remember" || remember_ok)
        .filter_map(|t| {
            let name = t.spec.name;
            let kws = keywords(name);
            let desc = tokens(t.spec.description);
            let (mut kw, mut ds) = (0.0, 0.0);
            for tok in &q {
                if kws.iter().any(|k| stem_match(tok, k)) {
                    kw += 2.0;
                } else if desc.iter().any(|d| stem_match(tok, d)) {
                    ds += 0.5;
                }
            }
            // Verändernde Tools nur bei einem eigenen Stichwort-Treffer –
            // nie, weil ein Wort zufällig in ihrer Beschreibung vorkommt.
            if t.spec.access != Access::Read && kw == 0.0 {
                return None;
            }
            let mut s = kw + ds;
            if s > 0.0 {
                if let Some(st) = stats.get(name) {
                    s += (st.successes as f64 + 1.0) / (st.successes as f64 + st.failures as f64 + 2.0) - 0.5;
                }
                if workflow_tools.contains(name) {
                    s += 1.0;
                }
            }
            Some((s, t))
        })
        .filter(|(s, _)| *s >= MIN_SCORE)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.spec.name.cmp(b.1.spec.name)));
    let best = scored.first().map(|(s, _)| *s).unwrap_or(0.0);
    scored.retain(|(s, _)| *s >= best * RELATIVE_CUTOFF);
    scored.truncate(top_k);

    let mut names: Vec<&str> = scored.iter().map(|(_, t)| t.spec.name).collect();
    for (_, t) in &scored {
        for h in helpers(t.spec.name) {
            if !names.contains(h) && all.iter().any(|x| x.spec.name == *h) {
                names.push(h);
            }
        }
    }
    let tools: Vec<ToolInfo> = names.iter().filter_map(|n| all.iter().find(|t| t.spec.name == *n).cloned()).collect();
    Selection { scores: scored.iter().map(|(s, t)| (t.spec.name.to_string(), *s)).collect(), tools }
}
