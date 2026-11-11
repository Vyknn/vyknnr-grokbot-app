<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, errorText } from "../lib/api";
  import type { Conversation } from "../lib/types";

  let {
    conversations,
    selected,
    view,
    onselect,
    onchanged,
    onsettings,
    onerror,
  }: {
    conversations: Conversation[];
    selected: number | null;
    view: "chat" | "settings";
    onselect: (id: number) => void;
    onchanged: (selectId?: number | null) => void;
    onsettings: () => void;
    onerror: (msg: string) => void;
  } = $props();

  let query = $state("");
  let results: Conversation[] | null = $state(null);
  let renaming: number | null = $state(null);
  let renameText = $state("");
  let confirmDelete: number | null = $state(null);
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  const shown = $derived(results ?? conversations);

  function onsearch() {
    clearTimeout(searchTimer);
    const q = query.trim();
    if (!q) {
      results = null;
      return;
    }
    searchTimer = setTimeout(async () => {
      try {
        results = await api.searchConversations(q);
      } catch (e) {
        onerror(errorText(e));
      }
    }, 200);
  }

  async function newChat() {
    try {
      const dir = await open({ directory: true, multiple: false, title: "เลือกโฟลเดอร์ทำงานของแชตนี้" });
      if (typeof dir !== "string") return;
      const name = dir.split(/[\\/]/).filter(Boolean).pop() ?? "แชตใหม่";
      const c = await api.createConversation(name, dir);
      query = "";
      results = null;
      onchanged(c.id);
    } catch (e) {
      onerror(errorText(e));
    }
  }

  function startRename(c: Conversation) {
    renaming = c.id;
    renameText = c.title;
  }

  async function commitRename(id: number) {
    const t = renameText.trim();
    renaming = null;
    if (!t) return;
    try {
      await api.renameConversation(id, t);
      onchanged();
      if (results) onsearch();
    } catch (e) {
      onerror(errorText(e));
    }
  }

  async function remove(id: number) {
    confirmDelete = null;
    try {
      await api.deleteConversation(id);
      onchanged(selected === id ? null : undefined);
      if (results) onsearch();
    } catch (e) {
      onerror(errorText(e));
    }
  }

  function focus(el: HTMLInputElement) {
    el.focus();
    el.select();
  }
</script>

<aside>
  <div class="brand">
    <span class="mark" aria-hidden="true"></span>
    <span>Assistant</span>
  </div>

  <button type="button" class="new" onclick={newChat}>+ แชตใหม่ในโฟลเดอร์…</button>

  <input
    class="search"
    type="search"
    placeholder="ค้นหาแชต"
    bind:value={query}
    oninput={onsearch}
    aria-label="ค้นหาแชต"
  />

  <ul>
    {#each shown as c (c.id)}
      <li class:active={c.id === selected && view === "chat"}>
        {#if renaming === c.id}
          <input
            class="rename"
            bind:value={renameText}
            use:focus
            onkeydown={(e) => {
              if (e.key === "Enter") commitRename(c.id);
              if (e.key === "Escape") renaming = null;
            }}
            onblur={() => commitRename(c.id)}
            aria-label="ชื่อแชตใหม่"
          />
        {:else if confirmDelete === c.id}
          <div class="confirm">
            <span>ลบแชตนี้?</span>
            <button type="button" class="danger" onclick={() => remove(c.id)}>ลบ</button>
            <button type="button" onclick={() => (confirmDelete = null)}>ไม่</button>
          </div>
        {:else}
          <button type="button" class="item" onclick={() => onselect(c.id)} title={c.cwd}>
            <span class="title">{c.title}</span>
            <span class="cwd">{c.cwd}</span>
          </button>
          <div class="acts">
            <button type="button" title="เปลี่ยนชื่อ" aria-label="เปลี่ยนชื่อ" onclick={() => startRename(c)}>✎</button>
            <button type="button" title="ลบ" aria-label="ลบ" onclick={() => (confirmDelete = c.id)}>✕</button>
          </div>
        {/if}
      </li>
    {:else}
      <li class="empty">{results ? "ไม่พบแชตที่ตรงกัน" : "ยังไม่มีแชต"}</li>
    {/each}
  </ul>

  <button type="button" class="settings" class:on={view === "settings"} onclick={onsettings}>ตั้งค่า</button>
</aside>

<style>
  aside {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px 10px;
    background: var(--bg-side);
    border-right: 1px solid var(--line);
    min-height: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    letter-spacing: 0.02em;
    padding: 2px 6px 6px;
  }
  .mark {
    width: 14px;
    height: 14px;
    border-radius: 4px;
    background: var(--accent);
    box-shadow: inset 0 0 0 3px var(--bg-side), inset 0 0 0 5px var(--accent);
  }
  .new {
    background: var(--accent);
    color: var(--on-accent);
    border-color: var(--accent);
    text-align: left;
  }
  .search {
    width: 100%;
    box-sizing: border-box;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    position: relative;
    border-radius: 8px;
  }
  li.active {
    background: var(--bg-raised);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  li:hover .acts,
  li:focus-within .acts {
    opacity: 1;
  }
  .item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    border: 0;
    background: transparent;
    padding: 7px 56px 7px 10px;
    text-align: left;
  }
  .title {
    font-size: 0.88rem;
    color: var(--ink);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }
  .cwd {
    font-size: 0.72rem;
    color: var(--ink-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
    direction: rtl;
    text-align: left;
  }
  .acts {
    position: absolute;
    right: 4px;
    top: 6px;
    display: flex;
    gap: 2px;
    opacity: 0;
  }
  .acts button {
    border: 0;
    background: transparent;
    padding: 2px 6px;
    color: var(--ink-soft);
  }
  .rename {
    width: 100%;
    box-sizing: border-box;
  }
  .confirm {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    font-size: 0.84rem;
  }
  .confirm span {
    flex: 1;
  }
  .danger {
    color: var(--danger);
  }
  .empty {
    color: var(--ink-faint);
    font-size: 0.84rem;
    padding: 8px 10px;
  }
  .settings {
    text-align: left;
  }
  .settings.on {
    background: var(--bg-raised);
  }
</style>
