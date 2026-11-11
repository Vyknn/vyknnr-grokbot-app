<script lang="ts">
  import type { ApprovalAnswer } from "../lib/api";
  import { pct, RISK_TEXT, sourceText } from "../lib/format";
  import type { ApprovalRequest } from "../lib/types";

  let {
    request,
    conversationTitle,
    onanswer,
  }: {
    request: ApprovalRequest;
    conversationTitle: string;
    onanswer: (id: string, a: ApprovalAnswer) => void;
  } = $props();

  let editing = $state(false);
  let edited = $state("");
  let dialog: HTMLDialogElement | undefined = $state();
  let answered = false;

  $effect(() => {
    // New request: reset and open.
    void request.id;
    editing = false;
    edited = request.command;
    answered = false;
    if (dialog && !dialog.open) dialog.showModal();
  });

  function answer(a: ApprovalAnswer) {
    if (answered) return;
    answered = true;
    onanswer(request.id, a);
  }

  // Esc and any other close are a Deny.
  function onclose() {
    answer("deny");
  }

  function oncancel(e: Event) {
    e.preventDefault();
    answer("deny");
  }

  const c = $derived(request.check);
  const risky = $derived(c.risk !== null && c.risk !== "read_only");
</script>

<dialog bind:this={dialog} {onclose} {oncancel} aria-labelledby="ap-title">
  <header>
    <h2 id="ap-title">ขออนุญาตรันคำสั่ง</h2>
    <span class="conv">{conversationTitle}</span>
  </header>

  {#if request.reason.kind === "limit_reached"}
    <p class="reason warn">รันคำสั่งในรอบนี้ไปแล้ว {request.reason.count - 1} คำสั่ง ครบจำนวนที่ตั้งไว้ ต้องถามก่อนรันต่อ</p>
  {:else}
    <p class="reason">Jev ไม่มั่นใจพอที่จะให้รันเอง หรือใช้ Jev ไม่ได้</p>
  {/if}

  {#if editing}
    <label class="lbl" for="ap-edit">แก้คำสั่ง (กดรันแล้วถือว่าคุณอนุมัติ)</label>
    <textarea id="ap-edit" bind:value={edited} rows="4" spellcheck="false"></textarea>
  {:else}
    <pre class="cmd">{request.command}</pre>
  {/if}

  <dl>
    <dt>โฟลเดอร์</dt>
    <dd class="mono">{request.cwd}</dd>
    <dt>ปลอดภัย (is_safe)</dt>
    <dd>{pct(c.is_safe)}</dd>
    <dt>ประเภทผลกระทบ</dt>
    <dd class:danger={risky}>
      {c.risk ? `${RISK_TEXT[c.risk] ?? c.risk} (${pct(c.risk_p)})` : "–"}
    </dd>
    <dt>ผู้ตัดสิน</dt>
    <dd>{sourceText(c.source)}</dd>
  </dl>

  <footer>
    <button type="button" class="deny" onclick={() => answer("deny")}>ปฏิเสธ</button>
    {#if editing}
      <button type="button" onclick={() => (editing = false)}>ยกเลิกการแก้</button>
      <button
        type="button"
        class="run"
        disabled={edited.trim() === ""}
        onclick={() => answer({ edited })}>รันคำสั่งที่แก้</button
      >
    {:else}
      <button type="button" onclick={() => (editing = true)}>แก้ไข</button>
      <button type="button" class="run" onclick={() => answer("run")}>รัน</button>
    {/if}
  </footer>
</dialog>

<style>
  dialog {
    width: min(640px, calc(100vw - 32px));
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--bg-raised);
    color: var(--ink);
    padding: 20px 22px;
  }
  dialog::backdrop {
    background: rgb(10 14 13 / 0.45);
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  h2 {
    margin: 0;
    font-size: 1.05rem;
  }
  .conv {
    color: var(--ink-soft);
    font-size: 0.8rem;
  }
  .reason {
    margin: 8px 0 12px;
    color: var(--ink-soft);
    font-size: 0.86rem;
  }
  .reason.warn {
    color: var(--warn);
  }
  .cmd,
  textarea {
    width: 100%;
    box-sizing: border-box;
    font-family: var(--mono);
    font-size: 0.86rem;
    background: var(--bg-sunk);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 0;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--ink);
  }
  .lbl {
    display: block;
    font-size: 0.8rem;
    color: var(--ink-soft);
    margin-bottom: 4px;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 16px;
    margin: 14px 0 18px;
    font-size: 0.86rem;
  }
  dt {
    color: var(--ink-soft);
  }
  dd {
    margin: 0;
    word-break: break-all;
  }
  .danger {
    color: var(--danger);
    font-weight: 600;
  }
  .mono {
    font-family: var(--mono);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .run {
    background: var(--accent);
    color: var(--on-accent);
    border-color: var(--accent);
  }
  .deny {
    margin-right: auto;
    color: var(--danger);
  }
</style>
