<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { api, errorText, events, type ProviderPick } from "../lib/api";
  import { APPROVAL_TEXT, pct, sourceText } from "../lib/format";
  import {
    applyEvent,
    emptyChat,
    fromStored,
    pushNotice,
    pushUser,
    stopWorking,
    type ChatState,
  } from "../lib/state";
  import {
    PROVIDER_LABEL,
    type Conversation,
    type ProviderId,
    type ProviderStatus,
    type TurnOutcomeEvent,
  } from "../lib/types";
  import ProviderPicker from "./ProviderPicker.svelte";

  let {
    conversation,
    status,
    onerror,
  }: {
    conversation: Conversation;
    status: ProviderStatus | null;
    onerror: (msg: string) => void;
  } = $props();

  let chat: ChatState = $state(emptyChat(0));
  let draft = $state("");
  let provider: ProviderPick = $state("auto");
  let choice: { userMessageId: number; ranked: [ProviderId, number][] } | null = $state(null);
  let scroller: HTMLElement | undefined = $state();
  let exporting = $state(false);
  let unlisten: (() => void)[] = [];

  async function load(id: number) {
    choice = null;
    try {
      const d = await api.getConversationDetail(id);
      if (conversation.id !== id) return;
      chat = fromStored(id, d.messages, d.commands, d.decisions);
      if (d.busy) chat = { ...chat, streaming: true };
      await scrollDown();
    } catch (e) {
      onerror(errorText(e));
    }
  }

  // Only the id matters; the sidebar refresh hands us new objects.
  const convId = $derived(conversation.id);
  $effect(() => {
    void load(convId);
  });

  async function scrollDown() {
    await tick();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  }

  function nearBottom(): boolean {
    return !scroller || scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 120;
  }

  async function onOutcome(o: TurnOutcomeEvent) {
    if (o.conversation_id !== conversation.id) return;
    if (o.error) {
      chat = pushNotice(stopWorking(chat), o.error);
      return;
    }
    const out = o.outcome;
    if (!out) return;
    if (out.kind === "needs_provider_choice") {
      chat = stopWorking(chat);
      choice = { userMessageId: out.user_message_id, ranked: out.ranked };
      await scrollDown();
    } else if (out.kind === "completed" || out.kind === "cancelled") {
      await load(conversation.id);
    } else {
      // TurnFailed already added a notice and cleared streaming.
      chat = stopWorking(chat);
    }
  }

  onMount(async () => {
    unlisten.push(
      await events.onEvent(async (ev) => {
        const stick = nearBottom();
        chat = applyEvent(chat, ev);
        if (stick) await scrollDown();
      }),
    );
    unlisten.push(await events.onTurnOutcome(onOutcome));
  });

  onDestroy(() => unlisten.forEach((u) => u()));

  async function send() {
    const text = draft.trim();
    if (!text || chat.streaming) return;
    choice = null;
    chat = pushUser(chat, text);
    draft = "";
    await scrollDown();
    try {
      await api.sendMessage(conversation.id, text, provider);
    } catch (e) {
      chat = pushNotice(stopWorking(chat), errorText(e));
    }
  }

  async function pick(p: ProviderId) {
    if (!choice) return;
    const id = choice.userMessageId;
    choice = null;
    chat = { ...chat, streaming: true };
    try {
      await api.continueWith(conversation.id, id, p);
    } catch (e) {
      chat = pushNotice(stopWorking(chat), errorText(e));
    }
  }

  async function stop() {
    try {
      await api.cancelTurn(conversation.id);
    } catch (e) {
      onerror(errorText(e));
    }
  }

  async function exportAs(format: "md" | "json") {
    exporting = false;
    try {
      const path = await api.exportConversation(conversation.id, format);
      if (path) chat = pushNotice(chat, `บันทึกแล้ว: ${path}`);
    } catch (e) {
      onerror(errorText(e));
    }
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send();
    }
  }

  function providerReady(p: ProviderId): boolean {
    return status?.[p] === "ready";
  }
</script>

<section class="chat">
  <header>
    <div class="head">
      <h1>{conversation.title}</h1>
      <span class="cwd" title={conversation.cwd}>{conversation.cwd}</span>
    </div>
    <div class="tools">
      <div class="export">
        <button type="button" onclick={() => (exporting = !exporting)} aria-expanded={exporting}>Export</button>
        {#if exporting}
          <div class="menu">
            <button type="button" onclick={() => exportAs("md")}>Markdown (.md)</button>
            <button type="button" onclick={() => exportAs("json")}>JSON (.json)</button>
          </div>
        {/if}
      </div>
    </div>
  </header>

  <div class="log" bind:this={scroller}>
    {#each chat.items as item (item.key)}
      {#if item.type === "message"}
        <article class="msg {item.role}">
          {#if item.role === "assistant"}
            <div class="meta">
              <span class="who">{item.provider ? PROVIDER_LABEL[item.provider] : "ตัวคุย"}</span>
              {#if item.badge}
                <span class="badge" title="ใครเลือกตัวคุยนี้">{sourceText(item.badge.source, item.badge.probability)}</span>
              {/if}
            </div>
          {/if}
          <div class="body">{item.content}{#if item.streaming}<span class="caret" aria-hidden="true"></span>{/if}</div>
        </article>
      {:else if item.type === "command"}
        <article class="cmd" class:denied={item.approval === "denied"}>
          <div class="cmd-head">
            <code>&gt; {item.command}</code>
            <span class="tag">{APPROVAL_TEXT[item.approval]}</span>
          </div>
          {#if item.output}
            <pre>{item.output}</pre>
          {/if}
          <div class="cmd-foot">
            {#if item.running}
              <span class="running">กำลังรัน…</span>
            {:else if item.approval !== "denied"}
              <span class:bad={item.exit_code !== 0}>
                {item.timed_out ? "หมดเวลา" : `exit ${item.exit_code ?? "?"}`}
              </span>
            {/if}
            {#if item.cwd}<span class="cwd">{item.cwd}</span>{/if}
          </div>
        </article>
      {:else}
        <div class="notice" role="status">{item.text}</div>
      {/if}
    {:else}
      <p class="empty">พิมพ์ข้อความเพื่อเริ่มคุย คำสั่งที่ตัวคุยเสนอจะรันในโฟลเดอร์ของแชตนี้</p>
    {/each}

    {#if choice}
      <div class="choice">
        <p>Jev ไม่มั่นใจว่าตัวคุยไหนเหมาะ เลือกเอง:</p>
        <div class="ranked">
          {#each choice.ranked as [p, prob] (p)}
            <button type="button" disabled={!providerReady(p)} onclick={() => pick(p)}>
              {PROVIDER_LABEL[p]}
              {#if prob > 0}<small>{pct(prob)}</small>{/if}
            </button>
          {/each}
        </div>
      </div>
    {/if}
  </div>

  <footer>
    <div class="row">
      <ProviderPicker bind:value={provider} {status} />
      {#if chat.streaming}
        <span class="working">กำลังทำงาน…</span>
      {/if}
    </div>
    <div class="compose">
      <textarea
        bind:value={draft}
        onkeydown={onkey}
        rows="3"
        placeholder="พิมพ์ข้อความ (Enter ส่ง, Shift+Enter ขึ้นบรรทัดใหม่)"
        aria-label="ข้อความ"
      ></textarea>
      {#if chat.streaming}
        <button type="button" class="stop" onclick={stop}>หยุด</button>
      {:else}
        <button type="button" class="send" disabled={!draft.trim()} onclick={send}>ส่ง</button>
      {/if}
    </div>
  </footer>
</section>

<style>
  .chat {
    display: grid;
    grid-template-rows: auto 1fr auto;
    min-height: 0;
    height: 100%;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 12px 20px;
    border-bottom: 1px solid var(--line);
  }
  .head {
    min-width: 0;
  }
  h1 {
    font-size: 1rem;
    margin: 0;
  }
  .cwd {
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--ink-faint);
  }
  .export {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    display: flex;
    flex-direction: column;
    background: var(--bg-raised);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 4px;
    z-index: 5;
    min-width: 160px;
  }
  .menu button {
    border: 0;
    text-align: left;
    background: transparent;
  }
  .log {
    overflow-y: auto;
    padding: 18px 20px 24px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .msg {
    max-width: 78ch;
  }
  .msg.user {
    align-self: flex-end;
    background: var(--user-bg);
    border-radius: 12px 12px 2px 12px;
    padding: 8px 12px;
  }
  .msg.assistant {
    border-left: 2px solid var(--accent);
    padding: 2px 0 2px 12px;
  }
  .body {
    white-space: pre-wrap;
    word-wrap: break-word;
    line-height: 1.6;
  }
  .meta {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-bottom: 2px;
    font-size: 0.74rem;
  }
  .who {
    font-weight: 700;
    color: var(--accent);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .badge {
    color: var(--ink-soft);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 0 8px;
  }
  .caret {
    display: inline-block;
    width: 7px;
    height: 1em;
    vertical-align: text-bottom;
    background: var(--accent);
    margin-left: 2px;
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0;
    }
  }
  .cmd {
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg-sunk);
    font-size: 0.84rem;
    max-width: 100ch;
  }
  .cmd.denied {
    border-style: dashed;
    opacity: 0.8;
  }
  .cmd-head,
  .cmd-foot {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    padding: 6px 10px;
    align-items: center;
  }
  .cmd-head code {
    font-family: var(--mono);
    word-break: break-all;
  }
  .tag {
    font-size: 0.72rem;
    color: var(--ink-soft);
    white-space: nowrap;
  }
  .cmd.denied .tag {
    color: var(--danger);
  }
  .cmd pre {
    margin: 0;
    padding: 8px 10px;
    border-top: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
    font-family: var(--mono);
    font-size: 0.8rem;
    max-height: 320px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }
  .cmd-foot {
    font-size: 0.74rem;
    color: var(--ink-soft);
  }
  .bad {
    color: var(--danger);
  }
  .running {
    color: var(--warn);
  }
  .notice {
    align-self: center;
    font-size: 0.82rem;
    color: var(--warn);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 3px 12px;
    max-width: 90%;
  }
  .empty {
    color: var(--ink-faint);
    text-align: center;
    margin-top: 18vh;
  }
  .choice {
    border: 1px solid var(--accent);
    border-radius: 10px;
    padding: 10px 14px;
    background: var(--bg-raised);
  }
  .choice p {
    margin: 0 0 8px;
  }
  .ranked {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .ranked small {
    color: var(--ink-soft);
    margin-left: 4px;
  }
  footer {
    border-top: 1px solid var(--line);
    padding: 10px 20px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .working {
    font-size: 0.8rem;
    color: var(--ink-soft);
  }
  .compose {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  .compose textarea {
    flex: 1;
    resize: vertical;
    min-height: 3.2em;
  }
  .send {
    background: var(--accent);
    color: var(--on-accent);
    border-color: var(--accent);
    min-width: 72px;
  }
  .stop {
    background: var(--danger);
    color: #fff;
    border-color: var(--danger);
    min-width: 72px;
  }
</style>
