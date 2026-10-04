# MEMORY.md

Datei: `~/Library/Application Support/JARVIS/brain.db` (SQLite, WAL, FTS5).
Das Modell wird **nicht** nachtrainiert. JARVIS lernt, indem es Erfahrungen
speichert und für das Ranking nutzt.

| Tabelle | Inhalt | Nutzung |
|---|---|---|
| `preferences` | Schlüssel/Wert (Sprache, Stimme, Gewohnheiten) | Systemprompt |
| `facts` (+FTS) | gelernte Fakten mit Quelle | Kontext bei passenden Fragen |
| `workflows` (+FTS) | benannte Abfolgen von Tool-Aufrufen je Intent, Erfolge/Fehler | Ranking nach Laplace-geglätteter Erfolgsquote |
| `workflow_runs` | jeder Lauf: Erfolg, Dauer, Notiz | Statistik |
| `skills` | wiederverwendbare Workflows mit Version | **standardmäßig deaktiviert**; Freigabe nur durch den Menschen |
| `tool_stats` | je Tool und Intent: Erfolge, Fehler, Ø-Dauer | Tool-Vorauswahl |
| `errors_solutions` | Fehlersignatur → Lösung, Häufigkeit | bei erneutem Fehler als Hinweis |
| `research_queries` | normalisierte Suchanfrage → Ergebnisse (TTL 6 h) | spart Suchen |
| `research_pages` | extrahierter Seitentext (TTL 24 h) | spart Seitenabrufe |
| `research_chunks` (+FTS) | deduplizierte Abschnitte (Hash) | Wissen wiederverwenden (7 Tage) |
| `audit_log` | hash-verkettet, unveränderlich (Trigger) | Nachvollziehbarkeit |

Schutz: Workflows und Skills mit gesperrten Aktionen werden beim Speichern
abgelehnt. Der Datenbankordner ist für alle Agent-Tools gesperrt.

Geplant (Phase 2): Embeddings über `nomic-embed-text` + `sqlite-vec` für
semantische Suche zusätzlich zu FTS5.
