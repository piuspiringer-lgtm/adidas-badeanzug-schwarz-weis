// Spiegel der Rust-Typen (serde-Serialisierung).

export type Phase = "Understand" | "Plan" | "SelectTools" | "Execute" | "Observe" | "Verify" | "Finish";
export type StepStatus = "Ok" | "Blocked" | "NotConfirmed" | "Failed" | "VerificationFailed";
export type Mode = "Performance" | "Balanced" | "Saver" | "Critical";

export type AgentEvent =
  | { type: "phase"; phase: Phase }
  | { type: "understood"; intent: string; complexity: string; mode: Mode; model: string }
  | { type: "plan"; text: string }
  | { type: "tools_selected"; tools: string[] }
  | { type: "tool_call"; tool: string; args: unknown }
  | { type: "tool_result"; tool: string; status: StepStatus; summary: string }
  | { type: "verified"; tool: string; ok: boolean; detail: string }
  | { type: "answer"; text: string }
  | { type: "error"; message: string };

export interface Step {
  tool: string;
  args: unknown;
  status: StepStatus;
  summary: string;
  verification: string | null;
  duration_ms: number;
}

export interface Outcome {
  answer: string;
  steps: Step[];
  success: boolean;
  intent: string;
  model: string;
  prompt_tokens: number;
  output_tokens: number;
  duration_ms: number;
}

export interface ConfirmRequest {
  id: string;
  tool: string;
  description: string;
  access: string;
  risk: string;
  reason: string;
  paths: string[];
}

export interface ServiceStatus {
  name: string;
  state: "Off" | "Loading" | "Ready" | "Active" | "Idle" | "Unloading" | "Error";
  users: number;
  idle_for_ms: number;
  last_error: string | null;
}

export interface Status {
  hardware: { os: string; arch: string; cpu_brand: string; cpu_cores: number; total_ram_gb: number; apple_silicon: boolean };
  snapshot: {
    total_ram_gb: number;
    available_ram_gb: number;
    cpu_usage_percent: number;
    battery: { percent: number; charging: boolean } | null;
    thermal: string;
    low_power_mode: boolean;
  };
  mode: Mode;
  profile: { main: string; fallback: string; embedding: string; context_window: number; stt_model: string; ai_ram_budget_gb: number };
  services: ServiceStatus[];
  inactive: [string, string][];
  fs_roots: string[];
  busy: boolean;
}

export interface ToolInfo {
  spec: {
    name: string;
    description: string;
    integration: string;
    access: "Read" | "Write" | "Destructive";
    risk: "Low" | "Medium" | "High" | "Critical";
    confirmation: "Never" | "WhenRisky" | "Always";
    capabilities: string[];
  };
}

export interface AuditEntry {
  id: number;
  ts: number;
  tool: string;
  origin: string;
  args_json: string;
  decision: string;
  outcome: string;
}

export interface AuditView {
  entries: AuditEntry[];
  chain_ok: boolean;
  checked: number;
}
