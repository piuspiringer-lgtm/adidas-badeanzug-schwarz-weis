// Brücke zum Rust-Kern. In Tauri: echte Befehle/Events. Im Browser
// (Entwicklung, Tests, Screenshots): ein Mock-Backend mit Beispieldaten.

import type { AgentEvent, AuditView, ConfirmRequest, Outcome, Status, ToolInfo } from "./types";
import { mockBackend } from "./mock";

export interface Backend {
  sendMessage(text: string): Promise<Outcome>;
  confirm(id: string, approved: boolean): Promise<void>;
  reset(): Promise<void>;
  status(): Promise<Status>;
  tools(): Promise<ToolInfo[]>;
  audit(limit: number): Promise<AuditView>;
  voiceStart(): Promise<void>;
  voiceStop(): Promise<string>;
  speak(text: string): Promise<void>;
  stopSpeaking(): Promise<void>;
  onAgentEvent(cb: (e: AgentEvent) => void): Promise<() => void>;
  onConfirmRequest(cb: (r: ConfirmRequest) => void): Promise<() => void>;
  onConfirmClosed(cb: (id: string) => void): Promise<() => void>;
}

const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  return {
    sendMessage: (text) => invoke<Outcome>("send_message", { text }),
    confirm: (id, approved) => invoke("confirm_response", { id, approved }),
    reset: () => invoke("reset_conversation"),
    status: () => invoke<Status>("system_status"),
    tools: () => invoke<ToolInfo[]>("list_tools"),
    audit: (limit) => invoke<AuditView>("audit_log", { limit }),
    voiceStart: () => invoke("voice_start"),
    voiceStop: () => invoke<string>("voice_stop"),
    speak: (text) => invoke("speak", { text }),
    stopSpeaking: () => invoke("stop_speaking"),
    onAgentEvent: (cb) => listen<AgentEvent>("agent-event", (e) => cb(e.payload)),
    onConfirmRequest: (cb) => listen<ConfirmRequest>("confirm-request", (e) => cb(e.payload)),
    onConfirmClosed: (cb) => listen<string>("confirm-closed", (e) => cb(e.payload)),
  };
}

let backend: Promise<Backend> | null = null;

export function getBackend(): Promise<Backend> {
  if (!backend) backend = isTauri() ? tauriBackend() : Promise.resolve(mockBackend());
  return backend;
}
