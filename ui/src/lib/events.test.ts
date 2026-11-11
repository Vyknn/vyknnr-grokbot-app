import { describe, expect, test, vi } from "vitest";
import { listenerBag } from "./listeners";

describe("listenerBag", () => {
  test("unlistens everything on dispose", async () => {
    const bag = listenerBag();
    const u = vi.fn();
    await bag.add(Promise.resolve(u));
    bag.dispose();
    expect(u).toHaveBeenCalledTimes(1);
  });

  test("a listener that resolves after dispose is unlistened at once", async () => {
    const bag = listenerBag();
    let resolve!: (f: () => void) => void;
    const p = new Promise<() => void>((r) => (resolve = r));
    const added = bag.add(p);
    bag.dispose();
    const u = vi.fn();
    resolve(u);
    await added;
    expect(u).toHaveBeenCalledTimes(1);
    bag.dispose();
    expect(u).toHaveBeenCalledTimes(1);
  });
});
