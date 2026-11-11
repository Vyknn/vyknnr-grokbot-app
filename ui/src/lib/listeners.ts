// Tauri `listen` resolves asynchronously; a component may be destroyed first.

export interface ListenerBag {
  add(p: Promise<() => void>): Promise<void>;
  dispose(): void;
}

export function listenerBag(): ListenerBag {
  let disposed = false;
  const fns: (() => void)[] = [];
  return {
    async add(p) {
      const un = await p;
      if (disposed) un();
      else fns.push(un);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const f of fns.splice(0)) f();
    },
  };
}
