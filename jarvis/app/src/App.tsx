import { useCallback, useEffect, useReducer, useState } from "react";
import { getBackend, type Backend } from "./api";
import { Activity, AuditPage, Chat, ConfirmDialog, PhaseTrack, Reactor, StatusPanel, ToolsView } from "./components";
import { initialState, reducer } from "./state";
import type { AuditView, ConfirmRequest, Status, ToolInfo } from "./types";

type Tab = "assistant" | "tools" | "audit";

export default function App() {
  const [backend, setBackend] = useState<Backend | null>(null);
  const [ui, dispatch] = useReducer(reducer, initialState);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<Tab>("assistant");
  const [status, setStatus] = useState<Status | null>(null);
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [audit, setAudit] = useState<AuditView | null>(null);
  const [confirms, setConfirms] = useState<ConfirmRequest[]>([]);

  useEffect(() => {
    getBackend().then(setBackend);
  }, []);

  // Events des Agenten und Bestätigungsanfragen abonnieren.
  useEffect(() => {
    if (!backend) return;
    const offs: Promise<() => void>[] = [
      backend.onAgentEvent((event) => dispatch({ type: "agent", event })),
      backend.onConfirmRequest((r) => {
        setConfirms((c) => [...c, r]);
        dispatch({ type: "waiting", on: true });
      }),
      backend.onConfirmClosed((id) => setConfirms((c) => c.filter((x) => x.id !== id))),
    ];
    backend.tools().then(setTools);
    return () => offs.forEach((p) => p.then((off) => off()));
  }, [backend]);

  // Systemstatus: alle 5 s, aber nur bei sichtbarem Fenster (spart Akku).
  useEffect(() => {
    if (!backend) return;
    let timer: ReturnType<typeof setInterval> | undefined;
    const refresh = () => backend.status().then(setStatus).catch(() => {});
    const start = () => {
      refresh();
      timer = setInterval(refresh, 5000);
    };
    const onVis = () => {
      clearInterval(timer);
      if (document.visibilityState === "visible") start();
    };
    start();
    document.addEventListener("visibilitychange", onVis);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [backend]);

  const refreshAudit = useCallback(() => backend?.audit(200).then(setAudit), [backend]);
  useEffect(() => {
    if (tab === "audit") refreshAudit();
  }, [tab, refreshAudit]);

  const send = async (text: string) => {
    if (!backend) return;
    dispatch({ type: "user", text });
    setBusy(true);
    try {
      const out = await backend.sendMessage(text);
      const secs = (out.duration_ms / 1000).toFixed(1);
      const meta = `${out.model} · ${out.steps.length} Schritt${out.steps.length === 1 ? "" : "e"} · ${secs} s`;
      dispatch({ type: "reply", text: out.answer, meta });
    } catch (e) {
      dispatch({ type: "failure", text: String(e) });
    } finally {
      setBusy(false);
      backend.status().then(setStatus).catch(() => {});
    }
  };

  const answerConfirm = useCallback(
    (id: string, approved: boolean) => {
      backend?.confirm(id, approved);
      setConfirms((c) => c.filter((x) => x.id !== id));
      dispatch({ type: "waiting", on: false });
    },
    [backend],
  );

  const reset = async () => {
    await backend?.reset();
    dispatch({ type: "reset" });
  };

  const current = confirms[0];

  return (
    <div className="app">
      <nav className="rail" aria-label="Bereiche">
        <div className="rail__logo" aria-hidden="true">J</div>
        <button className={tab === "assistant" ? "is-active" : ""} onClick={() => setTab("assistant")}>
          Assistent
        </button>
        <button className={tab === "tools" ? "is-active" : ""} onClick={() => setTab("tools")}>
          Werkzeuge
        </button>
        <button className={tab === "audit" ? "is-active" : ""} onClick={() => setTab("audit")}>
          Protokoll
        </button>
        <span className="rail__spacer" />
        {tab === "assistant" && (
          <button className="rail__reset" onClick={reset} disabled={busy} title="Gesprächsverlauf leeren">
            Neu
          </button>
        )}
      </nav>

      {tab === "assistant" && (
        <main className="main">
          <header className="hero">
            <Reactor state={ui.reactor} />
            <div className="hero__text">
              <h1>JARVIS</h1>
              <p className="muted">Lokal · privat · {status ? status.profile.main : "…"}</p>
              <PhaseTrack current={ui.phase} />
            </div>
          </header>
          <Chat messages={ui.messages} busy={busy} onSend={send} />
        </main>
      )}
      {tab === "tools" && (
        <main className="main main--page">
          <ToolsView tools={tools} />
        </main>
      )}
      {tab === "audit" && (
        <main className="main main--page">
          <AuditPage view={audit} onRefresh={() => refreshAudit()} />
        </main>
      )}

      <aside className="side">
        <StatusPanel status={status} />
        <Activity items={ui.activity} />
      </aside>

      {current && <ConfirmDialog req={current} onAnswer={(ok) => answerConfirm(current.id, ok)} />}
    </div>
  );
}
