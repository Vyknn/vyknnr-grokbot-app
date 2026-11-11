// @vitest-environment jsdom
import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, test, vi } from "vitest";
import type { Settings as SettingsT } from "../lib/types";

const SETTINGS: SettingsT = {
  thresholds: { route: 0.75, command_safe: 0.9, command_risk: 0.9 },
  max_commands_per_turn: 20,
  approval_timeout_secs: 300,
  auto_route: true,
  default_provider: null,
  provider_descriptions: { hermes: "h", claude_cli: "c", api: "a" },
  anthropic_model: "claude-opus-5-5",
  anthropic_base_url: "https://api.anthropic.com",
  jev_base_url: "https://api.typesafe.ai",
  claude_model: null,
  hermes_base_url: "http://127.0.0.1:8642/v1",
  hermes_allowed_toolsets: ["web"],
  mcp_port: 8765,
  run_timeout_secs: 120,
};

const REJECT = "hermes_allowed_toolsets ห้ามมี toolset ที่รันคำสั่งหรือแตะไฟล์เอง: terminal";

const saveSettings = vi.fn(async (_s: SettingsT) => {
  throw REJECT;
});

vi.mock("../lib/api", () => ({
  api: {
    getSettings: async () => structuredClone(SETTINGS),
    hasSecret: async () => false,
    saveSettings: (s: SettingsT) => saveSettings(s),
  },
  errorText: (e: unknown) => (typeof e === "string" ? e : String(e)),
}));

const { default: Settings } = await import("./Settings.svelte");

describe("settings", () => {
  test("shows the validation error from save next to the save button", async () => {
    const errors: string[] = [];
    render(Settings, {
      props: { status: null, onrefreshstatus: () => {}, onerror: (m: string) => errors.push(m) },
    });
    const box = await screen.findByLabelText(/toolset ของ Hermes/);
    await fireEvent.input(box, { target: { value: "web, terminal" } });
    await fireEvent.click(screen.getByRole("button", { name: "บันทึกการตั้งค่า" }));
    const alert = await screen.findByText(REJECT);
    expect(alert.getAttribute("role")).toBe("alert");
    expect(saveSettings).toHaveBeenCalledOnce();
    expect(saveSettings.mock.calls[0][0].hermes_allowed_toolsets).toEqual(["web", "terminal"]);
    expect(screen.queryByText("บันทึกแล้ว")).toBeNull();
  });
});
