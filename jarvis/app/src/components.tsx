import { useEffect, useRef, useState } from "react";
import type { ActivityItem, ChatMessage, ReactorState } from "./state";
import { PHASES, phaseLabel } from "./state";
import type { AuditView, ConfirmRequest, Phase, Status, ToolInfo } from "./types";

// ---------- Arc Reactor ----------

const REACTOR_LABEL: Record<ReactorState, string> = {
  idle: "Bereit",
  listening: "Hört zu",
  transcribing: "Versteht",
  thinking: "Denkt nach",
  acting: "Arbeitet",
  waiting: "Wartet auf dich",
  error: "Problem",
};

export function Reactor({ state }: { state: ReactorState }) {
  const ticks = Array.from({ length: 36 }, (_, i) => i * 10);
  return (
    <div className={`reactor reactor--${state}`} role="status" aria-label={`JARVIS: ${REACTOR_LABEL[state]}`}>
      <svg viewBox="0 0 200 200" aria-hidden="true">
        <defs>
          <radialGradient id="core" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="var(--core-hot)" />
            <stop offset="55%" stopColor="var(--core)" stopOpacity="0.55" />
            <stop offset="100%" stopColor="var(--core)" stopOpacity="0" />
          </radialGradient>
        </defs>
        <g className="reactor__ticks">
          {ticks.map((a) => (
            <line key={a} x1="100" y1="6" x2="100" y2={a % 30 === 0 ? 16 : 11} transform={`rotate(${a} 100 100)`} />
          ))}
        </g>
        <circle className="reactor__ring reactor__ring--outer" cx="100" cy="100" r="78" />
        <circle className="reactor__ring reactor__ring--dash" cx="100" cy="100" r="64" />
        <circle className="reactor__ring reactor__ring--inner" cx="100" cy="100" r="46" />
        <circle className="reactor__core" cx="100" cy="100" r="38" fill="url(#core)" />
      </svg>
      <span className="reactor__label">{REACTOR_LABEL[state]}</span>
    </div>
  );
}

// ---------- Phasen ----------

export function PhaseTrack({ current }: { current: Phase | null }) {
  const idx = current ? PHASES.indexOf(current) : -1;
  return (
    <ol className="phases" aria-label="Agent-Phasen">
      {PHASES.map((p, i) => (
        <li key={p} className={i === idx ? "is-current" : i < idx ? "is-done" : ""} aria-current={i === idx ? "step" : undefined}>
          {phaseLabel(p)}
        </li>
      ))}
    </ol>
  );
}

// ---------- Chat ----------

export interface Talk {
  available: boolean;
  reason: string | null;
  active: boolean;
  start: () => void;
  stop: () => void;
}

export function Chat({ messages, busy, onSend, talk }: { messages: ChatMessage[]; busy: boolean; onSend: (t: string) => void; talk?: Talk }) {
  const [text, setText] = useState("");
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => end.current?.scrollIntoView?.({ behavior: "smooth", block: "end" }), [messages.length]);
  const send = () => {
    const t = text.trim();
    if (!t || busy) return;
    onSend(t);
    setText("");
  };
  return (
    <section className="chat" aria-label="Unterhaltung">
      <div className="chat__log">
        {messages.length === 0 && (
          <div className="chat__empty">
            <p>Wie kann ich helfen?</p>
            <ul>
              <li>„Finde meine PDFs im Ordner Dokumente“</li>
              <li>„Erstelle eine Notiz einkauf.txt mit Milch und Brot“</li>
              <li>„Merk dir: Ich habe montags Mathe-Nachhilfe“</li>
            </ul>
          </div>
        )}
        {messages.map((m) => (
          <article key={m.id} className={`msg msg--${m.role}`}>
            <header>{m.role === "user" ? "Du" : m.role === "jarvis" ? "JARVIS" : "System"}</header>
            {/* Nur Text – nie HTML aus Modell oder Tools rendern. */}
            <p>{m.text}</p>
            {m.meta && <footer>{m.meta}</footer>}
          </article>
        ))}
        <div ref={end} />
      </div>
      <form
        className="chat__input"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <textarea
          aria-label="Nachricht an JARVIS"
          placeholder={busy ? "JARVIS arbeitet …" : "Nachricht an JARVIS – Enter senden, Shift+Enter neue Zeile"}
          value={text}
          rows={2}
          maxLength={4000}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        {talk && (
          <button
            type="button"
            className={`mic ${talk.active ? "is-active" : ""}`}
            disabled={busy || !talk.available}
            title={talk.available ? "Gedrückt halten zum Sprechen (oder ⌥+Leertaste halten)" : talk.reason ?? "Sprache nicht verfügbar"}
            aria-label="Sprechtaste (gedrückt halten)"
            aria-pressed={talk.active}
            onPointerDown={(e) => {
              e.preventDefault();
              talk.start();
            }}
            onPointerUp={talk.stop}
            onPointerLeave={() => talk.active && talk.stop()}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <rect x="9" y="3" width="6" height="11" rx="3" />
              <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" />
            </svg>
          </button>
        )}
        <button type="submit" disabled={busy || !text.trim()}>
          Senden
        </button>
      </form>
    </section>
  );
}

// ---------- Aktivität ----------

const STATUS_ICON: Record<string, string> = { Ok: "✓", Blocked: "⛔", NotConfirmed: "✋", Failed: "✗", VerificationFailed: "⚠" };

export function Activity({ items }: { items: ActivityItem[] }) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => end.current?.scrollIntoView?.({ block: "end" }), [items]);
  return (
    <section className="panel activity" aria-label="Aktivität">
      <h2>Aktivität</h2>
      {items.length === 0 && <p className="muted">Hier erscheint jeder Schritt von JARVIS – nichts passiert versteckt.</p>}
      <ul>
        {items.map((it) => {
          if (it.kind === "run") return <li key={it.id} className="act act--run">{it.text}</li>;
          if (it.kind === "info") return <li key={it.id} className="act act--info">{it.text}</li>;
          if (it.kind === "error") return <li key={it.id} className="act act--error">{it.text}</li>;
          return (
            <li key={it.id} className={`act act--call ${it.status ? `is-${it.status.toLowerCase()}` : "is-running"}`}>
              <div>
                <span className="act__icon">{it.status ? STATUS_ICON[it.status] : "…"}</span>
                <code>{it.tool}</code>
              </div>
              <div className="act__args">{it.args}</div>
              {it.summary && <div className="act__summary">{it.summary}</div>}
              {it.verified && <div className={`act__verify ${it.verified.ok ? "ok" : "bad"}`}>Geprüft: {it.verified.detail}</div>}
            </li>
          );
        })}
      </ul>
      <div ref={end} />
    </section>
  );
}

// ---------- Systemstatus ----------

function Meter({ label, value, max, unit, warn }: { label: string; value: number; max: number; unit: string; warn?: boolean }) {
  const pct = Math.max(0, Math.min(100, (value / max) * 100));
  return (
    <div className="meter">
      <div className="meter__head">
        <span>{label}</span>
        <span>
          {value.toFixed(1)} {unit}
        </span>
      </div>
      <div className="meter__bar" role="meter" aria-label={label} aria-valuenow={value} aria-valuemin={0} aria-valuemax={max}>
        <div className={warn ? "warn" : ""} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

const MODE_LABEL: Record<string, string> = { Performance: "Leistung", Balanced: "Ausgewogen", Saver: "Sparen", Critical: "Kritisch" };

export function StatusPanel({ status }: { status: Status | null }) {
  if (!status) return <section className="panel status"><h2>System</h2><p className="muted">Lade …</p></section>;
  const s = status.snapshot;
  const used = s.total_ram_gb - s.available_ram_gb;
  return (
    <section className="panel status" aria-label="Systemstatus">
      <h2>
        System <span className={`badge badge--mode-${status.mode.toLowerCase()}`}>{MODE_LABEL[status.mode]}</span>
      </h2>
      <dl className="kv">
        <dt>Chip</dt>
        <dd>{status.hardware.cpu_brand}</dd>
        <dt>Modell</dt>
        <dd>
          {status.mode === "Saver" || status.mode === "Critical" ? status.profile.fallback : status.profile.main}
          <span className="muted"> · Fallback {status.profile.fallback}</span>
        </dd>
        {s.battery && (
          <>
            <dt>Akku</dt>
            <dd>
              {s.battery.percent} % {s.battery.charging ? "⚡" : ""}
            </dd>
          </>
        )}
      </dl>
      <Meter label="RAM belegt" value={used} max={s.total_ram_gb} unit="GB" warn={s.available_ram_gb < 3} />
      <Meter label="CPU" value={s.cpu_usage_percent} max={100} unit="%" warn={s.cpu_usage_percent > 85} />
      <h3>Dienste</h3>
      <ul className="services">
        {status.services.map((sv) => (
          <li key={sv.name}>
            <span className={`dot dot--${sv.state.toLowerCase()}`} aria-hidden="true" />
            {sv.name}
            <span className="muted">{sv.state}</span>
          </li>
        ))}
      </ul>
      {status.inactive.length > 0 && (
        <>
          <h3>Nicht eingerichtet</h3>
          <ul className="inactive">
            {status.inactive.map(([name, why]) => (
              <li key={name} title={why}>
                {name}
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

// ---------- Bestätigung ----------

const RISK_LABEL: Record<string, string> = { Low: "niedrig", Medium: "mittel", High: "hoch", Critical: "kritisch" };

export function ConfirmDialog({ req, onAnswer }: { req: ConfirmRequest; onAnswer: (approved: boolean) => void }) {
  const deny = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    deny.current?.focus();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onAnswer(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [req.id, onAnswer]);
  const destructive = req.access === "Destructive";
  return (
    <div className="modal" role="presentation">
      <div className={`dialog ${destructive ? "dialog--danger" : ""}`} role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby="confirm-reason">
        <h2 id="confirm-title">Bestätigung erforderlich</h2>
        <p className="dialog__tool">
          <code>{req.tool}</code>
          <span className={`badge badge--risk-${req.risk.toLowerCase()}`}>Risiko {RISK_LABEL[req.risk] ?? req.risk}</span>
          <span className="badge">{req.access}</span>
        </p>
        <p id="confirm-reason" className="dialog__reason">{req.reason}</p>
        {req.paths.length > 0 && (
          <ul className="dialog__paths">
            {req.paths.map((p) => (
              <li key={p}>
                <code>{p}</code>
              </li>
            ))}
          </ul>
        )}
        <p className="muted">{req.description}</p>
        <div className="dialog__actions">
          <button ref={deny} className="btn-secondary" onClick={() => onAnswer(false)}>
            Ablehnen
          </button>
          <button className={destructive ? "btn-danger" : "btn-primary"} onClick={() => onAnswer(true)}>
            {destructive ? "Ja, ausführen" : "Erlauben"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------- Werkzeuge ----------

const CONFIRM_LABEL: Record<string, string> = { Never: "nein", WhenRisky: "bei Risiko", Always: "immer" };

export function ToolsView({ tools }: { tools: ToolInfo[] }) {
  const groups = tools.reduce<Record<string, ToolInfo[]>>((g, t) => {
    (g[t.spec.integration] ??= []).push(t);
    return g;
  }, {});
  return (
    <section className="page" aria-label="Werkzeuge">
      <h1>Werkzeuge</h1>
      <p className="muted">Jedes Werkzeug läuft über die Permission-Schicht. E-Mail, Teams und WebUntis sind technisch nur lesbar.</p>
      {Object.entries(groups).map(([integration, list]) => (
        <div key={integration} className="panel">
          <h2>{integration}</h2>
          <table className="table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Zugriff</th>
                <th>Risiko</th>
                <th>Bestätigung</th>
                <th>Beschreibung</th>
              </tr>
            </thead>
            <tbody>
              {list.map((t) => (
                <tr key={t.spec.name}>
                  <td>
                    <code>{t.spec.name}</code>
                  </td>
                  <td>
                    <span className={`badge badge--access-${t.spec.access.toLowerCase()}`}>{t.spec.access}</span>
                  </td>
                  <td>
                    <span className={`badge badge--risk-${t.spec.risk.toLowerCase()}`}>{RISK_LABEL[t.spec.risk]}</span>
                  </td>
                  <td>{CONFIRM_LABEL[t.spec.confirmation]}</td>
                  <td>{t.spec.description}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ))}
    </section>
  );
}

// ---------- Protokoll ----------

const DECISION_LABEL: Record<string, string> = {
  allowed: "erlaubt",
  confirmed: "bestätigt",
  not_confirmed: "abgelehnt",
  denied: "verweigert",
  hard_denied: "gesperrt",
  unknown: "unbekannt",
  invalid: "ungültig",
};

export function AuditPage({ view, onRefresh }: { view: AuditView | null; onRefresh: () => void }) {
  return (
    <section className="page" aria-label="Protokoll">
      <h1>
        Protokoll
        <button className="btn-secondary small" onClick={onRefresh}>
          Aktualisieren
        </button>
      </h1>
      {view && (
        <p className={view.chain_ok ? "ok" : "bad"}>
          {view.chain_ok ? `Hash-Kette intakt (${view.checked} Einträge geprüft)` : "⚠ Hash-Kette beschädigt – das Protokoll wurde verändert!"}
        </p>
      )}
      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>#</th>
              <th>Zeit</th>
              <th>Werkzeug</th>
              <th>Von</th>
              <th>Entscheidung</th>
              <th>Ergebnis</th>
            </tr>
          </thead>
          <tbody>
            {view?.entries.map((e) => (
              <tr key={e.id} className={`decision-${e.decision}`}>
                <td>{e.id}</td>
                <td>{new Date(e.ts * 1000).toLocaleString("de-AT")}</td>
                <td>
                  <code title={e.args_json}>{e.tool}</code>
                </td>
                <td>{e.origin}</td>
                <td>{DECISION_LABEL[e.decision] ?? e.decision}</td>
                <td>{e.outcome}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {view?.entries.length === 0 && <p className="muted">Noch keine Einträge.</p>}
      </div>
    </section>
  );
}
