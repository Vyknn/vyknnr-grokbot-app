<script lang="ts">
  import type { ProviderPick } from "../lib/api";
  import { unavailableReason } from "../lib/format";
  import type { ProviderId, ProviderStatus } from "../lib/types";

  let {
    value = $bindable("auto"),
    status,
  }: { value: ProviderPick; status: ProviderStatus | null } = $props();

  const options: { id: ProviderPick; label: string }[] = [
    { id: "auto", label: "Auto" },
    { id: "hermes", label: "Hermes" },
    { id: "claude_cli", label: "Claude CLI" },
    { id: "api", label: "API" },
  ];

  function reason(id: ProviderPick): string | null {
    if (id === "auto") return null;
    return unavailableReason(status?.[id as ProviderId]);
  }
</script>

<div class="picker" role="radiogroup" aria-label="เลือกตัวคุย">
  {#each options as o (o.id)}
    {@const why = reason(o.id)}
    <button
      type="button"
      role="radio"
      aria-checked={value === o.id}
      class:on={value === o.id}
      disabled={why !== null && status !== null}
      title={why ?? (o.id === "auto" ? "ให้ Jev เลือกตัวคุยที่เหมาะ" : "พร้อมใช้งาน")}
      onclick={() => (value = o.id)}
    >
      <span class="dot" class:off={why !== null}></span>{o.label}
    </button>
  {/each}
</div>

<style>
  .picker {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg-sunk);
  }
  button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: transparent;
    padding: 4px 10px;
    border-radius: 6px;
    color: var(--ink-soft);
    font-size: 0.82rem;
  }
  button.on {
    background: var(--bg-raised);
    color: var(--ink);
    box-shadow: 0 0 0 1px var(--line);
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ok);
  }
  .dot.off {
    background: var(--ink-faint);
  }
</style>
