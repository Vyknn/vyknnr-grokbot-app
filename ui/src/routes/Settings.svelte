<script lang="ts">
  import { onMount } from "svelte";
  import { api, errorText } from "../lib/api";
  import { unavailableReason } from "../lib/format";
  import { PROVIDER_LABEL, type ProviderId, type ProviderStatus, type SecretName, type Settings } from "../lib/types";

  let {
    status,
    onrefreshstatus,
    onerror,
  }: {
    status: ProviderStatus | null;
    onrefreshstatus: () => void;
    onerror: (msg: string) => void;
  } = $props();

  const KEYS: { name: Exclude<SecretName, "mcp_hermes">; label: string; hint: string }[] = [
    { name: "jev", label: "TypeSafe (Jev) API key", hint: "ใช้เลือกตัวคุยและตรวจคำสั่ง" },
    { name: "anthropic", label: "Anthropic API key", hint: "สำหรับตัวคุยแบบ API" },
    { name: "hermes_api", label: "Hermes API key", hint: "ค่าเดียวกับ API_SERVER_KEY ของ Hermes" },
  ];
  const PROVIDERS: ProviderId[] = ["hermes", "claude_cli", "api"];

  let s: Settings | null = $state(null);
  let has: Record<string, boolean> = $state({});
  let keyInput: Record<string, string> = $state({});
  let toolsetsText = $state("");
  let saving = $state(false);
  let saved = $state("");
  let snippet: string | null = $state(null);
  let copied = $state(false);

  async function refreshKeys() {
    for (const k of KEYS) has[k.name] = await api.hasSecret(k.name);
  }

  onMount(async () => {
    try {
      s = await api.getSettings();
      toolsetsText = s.hermes_allowed_toolsets.join(", ");
      await refreshKeys();
    } catch (e) {
      onerror(errorText(e));
    }
  });

  async function setKey(name: SecretName, value: string) {
    try {
      await api.setSecret(name, value);
      keyInput[name] = "";
      await refreshKeys();
      onrefreshstatus();
    } catch (e) {
      onerror(errorText(e));
    }
  }

  async function save() {
    if (!s) return;
    saving = true;
    saved = "";
    try {
      const next: Settings = {
        ...s,
        hermes_allowed_toolsets: toolsetsText
          .split(/[\s,]+/)
          .map((t) => t.trim())
          .filter(Boolean),
        claude_model: s.claude_model?.trim() ? s.claude_model.trim() : null,
      };
      await api.saveSettings(next);
      s = next;
      saved = "บันทึกแล้ว";
      snippet = null;
      onrefreshstatus();
    } catch (e) {
      onerror(errorText(e));
    } finally {
      saving = false;
    }
  }

  async function showSnippet() {
    try {
      snippet = await api.hermesSetupSnippet();
    } catch (e) {
      onerror(errorText(e));
    }
  }

  async function copySnippet() {
    if (!snippet) return;
    try {
      await navigator.clipboard.writeText(snippet);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch (e) {
      onerror(errorText(e));
    }
  }
</script>

<section class="settings">
  <h1>ตั้งค่า</h1>

  <div class="privacy" role="note">
    <strong>ข้อมูลที่ส่งออกจากเครื่อง:</strong>
    เมื่อตั้ง Jev key แล้ว ข้อความของคุณ (และประวัติล่าสุดบางส่วน) จะถูกส่งไปที่ TypeSafe (Jev) เพื่อเลือกตัวคุย
    และทุกคำสั่งที่ตัวคุยเสนอจะถูกส่งไปให้ Jev ตรวจก่อนรัน ทุกครั้งที่ตรวจจะใช้ token ของบัญชี TypeSafe ของคุณ
  </div>

  {#if status?.notices.length}
    <div class="startup" role="alert">
      {#each status.notices as n}<p>{n}</p>{/each}
    </div>
  {/if}

  <h2>สถานะตัวคุย <button type="button" class="link" onclick={onrefreshstatus}>ตรวจใหม่</button></h2>
  <ul class="status">
    {#each PROVIDERS as p (p)}
      {@const why = unavailableReason(status?.[p])}
      <li>
        <span class="dot" class:ok={why === null}></span>
        <strong>{PROVIDER_LABEL[p]}</strong>
        <span class="why">{why ?? "พร้อมใช้งาน"}</span>
      </li>
    {/each}
  </ul>

  <h2>API keys</h2>
  <p class="hint">key เก็บใน Windows Credential Manager แอปไม่แสดงค่าที่บันทึกไว้</p>
  <div class="keys">
    {#each KEYS as k (k.name)}
      <div class="key">
        <div class="key-head">
          <label for={`key-${k.name}`}>{k.label}</label>
          <span class="pill" class:set={has[k.name]}>{has[k.name] ? "ตั้งค่าแล้ว" : "ยังไม่ได้ตั้ง"}</span>
        </div>
        <div class="key-row">
          <input
            id={`key-${k.name}`}
            type="password"
            autocomplete="off"
            spellcheck="false"
            placeholder={k.hint}
            bind:value={keyInput[k.name]}
          />
          <button type="button" disabled={!keyInput[k.name]?.trim()} onclick={() => setKey(k.name, keyInput[k.name] ?? "")}>
            บันทึก
          </button>
          <button type="button" disabled={!has[k.name]} onclick={() => setKey(k.name, "")}>ลบ</button>
        </div>
      </div>
    {/each}
  </div>

  {#if s}
    <form
      onsubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <h2>การตัดสินใจ</h2>
      <div class="grid">
        <label>
          เกณฑ์เลือกตัวคุยอัตโนมัติ
          <input type="number" min="0" max="1" step="0.01" bind:value={s.thresholds.route} />
        </label>
        <label>
          เกณฑ์ is_safe ที่รันเองได้
          <input type="number" min="0" max="1" step="0.01" bind:value={s.thresholds.command_safe} />
        </label>
        <label>
          เกณฑ์ read_only ที่รันเองได้
          <input type="number" min="0" max="1" step="0.01" bind:value={s.thresholds.command_risk} />
        </label>
        <label>
          จำนวนคำสั่งสูงสุดต่อข้อความ
          <input type="number" min="1" step="1" bind:value={s.max_commands_per_turn} />
        </label>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={s.auto_route} />
        ให้ Jev เลือกตัวคุยอัตโนมัติ (ปิดแล้วจะใช้ตัวคุยเริ่มต้น หรือถามทุกครั้ง)
      </label>
      <label>
        ตัวคุยเริ่มต้น
        <select bind:value={s.default_provider}>
          <option value={null}>ไม่มี (ถาม)</option>
          {#each PROVIDERS as p (p)}<option value={p}>{PROVIDER_LABEL[p]}</option>{/each}
        </select>
      </label>

      <h2>คำอธิบายตัวคุย</h2>
      <p class="hint">Jev ใช้คำอธิบายนี้ตัดสินว่าข้อความควรไปตัวไหน</p>
      {#each PROVIDERS as p (p)}
        <label>
          {PROVIDER_LABEL[p]}
          <textarea rows="2" bind:value={s.provider_descriptions[p]}></textarea>
        </label>
      {/each}

      <h2>ตัวคุย</h2>
      <div class="grid">
        <label>
          โมเดล Anthropic
          <input bind:value={s.anthropic_model} spellcheck="false" />
        </label>
        <label>
          Hermes base URL
          <input bind:value={s.hermes_base_url} spellcheck="false" />
        </label>
        <label>
          พอร์ต MCP server (มีผลเมื่อเปิดแอปใหม่)
          <input type="number" min="1" max="65535" bind:value={s.mcp_port} />
        </label>
      </div>
      <label>
        toolset ของ Hermes ที่อนุญาตให้เปิด (คั่นด้วยจุลภาค)
        <textarea rows="2" bind:value={toolsetsText} spellcheck="false"></textarea>
      </label>

      <div class="actions">
        <button type="submit" class="primary" disabled={saving}>{saving ? "กำลังบันทึก…" : "บันทึกการตั้งค่า"}</button>
        <span class="saved">{saved}</span>
      </div>
    </form>

    <h2>เชื่อม Hermes</h2>
    <p class="hint">
      คัดลอกข้อความนี้ไปใส่ใน config ของ Hermes ข้อความมี key ของ MCP server ในเครื่องนี้ อย่าแชร์ให้คนอื่น
    </p>
    {#if snippet}
      <pre class="snippet">{snippet}</pre>
      <div class="actions">
        <button type="button" onclick={copySnippet}>{copied ? "คัดลอกแล้ว" : "คัดลอก"}</button>
        <button type="button" onclick={() => (snippet = null)}>ซ่อน</button>
      </div>
    {:else}
      <button type="button" onclick={showSnippet}>แสดงข้อความตั้งค่า Hermes</button>
    {/if}
  {/if}
</section>

<style>
  .settings {
    overflow-y: auto;
    padding: 20px 28px 48px;
    max-width: 820px;
  }
  h1 {
    font-size: 1.2rem;
    margin: 0 0 14px;
  }
  h2 {
    font-size: 0.95rem;
    margin: 26px 0 8px;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .privacy {
    border-left: 3px solid var(--warn);
    background: var(--bg-sunk);
    padding: 10px 14px;
    border-radius: 0 8px 8px 0;
    font-size: 0.88rem;
    line-height: 1.6;
  }
  .startup {
    margin-top: 10px;
    border: 1px solid var(--danger);
    border-radius: 8px;
    padding: 4px 12px;
    color: var(--danger);
    font-size: 0.86rem;
  }
  .hint {
    color: var(--ink-soft);
    font-size: 0.82rem;
    margin: 0 0 8px;
  }
  .status {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .status li {
    display: flex;
    gap: 10px;
    align-items: center;
    font-size: 0.88rem;
  }
  .status strong {
    min-width: 88px;
  }
  .why {
    color: var(--ink-soft);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ink-faint);
  }
  .dot.ok {
    background: var(--ok);
  }
  .keys {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .key-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 0.86rem;
    margin-bottom: 4px;
  }
  .key-row {
    display: flex;
    gap: 6px;
  }
  .key-row input {
    flex: 1;
  }
  .pill {
    font-size: 0.74rem;
    padding: 1px 8px;
    border-radius: 999px;
    border: 1px solid var(--line);
    color: var(--ink-soft);
  }
  .pill.set {
    color: var(--ok);
    border-color: var(--ok);
  }
  form label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 0.86rem;
    margin-bottom: 10px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 0 16px;
  }
  label.check {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 8px;
  }
  .primary {
    background: var(--accent);
    color: var(--on-accent);
    border-color: var(--accent);
  }
  .saved {
    color: var(--ok);
    font-size: 0.84rem;
  }
  .link {
    border: 0;
    background: transparent;
    color: var(--accent);
    font-size: 0.8rem;
    padding: 0;
  }
  .snippet {
    background: var(--bg-sunk);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 12px;
    font-family: var(--mono);
    font-size: 0.8rem;
    white-space: pre-wrap;
    word-break: break-all;
    user-select: text;
  }
</style>
