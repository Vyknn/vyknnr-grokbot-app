// Types mirroring assistant-core's serde output.

export type ProviderId = "hermes" | "claude_cli" | "api";
export type Role = "user" | "assistant" | "command_result" | "notice";
export type ApprovalKind = "auto" | "user" | "user_edited" | "denied";

export type DecisionSource =
  | { kind: "jev"; model: string }
  | { kind: "fallback"; reason: string }
  | { kind: "manual" };

export interface Conversation {
  id: number;
  title: string;
  cwd: string;
  created_at: string;
  updated_at: string;
}

export interface Message {
  id: number;
  conversation_id: number;
  role: Role;
  provider: ProviderId | null;
  content: string;
  created_at: string;
}

export interface CommandRecord {
  id: number;
  conversation_id: number;
  command: string;
  cwd: string;
  check: CommandCheck | null;
  approval: ApprovalKind;
  exit_code: number | null;
  output: string;
  truncated: boolean;
  started_at: string;
  finished_at: string | null;
}

export interface DecisionRecord {
  id: number;
  message_id: number;
  kind: "route" | "command";
  payload: unknown;
  created_at: string;
}

export type RunChunk = { stdout: string } | { stderr: string };

export type AppEvent =
  | {
      kind: "command_started";
      conversation_id: number;
      command_id: number;
      command: string;
      cwd: string;
      approval: ApprovalKind;
    }
  | { kind: "command_output"; command_id: number; chunk: RunChunk }
  | { kind: "command_finished"; command_id: number; exit_code: number | null; timed_out: boolean }
  | { kind: "command_denied"; conversation_id: number; command: string }
  | {
      kind: "route_decided";
      conversation_id: number;
      provider: ProviderId;
      source: DecisionSource;
      probability: number | null;
    }
  | { kind: "assistant_delta"; conversation_id: number; text: string }
  | { kind: "turn_finished"; conversation_id: number }
  | { kind: "turn_failed"; conversation_id: number; provider: ProviderId; error: string };

export interface CommandCheck {
  verdict: "auto_run" | "ask_user";
  is_safe: number | null;
  risk: string | null;
  risk_p: number | null;
  source: DecisionSource;
}

export interface ApprovalRequest {
  id: string;
  conversation_id: number;
  command: string;
  cwd: string;
  check: CommandCheck;
  reason: { kind: "needs_review" } | { kind: "limit_reached"; count: number };
}

export type TurnOutcome =
  | { kind: "completed"; provider: ProviderId }
  | { kind: "needs_provider_choice"; user_message_id: number; ranked: [ProviderId, number][] }
  | { kind: "failed"; provider: ProviderId; error: string }
  | { kind: "cancelled"; provider: ProviderId };

export interface TurnOutcomeEvent {
  conversation_id: number;
  outcome: TurnOutcome | null;
  error: string | null;
}

export type Availability = "ready" | { unavailable: string };

export interface ProviderStatus {
  hermes: Availability;
  claude_cli: Availability;
  api: Availability;
  notices: string[];
}

export interface Thresholds {
  route: number;
  command_safe: number;
  command_risk: number;
}

export interface Settings {
  thresholds: Thresholds;
  max_commands_per_turn: number;
  approval_timeout_secs: number;
  auto_route: boolean;
  default_provider: ProviderId | null;
  provider_descriptions: Partial<Record<ProviderId, string>>;
  anthropic_model: string;
  anthropic_base_url: string;
  jev_base_url: string;
  claude_model: string | null;
  hermes_base_url: string;
  hermes_allowed_toolsets: string[];
  mcp_port: number;
  run_timeout_secs: number;
}

export type SecretName = "jev" | "anthropic" | "hermes_api" | "mcp_hermes";

export const PROVIDER_LABEL: Record<ProviderId, string> = {
  hermes: "Hermes",
  claude_cli: "Claude CLI",
  api: "API",
};
