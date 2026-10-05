// Mock-Backend für Browser-Entwicklung und Tests. Simuliert den Agenten
// inklusive Bestätigungsdialog – ohne echte Dateien oder Modelle.

import type { Backend } from "./api";
import type { AgentEvent, AuditEntry, ConfirmRequest, Outcome, Status, ToolInfo } from "./types";

type Cb<T> = (v: T) => void;

const tool = (name: string, integration: string, access: ToolInfo["spec"]["access"], risk: ToolInfo["spec"]["risk"], confirmation: ToolInfo["spec"]["confirmation"], description: string, capabilities: string[]): ToolInfo => ({
  spec: { name, integration, access, risk, confirmation, description, capabilities },
});

const TOOLS: ToolInfo[] = [
  tool("fs_search", "Filesystem", "Read", "Low", "Never", "Sucht Dateien per Namensmuster (Glob) und optional nach Textinhalt.", ["FsRead"]),
  tool("fs_list", "Filesystem", "Read", "Low", "Never", "Listet den Inhalt eines Ordners.", ["FsRead"]),
  tool("fs_read", "Filesystem", "Read", "Low", "Never", "Liest eine Textdatei; mit 'query' nur die relevanten Abschnitte.", ["FsRead"]),
  tool("fs_create", "Filesystem", "Write", "Medium", "WhenRisky", "Erstellt eine neue Textdatei (Überschreiben nur mit Bestätigung).", ["FsWrite"]),
  tool("fs_move", "Filesystem", "Write", "Medium", "Always", "Verschiebt eine Datei oder einen Ordner.", ["FsWrite"]),
  tool("fs_trash", "Filesystem", "Destructive", "High", "Always", "Legt eine Datei oder einen Ordner in den Papierkorb.", ["FsTrash"]),
  tool("web_research", "Web", "Write", "Low", "Never", "Recherchiert eine Frage über mehrere Quellen, nutzt Cache.", ["NetworkGet", "MemoryRead", "MemoryWrite"]),
  tool("memory_recall", "Memory", "Read", "Low", "Never", "Sucht in gemerkten Informationen und Präferenzen.", ["MemoryRead"]),
];

export function mockBackend(): Backend {
  const agentCbs: Cb<AgentEvent>[] = [];
  const confirmCbs: Cb<ConfirmRequest>[] = [];
  const closedCbs: Cb<string>[] = [];
  const pending = new Map<string, Cb<boolean>>();
  const audit: AuditEntry[] = [];
  let busy = false;
  const emit = (e: AgentEvent) => agentCbs.forEach((cb) => cb(e));
  const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const log = (tool: string, decision: string, outcome: string, args: unknown) =>
    audit.unshift({ id: audit.length + 1, ts: Math.floor(Date.now() / 1000), tool, origin: "Agent", args_json: JSON.stringify(args), decision, outcome });
  const sub = <T,>(list: Cb<T>[], cb: Cb<T>) => {
    list.push(cb);
    return Promise.resolve(() => void list.splice(list.indexOf(cb), 1));
  };

  return {
    async sendMessage(text) {
      busy = true;
      const steps: Outcome["steps"] = [];
      const destructive = /lösch|papierkorb|entfern/i.test(text);
      const files = destructive || /datei|ordner|dokument|pdf/i.test(text);
      emit({ type: "phase", phase: "Understand" });
      await wait(250);
      emit({ type: "phase", phase: "SelectTools" });
      emit({ type: "tools_selected", tools: files ? (destructive ? ["fs_trash", "fs_search", "fs_list"] : ["fs_search", "fs_list", "fs_read"]) : [] });
      emit({ type: "understood", intent: text.toLowerCase().slice(0, 30), complexity: files ? "Standard" : "Simple", mode: "Performance", model: "qwen3:8b" });
      emit({ type: "phase", phase: "Execute" });
      await wait(400);
      let answer = "Hallo! Ich bin JARVIS, dein lokaler Assistent. Ich kann Dateien finden, lesen und ordnen, im Web recherchieren und mir Dinge merken.";
      if (files) {
        const args = { pattern: "*.pdf" };
        emit({ type: "tool_call", tool: "fs_search", args });
        emit({ type: "phase", phase: "Observe" });
        await wait(350);
        emit({ type: "tool_result", tool: "fs_search", status: "Ok", summary: "3 Treffer" });
        log("fs_search", "allowed", "ok (212 Zeichen)", args);
        steps.push({ tool: "fs_search", args, status: "Ok", summary: "3 Treffer", verification: null, duration_ms: 41 });
        answer = "Ich habe 3 PDFs gefunden: Rechnung_Sept.pdf, Zeugnis.pdf und Skript_Mathe.pdf (alle in Dokumente).";
      }
      if (destructive) {
        const args = { path: "~/Documents/alt/Rechnung_2019.pdf" };
        emit({ type: "phase", phase: "Execute" });
        emit({ type: "tool_call", tool: "fs_trash", args });
        const id = Math.random().toString(16).slice(2);
        const approved = await new Promise<boolean>((resolve) => {
          pending.set(id, resolve);
          confirmCbs.forEach((cb) =>
            cb({ id, tool: "fs_trash", description: "Legt eine Datei oder einen Ordner in den Papierkorb.", access: "Destructive", risk: "High", reason: "/Users/du/Documents/alt/Rechnung_2019.pdf in den Papierkorb legen", paths: ["/Users/du/Documents/alt/Rechnung_2019.pdf"] }),
          );
        });
        closedCbs.forEach((cb) => cb(id));
        emit({ type: "phase", phase: "Observe" });
        const status = approved ? "Ok" : "NotConfirmed";
        const summary = approved ? "Im Papierkorb: Rechnung_2019.pdf" : "vom Benutzer nicht bestätigt";
        emit({ type: "tool_result", tool: "fs_trash", status, summary });
        log("fs_trash", approved ? "confirmed" : "not_confirmed", summary, args);
        if (approved) emit({ type: "verified", tool: "fs_trash", ok: true, detail: "Rechnung_2019.pdf im Papierkorb" });
        steps.push({ tool: "fs_trash", args, status, summary, verification: approved ? "im Papierkorb" : null, duration_ms: 12 });
        answer = approved ? "Rechnung_2019.pdf liegt jetzt im Papierkorb." : "Okay, ich habe nichts gelöscht.\n\nHinweis zu Aktionen:\n• fs_trash (nicht bestätigt – nicht ausgeführt): vom Benutzer nicht bestätigt";
      }
      emit({ type: "phase", phase: "Verify" });
      await wait(150);
      emit({ type: "phase", phase: "Finish" });
      emit({ type: "answer", text: answer });
      busy = false;
      return { answer, steps, success: true, intent: "", model: "qwen3:8b", prompt_tokens: 812, output_tokens: 64, duration_ms: 1420 };
    },
    async confirm(id, approved) {
      pending.get(id)?.(approved);
      pending.delete(id);
    },
    async reset() {},
    async status(): Promise<Status> {
      return {
        hardware: { os: "macos", arch: "aarch64", cpu_brand: "Apple M4", cpu_cores: 10, total_ram_gb: 16, apple_silicon: true },
        snapshot: { total_ram_gb: 16, available_ram_gb: 7.4, cpu_usage_percent: 12, battery: { percent: 78, charging: false }, thermal: "Nominal", low_power_mode: false },
        mode: "Balanced",
        profile: { main: "qwen3:8b", fallback: "qwen3:4b", embedding: "nomic-embed-text", context_window: 8192, stt_model: "ggml-small.bin", ai_ram_budget_gb: 7 },
        services: [{ name: "ollama", state: busy ? "Active" : "Idle", users: busy ? 1 : 0, idle_for_ms: 42000, last_error: null }],
        inactive: [["email", "mail.provider nicht gesetzt"], ["teams", "microsoft.teams_enabled = false"], ["webuntis", "Passwort im Schlüsselbund fehlt"]],
        fs_roots: ["/Users/du/Documents", "/Users/du/Desktop", "/Users/du/Downloads"],
        busy,
        voice: { available: true, reason: null, stt_model: "ggml-small.bin", recording: false },
      };
    },
    tools: async () => TOOLS,
    voiceStart: async () => {},
    voiceStop: async () => "Finde meine PDFs im Ordner Dokumente",
    speak: async () => {},
    stopSpeaking: async () => {},
    audit: async (limit) => ({ entries: audit.slice(0, limit), chain_ok: true, checked: audit.length }),
    onAgentEvent: (cb) => sub(agentCbs, cb),
    onConfirmRequest: (cb) => sub(confirmCbs, cb),
    onConfirmClosed: (cb) => sub(closedCbs, cb),
  };
}
