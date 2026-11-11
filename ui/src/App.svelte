<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api, errorText, events, type ApprovalAnswer } from "./lib/api";
  import { listenerBag } from "./lib/listeners";
  import type { PendingChoice } from "./lib/state";
  import type { ApprovalRequest, Conversation, ProviderStatus } from "./lib/types";
  import ApprovalHost from "./routes/ApprovalHost.svelte";
  import Chat from "./routes/Chat.svelte";
  import Settings from "./routes/Settings.svelte";
  import Sidebar from "./routes/Sidebar.svelte";

  let conversations: Conversation[] = $state([]);
  let selected: number | null = $state(null);
  let view: "chat" | "settings" = $state("chat");
  let status: ProviderStatus | null = $state(null);
  let approvals: ApprovalRequest[] = $state([]);
  let toast = $state("");
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  /** Provider choices still owed, per conversation (survive switching chats). */
  let choices: Record<number, PendingChoice | null> = $state({});
  const listeners = listenerBag();

  const current = $derived(conversations.find((c) => c.id === selected) ?? null);

  function showError(msg: string) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 6000);
  }

  async function refreshList(selectId?: number | null) {
    try {
      conversations = await api.listConversations();
      if (selectId !== undefined) selected = selectId;
      if (selected !== null && !conversations.some((c) => c.id === selected)) selected = null;
      if (selectId !== undefined && selectId !== null) view = "chat";
    } catch (e) {
      showError(errorText(e));
    }
  }

  async function refreshStatus() {
    try {
      status = await api.providerStatus();
    } catch (e) {
      showError(errorText(e));
    }
  }

  async function answer(id: string, a: ApprovalAnswer) {
    approvals = approvals.filter((r) => r.id !== id);
    try {
      await api.answerApproval(id, a);
    } catch (e) {
      // Usually: the request already timed out or was cancelled.
      showError(errorText(e));
    }
  }

  onMount(async () => {
    void listeners.add(events.onApproval((r) => (approvals = [...approvals, r])));
    void listeners.add(events.onApprovalClosed((id) => (approvals = approvals.filter((r) => r.id !== id))));
    void listeners.add(
      events.onEvent((ev) => {
        // Keep the sidebar order fresh when a turn ends.
        if (ev.kind === "turn_finished" || ev.kind === "turn_failed") refreshList();
      }),
    );
    // Record a choice even when that chat is not open.
    void listeners.add(
      events.onTurnOutcome((o) => {
        if (o.outcome?.kind === "needs_provider_choice") {
          choices[o.conversation_id] = { userMessageId: o.outcome.user_message_id, ranked: o.outcome.ranked };
        }
      }),
    );
    await refreshList();
    if (conversations.length) selected = conversations[0].id;
    await refreshStatus();
  });

  onDestroy(() => listeners.dispose());
</script>

<div class="app">
  <Sidebar
    {conversations}
    {selected}
    {view}
    onselect={(id) => {
      selected = id;
      view = "chat";
    }}
    onchanged={refreshList}
    onsettings={() => {
      view = "settings";
      refreshStatus();
    }}
    onerror={showError}
  />

  <main>
    {#if status?.notices.length && view === "chat"}
      <div class="banner" role="alert">{status.notices.join(" ")}</div>
    {/if}
    {#if view === "settings"}
      <Settings {status} onrefreshstatus={refreshStatus} onerror={showError} />
    {:else if current}
      {#key current.id}
        <Chat
          conversation={current}
          {status}
          choice={choices[current.id] ?? null}
          onchoice={(c) => (choices[current.id] = c)}
          onerror={showError}
        />
      {/key}
    {:else}
      <div class="welcome">
        <h1>เริ่มแชตใหม่</h1>
        <p>กด “แชตใหม่ในโฟลเดอร์…” ทางซ้ายแล้วเลือกโฟลเดอร์ที่ให้ตัวคุยทำงาน</p>
        <p>ถ้ายังไม่ได้ตั้ง key ไปที่หน้า <button type="button" class="link" onclick={() => (view = "settings")}>ตั้งค่า</button></p>
      </div>
    {/if}
  </main>
</div>

<ApprovalHost
  requests={approvals}
  titleFor={(id) => conversations.find((c) => c.id === id)?.title ?? ""}
  onanswer={answer}
/>

{#if toast}
  <div class="toast" role="alert">
    <span>{toast}</span>
    <button type="button" aria-label="ปิด" onclick={() => (toast = "")}>✕</button>
  </div>
{/if}

<style>
  .app {
    display: grid;
    grid-template-columns: 260px 1fr;
    height: 100vh;
  }
  main {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  main > :global(:last-child) {
    flex: 1;
    min-height: 0;
  }
  .banner {
    background: var(--bg-sunk);
    color: var(--danger);
    border-bottom: 1px solid var(--line);
    padding: 8px 20px;
    font-size: 0.84rem;
  }
  .welcome {
    padding: 18vh 40px;
    color: var(--ink-soft);
  }
  .welcome h1 {
    color: var(--ink);
    font-size: 1.3rem;
  }
  .link {
    border: 0;
    background: transparent;
    color: var(--accent);
    padding: 0;
    text-decoration: underline;
  }
  .toast {
    position: fixed;
    bottom: 18px;
    right: 18px;
    max-width: 420px;
    display: flex;
    gap: 10px;
    align-items: flex-start;
    background: var(--ink);
    color: var(--bg);
    padding: 10px 14px;
    border-radius: 8px;
    font-size: 0.86rem;
    z-index: 20;
  }
  .toast button {
    border: 0;
    background: transparent;
    color: inherit;
    padding: 0;
  }
</style>
