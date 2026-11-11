// Chat view state. `applyEvent` is pure: it never mutates its input.

import type {
  AppEvent,
  ApprovalKind,
  CommandRecord,
  DecisionRecord,
  DecisionSource,
  Message,
  ProviderId,
  Role,
} from "./types";

export interface Badge {
  source: DecisionSource;
  probability: number | null;
}

export type ChatItem =
  | {
      type: "message";
      key: string;
      role: Role;
      provider: ProviderId | null;
      content: string;
      streaming: boolean;
      badge: Badge | null;
    }
  | {
      type: "command";
      key: string;
      command_id: number | null;
      command: string;
      cwd: string;
      approval: ApprovalKind;
      output: string;
      running: boolean;
      exit_code: number | null;
      timed_out: boolean;
    }
  | { type: "notice"; key: string; text: string };

export interface ChatState {
  conversationId: number;
  items: ChatItem[];
  streaming: boolean;
  route: { provider: ProviderId; badge: Badge } | null;
  seq: number;
}

export function emptyChat(conversationId: number): ChatState {
  return { conversationId, items: [], streaming: false, route: null, seq: 0 };
}

function key(s: ChatState, prefix: string): [string, number] {
  return [`${prefix}-${s.seq + 1}`, s.seq + 1];
}

/** Add the user's message and mark the chat as working. */
export function pushUser(s: ChatState, text: string): ChatState {
  const [k, seq] = key(s, "u");
  return {
    ...s,
    seq,
    streaming: true,
    route: null,
    items: [
      ...s.items,
      { type: "message", key: k, role: "user", provider: null, content: text, streaming: false, badge: null },
    ],
  };
}

function endStreaming(items: ChatItem[]): ChatItem[] {
  return items.map((i) => (i.type === "message" && i.streaming ? { ...i, streaming: false } : i));
}

function updateCommand(s: ChatState, id: number, f: (c: Extract<ChatItem, { type: "command" }>) => ChatItem): ChatState {
  const idx = s.items.findIndex((i) => i.type === "command" && i.command_id === id);
  if (idx < 0) return s;
  const items = s.items.slice();
  items[idx] = f(items[idx] as Extract<ChatItem, { type: "command" }>);
  return { ...s, items };
}

function chunkText(c: { stdout: string } | { stderr: string }): string {
  return "stdout" in c ? c.stdout : c.stderr;
}

export function applyEvent(s: ChatState, ev: AppEvent): ChatState {
  if ("conversation_id" in ev && ev.conversation_id !== s.conversationId) return s;
  switch (ev.kind) {
    case "route_decided": {
      const badge = { source: ev.source, probability: ev.probability };
      return { ...s, route: { provider: ev.provider, badge } };
    }
    case "assistant_delta": {
      const last = s.items[s.items.length - 1];
      if (last && last.type === "message" && last.role === "assistant" && last.streaming) {
        const items = s.items.slice(0, -1);
        items.push({ ...last, content: last.content + ev.text });
        return { ...s, items, streaming: true };
      }
      const [k, seq] = key(s, "a");
      return {
        ...s,
        seq,
        streaming: true,
        items: [
          ...endStreaming(s.items),
          {
            type: "message",
            key: k,
            role: "assistant",
            provider: s.route?.provider ?? null,
            content: ev.text,
            streaming: true,
            badge: s.route?.badge ?? null,
          },
        ],
      };
    }
    case "command_started": {
      const [k, seq] = key(s, "c");
      return {
        ...s,
        seq,
        items: [
          ...endStreaming(s.items),
          {
            type: "command",
            key: k,
            command_id: ev.command_id,
            command: ev.command,
            cwd: ev.cwd,
            approval: ev.approval,
            output: "",
            running: true,
            exit_code: null,
            timed_out: false,
          },
        ],
      };
    }
    case "command_output":
      return updateCommand(s, ev.command_id, (c) => ({ ...c, output: c.output + chunkText(ev.chunk) }));
    case "command_finished":
      return updateCommand(s, ev.command_id, (c) => ({
        ...c,
        running: false,
        exit_code: ev.exit_code,
        timed_out: ev.timed_out,
      }));
    case "command_denied": {
      const [k, seq] = key(s, "c");
      return {
        ...s,
        seq,
        items: [
          ...endStreaming(s.items),
          {
            type: "command",
            key: k,
            command_id: null,
            command: ev.command,
            cwd: "",
            approval: "denied",
            output: "",
            running: false,
            exit_code: null,
            timed_out: false,
          },
        ],
      };
    }
    case "turn_finished":
      return { ...s, streaming: false, items: endStreaming(s.items) };
    case "turn_failed": {
      const [k, seq] = key(s, "n");
      return {
        ...s,
        seq,
        streaming: false,
        items: [...endStreaming(s.items), { type: "notice", key: k, text: ev.error }],
      };
    }
  }
  return s;
}

/** Shape of a stored Route decision payload. */
interface RoutePayload {
  outcome?: { auto?: ProviderId; ask_user?: { ranked: [ProviderId, number][] } };
  source?: DecisionSource;
  probability?: number | null;
}

const ALL_PROVIDERS: ProviderId[] = ["hermes", "claude_cli", "api"];

export interface PendingChoice {
  userMessageId: number;
  ranked: [ProviderId, number][];
}

/**
 * A provider choice still owed for this conversation: the last stored
 * message is the user's and no turn is running. Uses Jev's stored ranking
 * when there is one, otherwise offers every provider.
 */
export function pendingChoice(messages: Message[], decisions: DecisionRecord[], busy: boolean): PendingChoice | null {
  if (busy) return null;
  const last = messages[messages.length - 1];
  if (!last || last.role !== "user") return null;
  let ranked: [ProviderId, number][] | null = null;
  for (const d of decisions) {
    if (d.kind !== "route" || d.message_id !== last.id) continue;
    const r = (d.payload as RoutePayload | null)?.outcome?.ask_user?.ranked;
    if (r) ranked = r;
  }
  return { userMessageId: last.id, ranked: ranked ?? ALL_PROVIDERS.map((p) => [p, 0]) };
}

/** Remove one item (e.g. an optimistic message whose send failed). */
export function dropItem(s: ChatState, key: string): ChatState {
  return { ...s, streaming: false, items: endStreaming(s.items.filter((i) => i.key !== key)) };
}

/** Add a local notice (e.g. a command error). */
export function pushNotice(s: ChatState, text: string): ChatState {
  const [k, seq] = key(s, "n");
  return { ...s, seq, items: [...endStreaming(s.items), { type: "notice", key: k, text }] };
}

/** End the working state without an event (e.g. needs a provider choice). */
export function stopWorking(s: ChatState): ChatState {
  return { ...s, streaming: false, items: endStreaming(s.items) };
}

/**
 * Build state from stored data. Command results are shown from the command
 * records (matched in order), so `command_result` messages are skipped.
 */
export function fromStored(
  conversationId: number,
  messages: Message[],
  commands: CommandRecord[],
  decisions: DecisionRecord[] = [],
): ChatState {
  type Timed = { at: string; order: number; item: ChatItem };
  const rows: Timed[] = [];
  let order = 0;
  // Route decision per user message: the replies after it get its badge.
  const routeBadge = new Map<number, Badge>();
  for (const d of decisions) {
    const p = d.kind === "route" ? (d.payload as RoutePayload | null) : null;
    if (p?.source) routeBadge.set(d.message_id, { source: p.source, probability: p.probability ?? null });
  }
  let badge: Badge | null = null;
  for (const m of messages) {
    if (m.role === "user") badge = routeBadge.get(m.id) ?? null;
    if (m.role === "command_result") continue;
    const item: ChatItem =
      m.role === "notice"
        ? { type: "notice", key: `m${m.id}`, text: m.content }
        : {
            type: "message",
            key: `m${m.id}`,
            role: m.role,
            provider: m.provider,
            content: m.content,
            streaming: false,
            badge: m.role === "assistant" ? badge : null,
          };
    rows.push({ at: m.created_at, order: order++, item });
  }
  for (const c of commands) {
    rows.push({
      at: c.started_at,
      order: order++,
      item: {
        type: "command",
        key: `c${c.id}`,
        command_id: c.id,
        command: c.command,
        cwd: c.cwd,
        approval: c.approval,
        output: c.output,
        running: false,
        exit_code: c.exit_code,
        timed_out: false,
      },
    });
  }
  rows.sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : a.order - b.order));
  return { conversationId, items: rows.map((r) => r.item), streaming: false, route: null, seq: 0 };
}
