# TOOLS.md – Integrationen, Berechtigungen, Kosten

Stand: Infrastruktur-Phase. Alle JARVIS-Tools laufen **lokal in der App** und
ausschließlich über den zentralen Tool-Gateway (Policy → Bestätigung → Audit).

## 1. Externe MCP-Server / Plugins / Connectors

| Komponente | Status | Begründung |
|---|---|---|
| GitHub (Claude Code) | aktiv | nur für die Entwicklung (Branches/PRs), nicht Teil von JARVIS |
| Microsoft-365-Connector (claude.ai) | **nicht installiert** | würde Claude in der Cloud Zugriff auf Mail/Teams geben – widerspricht „lokal-first“; JARVIS nutzt eigene Read-only-Adapter |
| Parallel Search / Exa (claude.ai) | **nicht installiert** | Websuche für Claude, nicht für JARVIS; JARVIS hat eigene Suche |
| Fremde Mail-/Teams-/Office-MCP-Server | **bewusst nicht** | bringen oft Sende-/Lösch-Tools mit |
| claude-security (Anthropic-Plugin) | optional, nicht installiert | könnte später für Security-Reviews helfen; braucht keine JARVIS-Berechtigungen |

Ergebnis: **Es wurde nichts Externes installiert.** Für JARVIS ist kein
fremder MCP-Server nötig. Eine MCP-Brücke für spätere, selbst gewählte Server
ist in der Architektur vorgesehen (Phase 7), läuft dann aber ebenfalls durch
den Gateway.

## 2. JARVIS-Komponenten

| Komponente | Zweck | Lokal/Cloud | Berechtigungen | Kosten | Ressourcen | Getestet |
|---|---|---|---|---|---|---|
| **Ollama** + qwen3:8b / qwen3:4b | lokales LLM + Fallback | lokal (127.0.0.1) | keine Netzfreigabe nach außen | kostenlos | ~5,2 GB RAM aktiv, 0 im Leerlauf; ~8 GB SSD | Mock ✅ · live auf Mac: `jarvis doctor` |
| nomic-embed-text | Embeddings (Memory) | lokal | – | kostenlos | ~0,3 GB | Phase 3 |
| **Filesystem-Tools** | suchen, lesen, erstellen, umbenennen, verschieben, kopieren, Papierkorb, öffnen, im Finder zeigen | lokal | nur freigegebene Ordner; macOS fragt beim ersten Zugriff | kostenlos | gering | ✅ 5 Integrationstests |
| **Web-Recherche** | Suche, Seiten lesen, Links folgen, Mehrquellen-Recherche, Cache | Internet (nur GET) | ausgehendes HTTPS; keine lokalen/privaten Ziele | kostenlos (DuckDuckGo-HTML); Brave optional mit Key | gering | ✅ 4 Tests gegen Mock-Server |
| **SQLite-Memory** | Präferenzen, Fakten, Workflows, Skills, Tool-Erfahrungen, Fehler/Lösungen, Research-Cache, Audit | lokal (`~/Library/Application Support/JARVIS/brain.db`) | für Agent-Tools gesperrt | kostenlos | < 50 MB | ✅ 12 Tests |
| **E-Mail (Outlook/M365)** | suchen, lesen, zusammenfassen | Microsoft Graph | **nur `Mail.Read`** (+ `User.Read`, `offline_access`) | kostenlos | gering | ✅ Mock; live nach Anmeldung |
| **E-Mail (Gmail)** | suchen, lesen, zusammenfassen | Gmail-API | **nur `gmail.readonly`** | kostenlos | gering | ✅ Code; Anmeldung folgt mit UI |
| **Microsoft Teams** | Chats lesen, durchsuchen | Microsoft Graph | **nur `Chat.Read`** | kostenlos | gering | ✅ Mock; live nach Anmeldung |
| **WebUntis** | Stundenplan, Entfall, Ferien, Suche | Schul-Server (JSON-RPC) | Schul-Login; nur Lese-Methoden | kostenlos | gering | ✅ Mock; live nach Konfiguration |
| **whisper.cpp** | Spracherkennung (Deutsch) | lokal, Metal | Mikrofon (nur Push-to-Talk) | kostenlos | ~1 GB RAM nur während Transkription | ✅ Fake-Binary; live: `jarvis transcribe` |
| **macOS TTS (`say`)** | Sprachausgabe | lokal | – | kostenlos | ~0 | live: `jarvis say` |
| Piper (optional) | alternative Stimme | lokal | – | kostenlos | ~80 MB | optional |

## 3. Alle Tools mit Berechtigungen

Ausgabe von `jarvis tools` (Spalten aus der jeweiligen `ToolSpec`):

| Tool | Integration | Zugriff | Risiko | Bestätigung | Fähigkeiten |
|---|---|---|---|---|---|
| fs_list | Filesystem | Read | Low | nie | FsRead |
| fs_search | Filesystem | Read | Low | nie | FsRead |
| fs_read | Filesystem | Read | Low | nie | FsRead |
| fs_create | Filesystem | Write | Medium | beim Überschreiben | FsWrite |
| fs_rename | Filesystem | Write | Medium | wenn Ziel existiert (wird abgelehnt) | FsWrite |
| fs_copy | Filesystem | Write | Low | beim Überschreiben | FsRead, FsWrite |
| fs_move | Filesystem | Write | Medium | **immer** | FsWrite |
| fs_trash | Filesystem | Destructive | High | **immer** | FsTrash |
| fs_open | Filesystem | Write | Medium | **immer** (keine Programme/Skripte) | OpenWithSystem |
| fs_reveal | Filesystem | Write | Low | nie | OpenWithSystem |
| web_search | Web | Write¹ | Low | nie | NetworkGet, MemoryRead, MemoryWrite |
| web_fetch | Web | Write¹ | Low | nie | NetworkGet, MemoryRead, MemoryWrite |
| web_research | Web | Write¹ | Low | nie | NetworkGet, MemoryRead, MemoryWrite |
| web_links | Web | Write¹ | Low | nie | NetworkGet, MemoryRead, MemoryWrite |
| email_search / email_read / email_recent / email_digest | Email | **Read** | Low | nie | MailRead, NetworkGet |
| teams_chats / teams_messages / teams_search | Teams | **Read** | Low | nie | TeamsRead, NetworkGet |
| webuntis_timetable / webuntis_holidays / webuntis_search | WebUntis | **Read** | Low | nie | WebUntisRead, NetworkGet |

¹ „Write“ nur, weil Ergebnisse in den lokalen Research-Cache geschrieben werden.
Ehrlich deklariert statt versteckt.

Ein endgültiges Löschen gibt es nicht. Ebenso wenig Shell-Befehle oder
Mikrofon-Steuerung durch das Modell.

## 4. Token-effiziente Recherche (`web_research`)

1. **Wissen wiederverwenden:** FTS5-Suche in bereits recherchierten Abschnitten (7 Tage). Gibt es genug Treffer aus ≥ 2 Quellen, wird kein Netz genutzt.
2. **Suchergebnisse statt Seiten:** Titel und Snippet (Cache 6 h, normalisierte Anfrage).
3. **Nur bei Bedarf laden:** höchstens N (Standard 3) Seiten, die per BM25 am relevantesten sind; Cache 24 h.
4. **Vorverarbeitung:** Navigation, Footer, Cookie-Banner und Skripte werden entfernt. Der Haupttext wird in Abschnitte von ~220 Tokens geteilt.
5. **Duplikate vermeiden:** inhaltsgleiche Abschnitte werden nicht gespeichert (Hash), nahezu gleiche nicht doppelt ans Modell gegeben (Jaccard ≥ 0,8).
6. **Budget:** Nur die besten Abschnitte bis zum Token-Budget (Standard 1500) gehen ans Modell, mit Quellennummern. Snippets bereits geladener Seiten entfallen.

## 5. Einrichtung je Integration

```bash
bash scripts/setup-mac.sh              # Ollama, Modelle, whisper.cpp, CLI (fragt jeden Schritt)
./target/release/jarvis doctor         # prüft alles lokal
./target/release/jarvis doctor --online  # zusätzlich Websuche + Read-only-Integrationen live
```

### Websuche
Funktioniert ohne Konto (DuckDuckGo-HTML). Optional ist Brave Search: Key
unter brave.com/search/api erstellen (Konditionen und kostenloses Kontingent
dort prüfen), dann `jarvis set-secret brave_api_key`. Optional ist auch eine
eigene SearXNG-Instanz (`web.searxng_url`).

### Outlook-Mail und Teams (Microsoft 365)
1. Im [Entra Admin Center](https://entra.microsoft.com) → *App-Registrierungen* → *Neu*. Name „JARVIS (lokal)“, Konten in beliebigem Organisationsverzeichnis.
2. *Authentifizierung* → „Öffentliche Clientflows zulassen“ = **Ja**. Es wird kein Secret erstellt.
3. *API-Berechtigungen* → Microsoft Graph, **delegiert**: `User.Read`, `Mail.Read`, `Chat.Read`, `offline_access`. **Nichts mit `Send`, `ReadWrite` oder `.All`.**
4. Die Client-ID in `config.toml` → `[microsoft] client_id` eintragen, `mail.provider = "outlook"`, `teams_enabled = true`.
5. `jarvis login microsoft` ausführen. Das Refresh-Token landet im macOS-Schlüsselbund.

⚠️ Schul-Tenants erlauben Benutzern oft keine eigene Zustimmung. Dann muss
die Schul-IT die App einmal freigeben (Admin-Consent). Kosten: keine.

### Gmail
Scope ausschließlich `gmail.readonly`. Die Anmeldung (Google-OAuth für
Desktop-Apps) kommt mit der UI-Phase. Bis dahin lässt sich zum Testen ein Token
mit `jarvis set-secret gmail_access_token` hinterlegen.

### WebUntis
In `config.toml`: `server` (z. B. `https://xyz.webuntis.com`), `school`,
`username`. Das Passwort wird mit `jarvis set-secret webuntis_password`
im Schlüsselbund gespeichert. Prüfen mit `jarvis run webuntis_timetable '{}'`.
Hinweis: Prüfungstermine sind über die klassische JSON-RPC-API für
Schüler oft nicht freigegeben. Stundenplan, Entfall, Vertretung und Ferien
funktionieren.

## 6. Tests

```bash
cargo test --workspace                          # 76 Tests, kein Netz nötig
cargo test -p jarvis-integrations --test security   # Pflicht-Sicherheitstests
cargo test -p jarvis-models -- --ignored        # live gegen lokales Ollama (Mac)
```
