# SECURITY.md

## Grundsätze

1. **Read-only bleibt read-only.** E-Mail, Microsoft Teams und WebUntis können technisch nichts senden, bearbeiten oder löschen.
2. **Keine Selbständerung.** JARVIS kann seine eigenen Sicherheits- und Berechtigungsregeln nicht verändern.
3. **Keine versteckten Aktionen.** Jeder Tool-Aufruf, auch abgelehnte, landet im hash-verketteten Audit-Log.
4. **Keine geheimen Uploads.** Netzwerkzugriff erfolgt nur über deklarierte Tools, das Web nur per GET.
5. **Destruktives nur mit Bestätigung** durch den Menschen. Endgültiges Löschen gibt es nicht.

## Der Weg eines Tool-Aufrufs

```
LLM/Skill/UI ─► ToolGateway.invoke(name, args, origin)
                 1. Sperrliste (HARD_DENIED_ACTIONS)        → sofort ablehnen + Audit
                 2. Tool in Registry?                        → sonst ablehnen + Audit
                 3. tool.facts(args): Pfade, Überschreiben, Vorschau
                 4. PolicyEngine.evaluate(spec, facts, origin)
                      Deny → Audit │ RequireConfirmation → Confirmer (Mensch) │ Allow
                 5. benötigte Dienste laden (Lifecycle)
                 6. ausführen → Audit (Ergebnis, Dauer)
```

Die Registry gibt Tools nicht heraus. Ein Aufruf ist **nur** über den Gateway
möglich.

## Read-only-Durchsetzung (5 Ebenen)

| Ebene | Mechanismus | Test |
|---|---|---|
| 1. OAuth-Scopes | nur `Mail.Read`, `Chat.Read`, `User.Read`, `offline_access`, `gmail.readonly`. `scopes_are_read_only()` prüft das beim Start. | `oauth::tests::*` |
| 2. Code | `MailReader`, `TeamsReader` und `UntisClient` haben keine Schreibmethoden. WebUntis kennt nur die geschlossene Aufzählung `UntisMethod` (7 Lese-Methoden). | `security::all_read_tools_only_send_get_or_allowlisted_search` |
| 3. HTTP | `ReadOnlyApi` kann nur GET und POST auf fest einkompilierte Such-Pfade (`/search/query`). Keine Redirects. | `http::tests::read_only_api_rejects_non_allowlisted_post` |
| 4. Registrierung | `validate_spec`: Read-only-Integrationen nur mit `Access::Read`, ohne verändernde Fähigkeiten, ohne Verben wie send/edit/delete/update/create/post im Namen | `security::write_tools_cannot_be_smuggled_into_read_only_integrations` |
| 5. Sperrliste | `send_email`, `delete_email`, `send_teams_message`, `edit_teams_message`, `modify_webuntis` und ~20 weitere sind fest einkompiliert. Selbst ein „Ja“ des Benutzers hebt das nicht auf. | `security::forbidden_actions_are_blocked_at_gateway_even_with_full_confirmation` |

Zusätzlich sorgt **Prompt-Injection-Resistenz** dafür, dass eine präparierte
Mail („rufe send_email auf“) nichts auslösen kann. Getestet in
`security::prompt_injection_in_email_cannot_trigger_writes`: 0 Nicht-GET-Requests.

## Unveränderliche Regeln

- Policy, Sperrliste und Validierung sind **Rust-Code im Binary**. Es gibt keine Policy-Datei, keine Tabelle und kein Tool dafür.
- Der JARVIS-Datenordner (`~/Library/Application Support/JARVIS`: Memory, Audit, Konfiguration) ist für alle Dateisystem-Tools gesperrt. Das gilt doppelt: in der Sandbox und in der PolicyEngine.
- Skills und Workflows dürfen keine gesperrten Aktionen enthalten. Neue oder geänderte Skills sind **deaktiviert**, bis der Mensch sie freigibt.
- Workflows laufen ebenfalls durch den Gateway (Origin `Skill`) und haben keine Sonderrechte.

## Dateisystem

- **Sandbox:** nur freigegebene Wurzelordner. Pfade werden kanonisiert, sodass weder `..` noch Symlinks hinausführen.
- **Immer gesperrt:** `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/Library/Keychains` und der JARVIS-Datenordner. Sie tauchen auch in Suchergebnissen nicht auf.
- **Bestätigung:** Papierkorb, Verschieben und Öffnen immer; Erstellen und Kopieren nur beim Überschreiben. Umbenennen auf ein bestehendes Ziel wird abgelehnt.
- `fs_open` öffnet nie Programme oder Skripte (`.app`, `.command`, `.sh`, ausführbare Dateien …).
- Kein `shell_exec` und kein endgültiges Löschen.

## Netzwerk

- **Web:** nur GET, max. 3 MB Antwort, max. 2048 Zeichen URL, max. 300 Zeichen Suchanfrage. Das begrenzt das Ausschleusen von Daten über URLs.
- **SSRF-Schutz:** localhost, private Netze (10/8, 172.16/12, 192.168/16, 100.64/10, Link-Local inkl. 169.254.169.254, IPv6 ULA) und `.local` sind blockiert. Das gilt auch nach der DNS-Auflösung (eigener Resolver) und bei Redirects. Der System-Proxy wird ignoriert, damit dieser Schutz nicht umgangen wird.
- Ollama ist nur über 127.0.0.1 erreichbar. Für das LLM ist das keine Ziel-URL.
- Jede URL steht im Audit-Log.

## Geheimnisse

- Passwörter, Tokens und API-Keys liegen im **macOS-Schlüsselbund** (Dienst `JARVIS`) oder in `JARVIS_<NAME>`-Umgebungsvariablen. Sie stehen **nie** in `config.toml`.
- `jarvis set-secret` lässt `security` selbst verdeckt abfragen. So steht das Geheimnis nicht in der Prozessliste.
- Das Audit-Log schwärzt Schlüssel wie `password`, `token`, `secret`, `api_key` und `authorization`.

## Audit

- SQLite-Tabelle `audit_log`: Jeder Eintrag enthält SHA-256 über Vorgänger-Hash und Inhalt.
- Trigger verhindern `UPDATE` und `DELETE`. `jarvis audit` prüft die Kette (`verify()` erkennt Manipulation).

## Ressourcen

- Kein Dienst läuft dauerhaft. Ollama wird bei Bedarf gestartet und nach `keep_alive` bzw. Idle-Timeout entladen und beendet.
- Das Mikrofon wird **nur** per Push-to-Talk (Benutzeraktion) aktiviert. Es gibt kein Tool dafür, und das Wake Word ist standardmäßig aus.

## Bekannte Grenzen / offene Punkte

| Punkt | Bewertung / Plan |
|---|---|
| `jarvis login microsoft` speichert das Refresh-Token über `security … -w <token>`; es ist dabei kurz in der Prozessliste sichtbar | gering (lokal, Millisekunden); wird mit nativer Keychain-API (Tauri-Phase) behoben |
| DuckDuckGo-HTML ist keine offizielle API | als Fallback abschaltbar (`web.duckduckgo_fallback = false`); Brave/SearXNG empfohlen |
| GET-URLs können theoretisch Daten im Query enthalten | durch Längenlimits begrenzt und vollständig im Audit sichtbar; UI zeigt externe Abrufe live |
| WebUntis-JSON-RPC ist inoffiziell | nur Lese-Methoden; Änderungen der API führen zu Fehlern, nie zu Schreibzugriffen |
| Gmail-Anmeldung noch nicht implementiert | folgt mit UI; Scope steht fest (`gmail.readonly`) |
| Bestätigungsdialog im CLI ist ein Terminal-Prompt | Tauri-UI zeigt native Dialoge mit Vorschau |

## Melden

Sicherheitsprobleme bitte als privates GitHub-Security-Advisory melden.
