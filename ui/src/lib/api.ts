// Typed wrappers around the Tauri commands and events.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppEvent,
  ApprovalRequest,
  CommandRecord,
  Conversation,
  DecisionRecord,
  Message,
  ProviderId,
  ProviderStatus,
  SecretName,
  Settings,
  TurnOutcomeEvent,
} from "./types";

export type ProviderPick = "auto" | ProviderId;
export type ApprovalAnswer = "run" | "deny" | { edited: string };

export const api = {
  listConversations: () => invoke<Conversation[]>("list_conversations"),
  searchConversations: (q: string) => invoke<Conversation[]>("search_conversations", { q }),
  createConversation: (title: string, cwd: string) =>
    invoke<Conversation>("create_conversation", { title, cwd }),
  renameConversation: (id: number, title: string) => invoke<void>("rename_conversation", { id, title }),
  deleteConversation: (id: number) => invoke<void>("delete_conversation", { id }),
  getMessages: (id: number) => invoke<Message[]>("get_messages", { id }),
  getConversationDetail: (id: number) =>
    invoke<{ messages: Message[]; commands: CommandRecord[]; decisions: DecisionRecord[]; busy: boolean }>("get_conversation_detail", { id }),
  sendMessage: (conversationId: number, text: string, provider: ProviderPick) =>
    invoke<void>("send_message", {
      conversationId,
      text,
      provider: provider === "auto" ? null : provider,
    }),
  continueWith: (conversationId: number, userMessageId: number, provider: ProviderId) =>
    invoke<void>("continue_with", { conversationId, userMessageId, provider }),
  cancelTurn: (conversationId: number) => invoke<void>("cancel_turn", { conversationId }),
  answerApproval: (id: string, response: ApprovalAnswer) => invoke<void>("answer_approval", { id, response }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  setSecret: (key: SecretName, value: string) => invoke<void>("set_secret", { key, value }),
  hasSecret: (key: SecretName) => invoke<boolean>("has_secret", { key }),
  providerStatus: () => invoke<ProviderStatus>("provider_status"),
  exportConversation: (id: number, format: "md" | "json") =>
    invoke<string | null>("export_conversation", { id, format }),
  hermesSetupSnippet: () => invoke<string>("hermes_setup_snippet"),
  startupNotices: () => invoke<string[]>("startup_notices"),
};

export const events = {
  onEvent: (f: (e: AppEvent) => void): Promise<UnlistenFn> => listen<AppEvent>("app://event", (e) => f(e.payload)),
  onApproval: (f: (r: ApprovalRequest) => void): Promise<UnlistenFn> =>
    listen<ApprovalRequest>("app://approval", (e) => f(e.payload)),
  onApprovalClosed: (f: (id: string) => void): Promise<UnlistenFn> =>
    listen<{ id: string }>("app://approval-closed", (e) => f(e.payload.id)),
  onTurnOutcome: (f: (o: TurnOutcomeEvent) => void): Promise<UnlistenFn> =>
    listen<TurnOutcomeEvent>("app://turn-outcome", (e) => f(e.payload)),
};

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
