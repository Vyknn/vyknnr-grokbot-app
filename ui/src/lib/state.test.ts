import { describe, expect, test } from "vitest";
import { applyEvent, emptyChat, fromStored, pushUser, type ChatState } from "./state";
import type { AppEvent } from "./types";

const C = 1;
const start = (): ChatState => pushUser(emptyChat(C), "สวัสดี");

function run(s: ChatState, evs: AppEvent[]): ChatState {
  return evs.reduce(applyEvent, s);
}

describe("applyEvent", () => {
  test("deltas append to the streaming assistant message", () => {
    const s = run(start(), [
      { kind: "assistant_delta", conversation_id: C, text: "สวัส" },
      { kind: "assistant_delta", conversation_id: C, text: "ดีครับ" },
    ]);
    const last = s.items[s.items.length - 1];
    expect(last.type).toBe("message");
    if (last.type !== "message") throw new Error();
    expect(last.role).toBe("assistant");
    expect(last.content).toBe("สวัสดีครับ");
    expect(last.streaming).toBe(true);
    expect(s.items.filter((i) => i.type === "message" && i.role === "assistant")).toHaveLength(1);
  });

  test("CommandOutput attaches to its command block", () => {
    const s = run(start(), [
      { kind: "command_started", conversation_id: C, command_id: 5, command: "dir", cwd: "C:\\x", approval: "auto" },
      { kind: "command_started", conversation_id: C, command_id: 6, command: "ls", cwd: "C:\\x", approval: "user" },
      { kind: "command_output", command_id: 5, chunk: { stdout: "a\n" } },
      { kind: "command_output", command_id: 6, chunk: { stderr: "oops" } },
      { kind: "command_output", command_id: 5, chunk: { stdout: "b\n" } },
      { kind: "command_output", command_id: 99, chunk: { stdout: "stray" } },
    ]);
    const cmds = s.items.filter((i) => i.type === "command");
    expect(cmds).toHaveLength(2);
    const [c5, c6] = cmds;
    if (c5.type !== "command" || c6.type !== "command") throw new Error();
    expect(c5.output).toBe("a\nb\n");
    expect(c6.output).toBe("oops");
    expect(JSON.stringify(s)).not.toContain("stray");
  });

  test("TurnFailed shows notice and clears streaming flag", () => {
    const s = run(start(), [
      { kind: "assistant_delta", conversation_id: C, text: "ครึ่ง" },
      { kind: "turn_failed", conversation_id: C, provider: "hermes", error: "Hermes ไม่ได้เปิด API server" },
    ]);
    expect(s.streaming).toBe(false);
    const notice = s.items[s.items.length - 1];
    expect(notice.type).toBe("notice");
    if (notice.type !== "notice") throw new Error();
    expect(notice.text).toContain("Hermes ไม่ได้เปิด API server");
    for (const i of s.items) if (i.type === "message") expect(i.streaming).toBe(false);
  });

  test("RouteDecided sets badge", () => {
    const s = run(start(), [
      {
        kind: "route_decided",
        conversation_id: C,
        provider: "claude_cli",
        source: { kind: "jev", model: "jev-1" },
        probability: 0.82,
      },
      { kind: "assistant_delta", conversation_id: C, text: "ok" },
    ]);
    expect(s.route?.provider).toBe("claude_cli");
    const last = s.items[s.items.length - 1];
    if (last.type !== "message") throw new Error();
    expect(last.provider).toBe("claude_cli");
    expect(last.badge?.probability).toBe(0.82);
    expect(last.badge?.source.kind).toBe("jev");
  });

  test("CommandStarted/Finished lifecycle", () => {
    let s = run(start(), [
      { kind: "command_started", conversation_id: C, command_id: 3, command: "Get-ChildItem", cwd: "C:\\p", approval: "user_edited" },
    ]);
    let c = s.items.find((i) => i.type === "command");
    if (!c || c.type !== "command") throw new Error();
    expect(c.running).toBe(true);
    expect(c.approval).toBe("user_edited");
    s = applyEvent(s, { kind: "command_finished", command_id: 3, exit_code: 1, timed_out: false });
    c = s.items.find((i) => i.type === "command");
    if (!c || c.type !== "command") throw new Error();
    expect(c.running).toBe(false);
    expect(c.exit_code).toBe(1);
    expect(c.timed_out).toBe(false);
  });

  test("CommandDenied adds a denied block", () => {
    const s = applyEvent(start(), { kind: "command_denied", conversation_id: C, command: "rm -r x" });
    const c = s.items[s.items.length - 1];
    if (c.type !== "command") throw new Error();
    expect(c.approval).toBe("denied");
    expect(c.running).toBe(false);
  });

  test("TurnFinished clears streaming", () => {
    const s = run(start(), [
      { kind: "assistant_delta", conversation_id: C, text: "x" },
      { kind: "turn_finished", conversation_id: C },
    ]);
    expect(s.streaming).toBe(false);
    for (const i of s.items) if (i.type === "message") expect(i.streaming).toBe(false);
  });

  test("events for other conversations are ignored and input is not mutated", () => {
    const s0 = start();
    const snapshot = JSON.stringify(s0);
    const s1 = applyEvent(s0, { kind: "assistant_delta", conversation_id: 2, text: "nope" });
    expect(s1).toBe(s0);
    applyEvent(s0, { kind: "assistant_delta", conversation_id: C, text: "yes" });
    expect(JSON.stringify(s0)).toBe(snapshot);
  });

  test("a delta after a command block starts a new assistant message", () => {
    const s = run(start(), [
      { kind: "assistant_delta", conversation_id: C, text: "ก่อน" },
      { kind: "command_started", conversation_id: C, command_id: 1, command: "dir", cwd: "C:\\", approval: "auto" },
      { kind: "assistant_delta", conversation_id: C, text: "หลัง" },
    ]);
    const msgs = s.items.filter((i) => i.type === "message" && i.role === "assistant");
    expect(msgs.map((m) => (m.type === "message" ? m.content : ""))).toEqual(["ก่อน", "หลัง"]);
  });
});

describe("fromStored", () => {
  test("orders by time, skips command_result text, badges replies from route decisions", () => {
    const t = (n: number) => `2026-11-11T00:00:0${n}.000Z`;
    const s = fromStored(
      C,
      [
        { id: 1, conversation_id: C, role: "user", provider: null, content: "ls", created_at: t(1) },
        { id: 2, conversation_id: C, role: "command_result", provider: "api", content: "[raw]", created_at: t(3) },
        { id: 3, conversation_id: C, role: "assistant", provider: "api", content: "done", created_at: t(4) },
      ],
      [
        {
          id: 9, conversation_id: C, command: "dir", cwd: "C:\\", check: null, approval: "auto",
          exit_code: 0, output: "x", truncated: false, started_at: t(2), finished_at: t(3),
        },
      ],
      [{ id: 1, message_id: 1, kind: "route", payload: { outcome: { auto: "api" }, source: { kind: "jev", model: "m" } }, created_at: t(1) }],
    );
    expect(s.items.map((i) => i.type)).toEqual(["message", "command", "message"]);
    const last = s.items[2];
    if (last.type !== "message") throw new Error();
    expect(last.badge?.source.kind).toBe("jev");
    expect(JSON.stringify(s)).not.toContain("[raw]");
  });
});
