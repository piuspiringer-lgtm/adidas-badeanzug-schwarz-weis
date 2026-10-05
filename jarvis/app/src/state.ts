// Reiner Zustand der Oberfläche (testbar ohne DOM).

import type { AgentEvent, Phase, StepStatus } from "./types";

export type ChatMessage = { id: number; role: "user" | "jarvis" | "system"; text: string; meta?: string };

export type ActivityItem =
  | { id: number; kind: "run"; text: string }
  | { id: number; kind: "info"; text: string }
  | { id: number; kind: "call"; tool: string; args: string; status?: StepStatus; summary?: string; verified?: { ok: boolean; detail: string } }
  | { id: number; kind: "error"; text: string };

export type ReactorState = "idle" | "listening" | "transcribing" | "thinking" | "acting" | "waiting" | "error";

export interface UiState {
  messages: ChatMessage[];
  activity: ActivityItem[];
  phase: Phase | null;
  reactor: ReactorState;
  nextId: number;
}

export const initialState: UiState = { messages: [], activity: [], phase: null, reactor: "idle", nextId: 1 };

export type Action =
  | { type: "user"; text: string }
  | { type: "agent"; event: AgentEvent }
  | { type: "reply"; text: string; meta: string }
  | { type: "failure"; text: string }
  | { type: "waiting"; on: boolean }
  | { type: "voice"; state: "listening" | "transcribing" | "idle" }
  | { type: "reset" };

const PHASE_LABEL: Record<Phase, string> = {
  Understand: "Verstehen",
  Plan: "Planen",
  SelectTools: "Werkzeuge wählen",
  Execute: "Ausführen",
  Observe: "Beobachten",
  Verify: "Prüfen",
  Finish: "Abschließen",
};
export const phaseLabel = (p: Phase) => PHASE_LABEL[p];
export const PHASES: Phase[] = ["Understand", "Plan", "SelectTools", "Execute", "Observe", "Verify", "Finish"];

const MAX_ACTIVITY = 200;

export function reducer(s: UiState, a: Action): UiState {
  const id = s.nextId;
  const push = (item: ActivityItem) => [...s.activity, item].slice(-MAX_ACTIVITY);
  switch (a.type) {
    case "user":
      return {
        ...s,
        nextId: id + 2,
        messages: [...s.messages, { id, role: "user", text: a.text }],
        activity: push({ id: id + 1, kind: "run", text: a.text }),
        reactor: "thinking",
        phase: null,
      };
    case "reply":
      return { ...s, nextId: id + 1, messages: [...s.messages, { id, role: "jarvis", text: a.text, meta: a.meta }], reactor: "idle", phase: null };
    case "failure":
      return { ...s, nextId: id + 1, messages: [...s.messages, { id, role: "system", text: a.text }], reactor: "error", phase: null };
    case "voice":
      return { ...s, reactor: a.state };
    case "waiting":
      return { ...s, reactor: a.on ? "waiting" : "acting" };
    case "reset":
      return { ...initialState, nextId: id };
    case "agent": {
      const e = a.event;
      switch (e.type) {
        case "phase": {
          // Phasen zeigt die Phasenleiste; die Aktivität bleibt ruhig.
          const reactor: ReactorState = e.phase === "Execute" || e.phase === "Observe" ? "acting" : "thinking";
          return { ...s, phase: e.phase, reactor: s.reactor === "waiting" ? "waiting" : reactor };
        }
        case "understood":
          return { ...s, nextId: id + 1, activity: push({ id, kind: "info", text: `${e.complexity} · ${e.model} · Modus ${e.mode}` }) };
        case "tools_selected":
          return { ...s, nextId: id + 1, activity: push({ id, kind: "info", text: e.tools.length ? `Werkzeuge: ${e.tools.join(", ")}` : "Keine Werkzeuge nötig" }) };
        case "plan":
          return { ...s, nextId: id + 1, activity: push({ id, kind: "info", text: `Plan:\n${e.text}` }) };
        case "tool_call":
          return { ...s, nextId: id + 1, reactor: "acting", activity: push({ id, kind: "call", tool: e.tool, args: JSON.stringify(e.args) }) };
        case "tool_result":
        case "verified": {
          const idx = [...s.activity].reverse().findIndex((x) => x.kind === "call" && x.tool === e.tool);
          if (idx < 0) return s;
          const i = s.activity.length - 1 - idx;
          const item = s.activity[i] as Extract<ActivityItem, { kind: "call" }>;
          const updated = e.type === "tool_result" ? { ...item, status: e.status, summary: e.summary } : { ...item, verified: { ok: e.ok, detail: e.detail } };
          const activity = [...s.activity];
          activity[i] = updated;
          return { ...s, activity };
        }
        case "error":
          return { ...s, nextId: id + 1, reactor: "error", activity: push({ id, kind: "error", text: e.message }) };
        case "answer":
          return s;
      }
    }
  }
  return s;
}
