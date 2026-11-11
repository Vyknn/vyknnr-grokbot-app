// @vitest-environment jsdom
import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from "vitest";
import type { ApprovalRequest } from "../lib/types";
import ApprovalHost from "./ApprovalHost.svelte";

beforeAll(() => {
  // jsdom has no modal dialog support.
  HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function (this: HTMLDialogElement) {
    this.removeAttribute("open");
    this.dispatchEvent(new Event("close"));
  };
});

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function req(id: string, command: string): ApprovalRequest {
  return {
    id,
    conversation_id: 1,
    command,
    cwd: "C:\work",
    check: { verdict: "ask_user", is_safe: 0.4, risk: "destructive", risk_p: 0.8, source: { kind: "manual" } },
    reason: { kind: "needs_review" },
  };
}

async function settle() {
  await vi.advanceTimersByTimeAsync(0);
}

describe("approval dialog", () => {
  test("buttons are disabled for 500 ms after a request appears", async () => {
    const answers: [string, unknown][] = [];
    render(ApprovalHost, {
      props: { requests: [req("a", "Remove-Item a")], titleFor: () => "", onanswer: (id, a) => answers.push([id, a]) },
    });
    await settle();
    const run = screen.getByRole("button", { name: "รัน" }) as HTMLButtonElement;
    expect(run.disabled).toBe(true);
    await fireEvent.click(run);
    expect(answers).toEqual([]);
    await vi.advanceTimersByTimeAsync(500);
    expect(run.disabled).toBe(false);
    await fireEvent.click(run);
    expect(answers).toEqual([["a", "run"]]);
  });

  test("a second click within 500 ms does not answer the next queued request", async () => {
    const answers: [string, unknown][] = [];
    let requests = [req("a", "Remove-Item a"), req("b", "Remove-Item b")];
    const onanswer = (id: string, a: unknown) => {
      answers.push([id, a]);
      requests = requests.filter((q) => q.id !== id);
      void view.rerender({ requests, titleFor: () => "", onanswer });
    };
    const view = render(ApprovalHost, { props: { requests, titleFor: () => "", onanswer } });
    await vi.advanceTimersByTimeAsync(600);
    await fireEvent.click(screen.getByRole("button", { name: "รัน" }));
    await settle();
    // Request B is now shown; a quick second click must not answer it.
    expect(screen.getByText("Remove-Item b")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "รัน" }));
    await vi.advanceTimersByTimeAsync(100);
    await fireEvent.click(screen.getByRole("button", { name: "รัน" }));
    expect(answers).toEqual([["a", "run"]]);
    await vi.advanceTimersByTimeAsync(500);
    await fireEvent.click(screen.getByRole("button", { name: "รัน" }));
    expect(answers).toEqual([
      ["a", "run"],
      ["b", "run"],
    ]);
  });

  test("Esc denies immediately and exactly once", async () => {
    const answers: [string, unknown][] = [];
    render(ApprovalHost, {
      props: { requests: [req("a", "del")], titleFor: () => "", onanswer: (id, a) => answers.push([id, a]) },
    });
    await settle();
    const dialog = document.querySelector("dialog")!;
    // Esc fires `cancel`, then the dialog closes and fires `close`.
    await fireEvent(dialog, new Event("cancel", { cancelable: true }));
    dialog.close();
    await settle();
    expect(answers).toEqual([["a", "deny"]]);
  });

  test("an empty edit cannot be sent", async () => {
    const answers: [string, unknown][] = [];
    render(ApprovalHost, {
      props: { requests: [req("a", "del x")], titleFor: () => "", onanswer: (id, a) => answers.push([id, a]) },
    });
    await vi.advanceTimersByTimeAsync(600);
    await fireEvent.click(screen.getByRole("button", { name: "แก้ไข" }));
    const box = screen.getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(box, { target: { value: "   " } });
    const send = screen.getByRole("button", { name: "รันคำสั่งที่แก้" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    await fireEvent.click(send);
    expect(answers).toEqual([]);
    await fireEvent.input(box, { target: { value: "dir" } });
    await fireEvent.click(send);
    expect(answers).toEqual([["a", { edited: "dir" }]]);
  });
});
