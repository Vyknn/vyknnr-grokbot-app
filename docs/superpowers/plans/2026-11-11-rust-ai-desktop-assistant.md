# แผนการสร้าง: แอปผู้ช่วย AI บนเดสก์ท็อป (Rust) v1

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** สร้างแอปแชต AI บน Windows ด้วย Tauri 2 ที่สลับคุยกับ Hermes Agent, Claude CLI และ Anthropic API ได้ ให้ Jev เลือกตัวคุยและตรวจทุกคำสั่งก่อนรัน เก็บประวัติใน SQLite และ export ได้

**Architecture:** ตรรกะทั้งหมดอยู่ใน crate `assistant-core` ซึ่งทดสอบได้โดยไม่ต้องมี Tauri ส่วน `src-tauri` เป็นแค่ตัวเชื่อม UI กับ core การรันคำสั่งทุกครั้งต้องผ่าน `CommandService::authorize` จุดเดียว ทั้งจาก Anthropic tool use, จาก MCP server ในเครื่องที่ Claude CLI และ Hermes เรียก และจาก `approval.request` ของ Hermes

**Tech Stack:** Rust ≥ 1.85 (edition 2021 สำหรับ crate ของเรา), Tauri 2, tokio, reqwest (rustls, stream), serde/serde_json, rusqlite (bundled), keyring, rmcp (streamable HTTP server) + axum, wiremock (ทดสอบ), Svelte 5 + TypeScript + Vite + vitest (UI)

**Spec:** `docs/superpowers/specs/2026-11-11-rust-ai-desktop-assistant-design.md`

## Global Constraints

- แพลตฟอร์ม v1: Windows 11 เท่านั้น shell ที่ใช้รันคำสั่งคือ PowerShell (`powershell.exe -NoProfile -NonInteractive -Command`)
- ห้ามคัดลอกโค้ด ชื่อ โลโก้ หรือหน้าตาจาก Grok Bot และห้ามนำไฟล์จาก `Grok_Bot_0.79.4_Setup.exe` มาใช้ ชื่อแอปตอนนี้เป็นชื่อชั่วคราว: `Assistant`, identifier `com.local.assistant`
- API key ทุกตัว (Jev, Anthropic, Hermes) เก็บใน Windows Credential Manager ผ่าน `keyring` ด้วย service `com.local.assistant` ห้ามเขียน key ลงไฟล์ตั้งค่า ฐานข้อมูล log หรือไฟล์ export
- Jev: `POST https://api.typesafe.ai/v1/systemone`, header `Authorization: Bearer <key>`, `model: "jev-latest"`
- เกณฑ์เริ่มต้น (ปรับได้ในหน้าตั้งค่า): route ≥ 0.75, `is_safe` ≥ 0.9, `risk == read_only` ที่ probability ≥ 0.9
- จำกัดการรันคำสั่งไม่เกิน 20 ครั้งต่อหนึ่งข้อความผู้ใช้ (ปรับได้) เมื่อครบต้องถามผู้ใช้
- ถ้าไม่แน่ใจ ให้ถามผู้ใช้ก่อนเสมอ: Jev error/timeout/คำตอบผิดรูปแบบ, ผู้ใช้ปิดกล่องอนุมัติ, approval หมดเวลา และแอปปิดระหว่างรออนุมัติ ทุกกรณีถือเป็น "ไม่รัน" หรือ "ถามผู้ใช้"
- ค่าเริ่มต้นโมเดลของ Anthropic API คือ `claude-opus-5-5` และ `anthropic-version: 2023-06-01`
- Hermes API server ค่าเริ่มต้นอยู่ที่ `http://127.0.0.1:8642/v1` ใช้ header `Authorization: Bearer <API_SERVER_KEY>`
- MCP server ของแอปฟังที่ `127.0.0.1` เท่านั้น พอร์ตเริ่มต้น `8765` (ปรับได้)
- หน้าตั้งค่าต้องแจ้งผู้ใช้ว่าข้อความและคำสั่งที่ให้ Jev ตรวจจะถูกส่งไปที่ TypeSafe และมีค่าใช้จ่ายตาม token

## Review Focus

1. **Jev ตอบผิดปกติ** (HTTP 4xx/5xx, timeout, JSON ผิดรูป หรือไม่มี answer id ที่ถาม): route ต้องให้ผู้ใช้เลือก และคำสั่งต้องถามผู้ใช้ ห้ามรันเอง ทดสอบใน Task 4
2. **คำสั่งค้างหรือ output ยาวมาก**: คำสั่งต้องถูกหยุดเมื่อครบ timeout ต้องเก็บ output ไม่เกิน 64 KiB และส่งกลับให้โมเดลไม่เกิน 16 KiB พร้อมบอกว่าถูกตัด ทดสอบใน Task 8
3. **ตัวคุยที่ Jev เลือกใช้ไม่ได้** (Hermes ไม่ได้เปิด หรือไม่เจอ `claude`): แชตต้องแสดง error และให้ผู้ใช้เลือกตัวอื่น โดยข้อความของผู้ใช้ไม่หาย ทดสอบใน Task 13
4. **ข้อความไทยและ path ที่มีช่องว่าง**: ข้อความไทยต้องผ่าน PowerShell, SQLite และ export โดยไม่เพี้ยน และ cwd เช่น `C:\Users\A B\proj` ต้องใช้ได้ ทดสอบใน Task 6 และ 8
5. **ไม่มีใครตอบกล่องอนุมัติ** (ผู้ใช้ปิดกล่อง, ปิดแชต หรือ approval ใน MCP ที่ Claude CLI รออยู่หลุด): ต้องได้ผลเป็น deny และไม่รันคำสั่ง ทดสอบใน Task 9

---

## โครงสร้างไฟล์

```
Cargo.toml                      # workspace: crates/core, src-tauri
crates/core/
  Cargo.toml                    # name = "assistant-core"
  src/lib.rs                    # pub mod ทั้งหมด
  src/types.rs                  # ProviderId, Role, Message, ChatEvent, TurnRequest, ...
  src/jev/mod.rs                # JevClient, คำถาม/คำตอบแบบมีชนิด
  src/decision/policy.rs        # ฟังก์ชันบริสุทธิ์: Thresholds -> outcome
  src/decision/router.rs        # Router: Jev + policy + fallback
  src/decision/gate.rs          # CommandGate: Jev + policy + fallback
  src/store/mod.rs              # Store (SQLite) + migrations
  src/export.rs                 # Markdown / JSON
  src/history.rs                # แปลงประวัติกลาง -> รูปแบบของแต่ละตัวคุย
  src/runner.rs                 # รัน PowerShell, stream, timeout, cancel
  src/command_service.rs        # จุดตรวจเดียว: gate + approver + cap + runner + บันทึก
  src/mcp_server.rs             # MCP server: tool run_command
  src/providers/mod.rs          # trait ChatProvider
  src/providers/anthropic.rs
  src/providers/claude_cli.rs
  src/providers/hermes.rs
  src/orchestrator.rs           # หนึ่ง turn: route -> provider -> commands -> store
  src/settings.rs               # Settings (JSON) + SecretStore (keyring)
  tests/                        # integration tests ต่อ module
src-tauri/                      # Tauri 2 app: commands + events + approver
ui/                             # Svelte 5 + TS + Vite
```

---

### Task 1: Workspace และชนิดข้อมูลหลัก

**Files:**
- Create: `Cargo.toml`, `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/core/src/types.rs`, `.gitignore`
- Test: `crates/core/src/types.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `enum ProviderId { Hermes, ClaudeCli, Api }` + `as_str()`/`FromStr` ด้วยค่า `"hermes" | "claude_cli" | "api"`
  - `enum Role { User, Assistant, CommandResult, Notice }`
  - `struct Message { id: i64, conversation_id: i64, role: Role, provider: Option<ProviderId>, content: String, created_at: DateTime<Utc> }`
  - `struct NewMessage { role, provider, content }`
  - `enum ChatEvent { TextDelta(String), CommandProposed { call_id: String, command: String, cwd: Option<PathBuf> }, Done, Error(String) }`
  - `struct TurnRequest { conversation_id: i64, turn_token: String, history: Vec<Message>, cwd: PathBuf }`
  - `enum Availability { Ready, Unavailable(String) }`
  - ทุกชนิด derive `Debug, Clone, Serialize, Deserialize, PartialEq` (เว้น `DateTime` ใช้ chrono serde)

- [ ] **Step 1:** รัน `rustup update stable` แล้วตรวจว่า `rustc --version` ได้ ≥ 1.85 ก่อนเริ่ม
- [ ] **Step 2: เขียน test ที่ fail**

```rust
#[test]
fn provider_id_round_trips_as_snake_case() {
    for (p, s) in [(ProviderId::Hermes,"hermes"),(ProviderId::ClaudeCli,"claude_cli"),(ProviderId::Api,"api")] {
        assert_eq!(p.as_str(), s);
        assert_eq!(s.parse::<ProviderId>().unwrap(), p);
        assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{s}\""));
    }
    assert!("grok".parse::<ProviderId>().is_err());
}
```

- [ ] **Step 3:** รัน `cargo test -p assistant-core` ต้อง FAIL (ยังไม่มีชนิด)
- [ ] **Step 4:** เขียน workspace (`resolver = "2"`), `assistant-core` และชนิดตาม Interfaces (`#[serde(rename_all = "snake_case")]`) เพิ่ม `Grok_Bot_*.exe`, `target/`, `ui/node_modules/` และ `ui/dist/` ลง `.gitignore`
- [ ] **Step 5:** รัน `cargo test -p assistant-core` ต้อง PASS
- [ ] **Step 6:** Commit `feat(core): workspace and core domain types`

---

### Task 2: Jev client

**Files:**
- Create: `crates/core/src/jev/mod.rs`
- Test: `crates/core/tests/jev_client.rs`

**Interfaces:**
- Produces:
  - `enum JevQuestion { Choice { instructions: String, criteria: IndexMap<String, String> }, Noul { instructions: String } }` serialize เป็น `{"type":"choice","instructions":..,"criteria":{..}}` / `{"type":"noul","instructions":..}`
  - `enum JevAnswer { Choice { choice: String, probabilities: HashMap<String, f64>, confidence: f64 }, Noul { noul: f64 }, Other }` (`#[serde(tag="type")]`, `Other` เป็น `#[serde(other)]`)
  - `struct JevResponse { model: String, answers: HashMap<String, JevAnswer> }`
  - `enum JevError { Http(u16, String), Transport(String), Decode(String), MissingAnswer(String) }`
  - `#[async_trait] trait JevApi: Send + Sync { async fn ask(&self, state: &str, questions: IndexMap<String, JevQuestion>) -> Result<JevResponse, JevError>; }`
  - `struct JevClient { base_url: Url, api_key: String, http: reqwest::Client }` implement `JevApi` ด้วย timeout 10 วินาที
  - `JevResponse::choice(&self, id) -> Result<(&str, &HashMap<String,f64>), JevError>` และ `noul(&self, id) -> Result<f64, JevError>` คืน `MissingAnswer(id)` เมื่อไม่มี id หรือชนิดไม่ตรง

- [ ] **Step 1: เขียน test ที่ fail** (ใช้ wiremock)

```rust
#[tokio::test]
async fn sends_bearer_and_parses_choice_and_noul() {
    // mock: POST /v1/systemone, header Authorization == "Bearer k", body_partial_json {"model":"jev-latest"}
    // ตอบ {"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"api","confidence":0.8,
    //   "probabilities":{"api":0.9,"hermes":0.1}},"is_safe":{"type":"noul","noul":0.97}},"usage":{"input_tokens":1,"output_tokens":1}}
    let r = client.ask("ภาษาไทย", qs).await.unwrap();
    assert_eq!(r.choice("route").unwrap().0, "api");
    assert_eq!(r.noul("is_safe").unwrap(), 0.97);
    assert!(matches!(r.noul("route"), Err(JevError::MissingAnswer(_))));
}
#[tokio::test] async fn http_500_is_http_error() { /* assert matches!(err, JevError::Http(500, _)) */ }
#[tokio::test] async fn garbage_body_is_decode_error() { /* body "not json" -> JevError::Decode */ }
#[tokio::test] async fn slow_server_is_transport_error() { /* delay 11s -> JevError::Transport */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test jev_client` ต้อง FAIL
- [ ] **Step 3:** เขียน `JevClient` ตาม Interfaces body ต้องเป็น `{"state":..,"model":"jev-latest","questions":{..}}` และ test timeout ให้ตั้ง timeout ผ่าน constructor `JevClient::with_timeout` เพื่อไม่ต้องรอ 11 วินาทีจริง
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): typed Jev client`

---

### Task 3: นโยบายการตัดสินใจ (ฟังก์ชันบริสุทธิ์)

**Files:**
- Create: `crates/core/src/decision/mod.rs`, `crates/core/src/decision/policy.rs`
- Test: ใน `policy.rs`

**Interfaces:**
- Produces:
  - `struct Thresholds { route: f64, command_safe: f64, command_risk: f64 }` + `Default` = `0.75, 0.9, 0.9`
  - `enum Risk { ReadOnly, ModifiesFiles, Destructive, Network, SystemChange, Other }` (snake_case ตรงกับ criteria key ของ Jev)
  - `enum DecisionSource { Jev { model: String }, Fallback { reason: String }, Manual }`
  - `fn route_outcome(choice: &str, probs: &HashMap<String,f64>, available: &[ProviderId], t: &Thresholds) -> RouteOutcome`
  - `enum RouteOutcome { Auto(ProviderId), AskUser { ranked: Vec<(ProviderId, f64)> } }` (`ranked` เรียงจากมากไปน้อย และมีเฉพาะตัวที่อยู่ใน `available`)
  - `fn command_outcome(is_safe: f64, risk: &str, risk_probs: &HashMap<String,f64>, t: &Thresholds) -> CommandVerdict`
  - `enum CommandVerdict { AutoRun, AskUser }`

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[test] fn route_auto_at_threshold() { assert_eq!(route_outcome("api", &p(&[("api",0.75),("hermes",0.25)]), &[Api,Hermes], &T), RouteOutcome::Auto(Api)); }
#[test] fn route_below_threshold_asks_ranked() { /* api 0.6, hermes 0.4 -> AskUser{ranked:[(Api,0.6),(Hermes,0.4)]} */ }
#[test] fn route_choice_not_available_asks() { /* choice "hermes" 0.95 แต่ available=[Api] -> AskUser{ranked:[(Api,..)]} */ }
#[test] fn route_unknown_label_asks() { /* choice "grok" -> AskUser */ }
#[test] fn command_auto_only_when_safe_and_read_only() {
    assert_eq!(command_outcome(0.95, "read_only", &p(&[("read_only",0.92)]), &T), CommandVerdict::AutoRun);
    assert_eq!(command_outcome(0.89, "read_only", &p(&[("read_only",0.99)]), &T), CommandVerdict::AskUser);
    assert_eq!(command_outcome(0.99, "read_only", &p(&[("read_only",0.89)]), &T), CommandVerdict::AskUser);
    assert_eq!(command_outcome(0.99, "modifies_files", &p(&[("modifies_files",0.99)]), &T), CommandVerdict::AskUser);
}
#[test] fn command_nan_asks() { assert_eq!(command_outcome(f64::NAN, "read_only", &p(&[("read_only",1.0)]), &T), CommandVerdict::AskUser); }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core decision::policy` ต้อง FAIL
- [ ] **Step 3:** เขียนฟังก์ชันตาม Interfaces ใช้ `>=` เทียบกับ threshold ค่า NaN หรือนอกช่วง 0..=1 ให้ถือว่าไม่ผ่าน และ AutoRun ต้องได้ `risk == "read_only"` พร้อม `risk_probs["read_only"] >= command_risk`
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): routing and command decision policy`

---

### Task 4: Router และ CommandGate (Jev + นโยบาย + fallback)

**Files:**
- Create: `crates/core/src/decision/router.rs`, `crates/core/src/decision/gate.rs`
- Test: `crates/core/tests/decision_services.rs`

**Interfaces:**
- Consumes: `JevApi`, `JevResponse::{choice,noul}` (Task 2), `route_outcome`, `command_outcome`, `Thresholds`, `DecisionSource` (Task 3)
- Produces:
  - `struct ProviderProfile { id: ProviderId, description: String }` (คำอธิบายที่ส่งเป็น criteria ผู้ใช้แก้ได้)
  - `struct Router { jev: Option<Arc<dyn JevApi>>, thresholds: Thresholds }`
  - `Router::decide(&self, user_text: &str, recent: &[Message], available: &[ProviderProfile]) -> RouteDecision`
  - `struct RouteDecision { outcome: RouteOutcome, source: DecisionSource }`
  - `struct CommandGate { jev: Option<Arc<dyn JevApi>>, thresholds: Thresholds }`
  - `CommandGate::check(&self, command: &str, cwd: &Path) -> CommandCheck`
  - `struct CommandCheck { verdict: CommandVerdict, is_safe: Option<f64>, risk: Option<String>, risk_p: Option<f64>, source: DecisionSource }`
- คำถามที่ส่งให้ Jev:
  - route: id `"route"` เป็น Choice โดย criteria = `{provider.as_str(): description}` ของตัวที่ใช้ได้เท่านั้น `state` = ข้อความผู้ใช้ล่าสุด + ข้อความล่าสุดไม่เกิน 6 ข้อความ ตัดรวมไม่เกิน 4,000 ตัวอักษร
  - command: id `"is_safe"` เป็น Noul ใช้ instructions ตามข้อ 9.2 ของ spec ตรงตัว และ id `"risk"` เป็น Choice โดย criteria มี 6 ค่าตาม `Risk` `state` = `"cwd: <cwd>\ncommand: <command>"`

- [ ] **Step 1: เขียน test ที่ fail** โดยใช้ `FakeJev` ที่คืนค่าที่ตั้งไว้ หรือคืน error

```rust
#[tokio::test] async fn router_without_jev_asks_with_fallback_source() { /* jev None -> AskUser, source Fallback{reason:"jev_not_configured"} */ }
#[tokio::test] async fn router_single_available_provider_skips_jev() { /* available=[Api] -> Auto(Api), source Manual; FakeJev.calls()==0 */ }
#[tokio::test] async fn router_jev_error_falls_back_to_ask() { /* FakeJev Err(Http(503)) -> AskUser, source Fallback */ }
#[tokio::test] async fn router_sends_only_available_providers_as_criteria() { /* FakeJev.last_questions()["route"] criteria keys == {"api","hermes"} */ }
#[tokio::test] async fn gate_error_or_missing_answer_asks() {
    for fake in [FakeJev::err(JevError::Transport("x".into())), FakeJev::ok_without("risk"), FakeJev::ok_garbage_type("is_safe")] {
        let c = CommandGate::new(Some(fake), T).check("dir", Path::new("C:\\")).await;
        assert_eq!(c.verdict, CommandVerdict::AskUser);
        assert!(matches!(c.source, DecisionSource::Fallback{..}));
    }
}
#[tokio::test] async fn gate_auto_runs_safe_read_only() { /* is_safe .97, risk read_only .95 -> AutoRun, source Jev{model} */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test decision_services` ต้อง FAIL
- [ ] **Step 3:** เขียน `Router` และ `CommandGate` ตาม Interfaces ทุก `Err` ต้องกลายเป็น `AskUser` + `Fallback { reason }` ห้าม propagate error ออกไป
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): Jev-backed router and command gate with safe fallback`

---

### Task 5: Store (SQLite)

**Files:**
- Create: `crates/core/src/store/mod.rs`, `crates/core/src/store/migrations.sql`
- Test: `crates/core/tests/store.rs`

**Interfaces:**
- Consumes: `Message`, `NewMessage`, `ProviderId`, `Role` (Task 1), `DecisionSource`, `CommandCheck` (Task 3/4)
- Produces:
  - `struct Store` (ห่อ `rusqlite::Connection` ไว้ใน `Mutex`) + `Store::open(path: &Path) -> Result<Store>` และ `Store::open_in_memory()`
  - `struct Conversation { id: i64, title: String, cwd: PathBuf, created_at, updated_at }`
  - `create_conversation(title, cwd) -> Conversation`, `list_conversations() -> Vec<Conversation>` (เรียงจาก `updated_at` ล่าสุด), `rename_conversation(id, title)`, `delete_conversation(id)` (ลบ message/decision/command ของแชตนั้นตาม cascade), `search(query) -> Vec<Conversation>` (ค้นจาก title หรือ content ด้วย `LIKE` แบบไม่สนตัวพิมพ์)
  - `append_message(conv_id, NewMessage) -> Message` (อัปเดต `updated_at` ด้วย), `messages(conv_id) -> Vec<Message>`
  - `record_decision(message_id: i64, kind: DecisionKind, payload: &serde_json::Value)` โดย `enum DecisionKind { Route, Command }`
  - `struct CommandRecord { id, conversation_id, command, cwd, check: serde_json::Value, approval: Approval, exit_code: Option<i32>, output: String, truncated: bool, started_at, finished_at: Option<_> }`
  - `enum Approval { Auto, User, UserEdited, Denied }`
  - `insert_command(..) -> i64`, `finish_command(id, exit_code, output, truncated)`, `commands(conv_id) -> Vec<CommandRecord>`

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[test] fn thai_text_round_trips() { /* append "สวัสดี ครับ 🙂" -> messages()[0].content เท่าเดิม */ }
#[test] fn list_orders_by_last_activity() { /* a, b; append to a -> list()[0].id == a.id */ }
#[test] fn search_matches_title_and_content_case_insensitive() { /* "Deploy" title, "kubectl" content -> search("deploy"), search("KUBECTL") พบ */ }
#[test] fn delete_cascades() { /* delete -> messages(id) ว่าง, commands(id) ว่าง */ }
#[test] fn reopen_keeps_data() { /* open(tmp) write, drop, open(tmp) read */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test store` ต้อง FAIL
- [ ] **Step 3:** เขียน schema ใน `migrations.sql` (ตาราง `conversations`, `messages`, `decisions`, `commands` เปิด `PRAGMA foreign_keys=ON` ใช้ `ON DELETE CASCADE` และ `PRAGMA user_version` เพื่อทำ migration) แล้วเขียน API ตาม Interfaces
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): SQLite store for conversations, messages, decisions, commands`

---

### Task 6: Export

**Files:**
- Create: `crates/core/src/export.rs`
- Test: `crates/core/tests/export.rs`

**Interfaces:**
- Consumes: `Store` (Task 5)
- Produces: `fn export_markdown(store: &Store, conv_id: i64) -> Result<String>`, `fn export_json(store: &Store, conv_id: i64) -> Result<String>` และ `fn write_export(path: &Path, content: &str) -> Result<()>` (เขียนเป็น UTF-8 ไม่มี BOM)
- Markdown: `# <title>` ตามด้วยแต่ละข้อความเป็น `## <ผู้ใช้|ผู้ช่วย (<provider>)|คำสั่ง>` + เนื้อหา คำสั่งแสดงเป็น code block `powershell` พร้อม exit code และวิธีอนุมัติ
- JSON: `{ "conversation": {..}, "messages": [..], "commands": [..], "decisions": [..] }`

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[test] fn markdown_contains_thai_and_provider_labels() { /* contains "สวัสดี", "ผู้ช่วย (hermes)", "```powershell" */ }
#[test] fn json_is_valid_and_complete() { /* serde_json::from_str ok, messages.len()==3, commands.len()==1 */ }
#[test] fn export_never_contains_secrets() {
    // เก็บ message ที่มี "sk-ant-api03-XXXX" และ command ที่มี "$env:TYPESAFE_API_KEY='ts_live_YYYY'"
    // ผลต้องไม่มี "sk-ant-api03-XXXX" และ "ts_live_YYYY" แต่ต้องมี "[REDACTED]"
}
#[test] fn writes_to_path_with_spaces_and_thai() { /* tmp/"แชต ทดสอบ"/"a b.md" อ่านกลับได้ตรง */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test export` ต้อง FAIL
- [ ] **Step 3:** เขียน export และ `fn redact_secrets(s: &str) -> Cow<str>` ใช้ regex ตรวจ `sk-ant-[A-Za-z0-9_-]{8,}`, `ts_(live|test)_[A-Za-z0-9]{8,}`, `Bearer\s+[A-Za-z0-9._-]{8,}` และ key ที่แอปเก็บไว้ใน keyring ซึ่งส่งเข้ามาเป็น `&[String]`
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): markdown/json export with secret redaction`

---

### Task 7: แปลงประวัติกลางเป็นรูปแบบของแต่ละตัวคุย

**Files:**
- Create: `crates/core/src/history.rs`
- Test: ใน `history.rs`

**Interfaces:**
- Consumes: `Message`, `Role`, `ProviderId` (Task 1)
- Produces:
  - `fn to_anthropic(history: &[Message]) -> Vec<serde_json::Value>` คืน `[{role:"user"|"assistant", content:String}]` และต้องสลับ user/assistant ได้ถูกต้อง: ข้อความ `CommandResult` และ `Notice` ให้แปลงเป็น user context แล้วรวมข้อความ user ที่ติดกันด้วย `"\n\n"`
  - `fn to_hermes(history: &[Message]) -> (Vec<serde_json::Value>, String)` คืน `(conversation_history ในรูป OpenAI {role, content}, input = ข้อความ user ล่าสุด)`
  - `fn to_cli_prompt(history: &[Message]) -> String` เป็นข้อความเดียวที่มีหัวแต่ละบท `[ผู้ใช้]`, `[ผู้ช่วย: <provider>]`, `[ผลคำสั่ง]` และจบด้วยข้อความ user ล่าสุดภายใต้หัว `[ข้อความปัจจุบัน]`
  - `fn trim_history(history: &[Message], max_chars: usize) -> Vec<Message>` ตัดข้อความเก่าออกก่อน และต้องเก็บข้อความ user ล่าสุดไว้เสมอ ค่าเริ่มต้น `max_chars = 200_000`
- รูปแบบข้อความผลคำสั่ง: `"[คำสั่งที่รันโดย <provider>]\ncwd: ..\n> <command>\nexit: <code>\n<output>"`

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[test] fn anthropic_alternates_and_merges_context() { /* user, assistant(hermes), command_result, user -> roles ["user","assistant","user"] และ content สุดท้ายมีทั้งผลคำสั่งและข้อความ user */ }
#[test] fn anthropic_starts_with_user() { /* history เริ่มด้วย assistant -> แทรก user "(ต่อจากบทสนทนาก่อนหน้า)" */ }
#[test] fn hermes_splits_last_user_as_input() { /* input == ข้อความ user สุดท้าย; history ไม่มีข้อความนั้น */ }
#[test] fn cli_prompt_labels_other_providers() { /* contains "[ผู้ช่วย: hermes]" และ "[ข้อความปัจจุบัน]" */ }
#[test] fn trim_keeps_latest_user() { /* max_chars เล็กกว่าข้อความล่าสุด -> ยังคืนข้อความล่าสุด 1 ข้อความ */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core history` ต้อง FAIL
- [ ] **Step 3:** เขียนฟังก์ชันตาม Interfaces
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): provider-neutral history translation`

---

### Task 8: Command runner (PowerShell)

**Files:**
- Create: `crates/core/src/runner.rs`
- Test: `crates/core/tests/runner.rs` (`#[cfg(windows)]`)

**Interfaces:**
- Produces:
  - `struct RunLimits { timeout: Duration, max_stored: usize, max_for_model: usize }` + `Default` = `120s, 64 * 1024, 16 * 1024`
  - `enum RunChunk { Stdout(String), Stderr(String) }`
  - `struct RunResult { exit_code: Option<i32>, output: String, truncated: bool, timed_out: bool, cancelled: bool }`
  - `async fn run_powershell(command: &str, cwd: &Path, limits: &RunLimits, on_chunk: impl FnMut(RunChunk) + Send, cancel: CancellationToken) -> std::io::Result<RunResult>`
  - `fn for_model(r: &RunResult, max: usize) -> String` (ตัดตรงขอบตัวอักษร UTF-8 และต่อท้ายด้วย `\n[ตัดเหลือ N จาก M ไบต์]` เมื่อถูกตัด)
- วิธีรัน: `powershell.exe -NoProfile -NonInteractive -Command "[Console]::OutputEncoding=[Text.Encoding]::UTF8; <command>"` ตั้ง `CREATE_NO_WINDOW` ใช้ `kill_on_drop(true)` และเมื่อ timeout หรือ cancel ให้ใช้ `taskkill /T /F /PID` เพื่อปิดทั้ง process tree

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[tokio::test] async fn captures_thai_output_and_exit_code() { /* "Write-Output 'สวัสดี'; exit 3" -> output.trim()=="สวัสดี", exit_code==Some(3) */ }
#[tokio::test] async fn runs_in_cwd_with_spaces() { /* tmp/"A B" ; "(Get-Location).Path" -> ends_with("A B") */ }
#[tokio::test] async fn timeout_kills_process_tree() { /* "Start-Sleep 30", timeout 1s -> timed_out, elapsed < 5s */ }
#[tokio::test] async fn cancel_stops() { /* cancel after 200ms -> cancelled */ }
#[tokio::test] async fn large_output_truncated() { /* "1..200000 | % { 'xxxxxxxx' }" -> output.len() <= 64*1024, truncated */ }
#[test] fn for_model_cuts_on_char_boundary() { /* "ก".repeat(10_000), max 100 -> valid UTF-8, ลงท้ายด้วย "ไบต์]" */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test runner` ต้อง FAIL
- [ ] **Step 3:** เขียน runner ตาม Interfaces และอ่าน stdout/stderr พร้อมกันด้วย 2 task
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): PowerShell runner with streaming, timeout, cancel, truncation`

---

### Task 9: CommandService (จุดตรวจเดียว)

**Files:**
- Create: `crates/core/src/command_service.rs`
- Test: `crates/core/tests/command_service.rs`

**Interfaces:**
- Consumes: `CommandGate`, `CommandCheck` (Task 4), `Store`, `Approval` (Task 5), `run_powershell`, `RunLimits`, `RunResult` (Task 8)
- Produces:
  - `struct CommandContext { conversation_id: i64, turn_token: String, cwd: PathBuf, provider: ProviderId, count: Arc<AtomicU32>, max_per_turn: u32, cancel: CancellationToken }`
  - `struct ApprovalRequest { id: String, conversation_id: i64, command: String, cwd: PathBuf, check: CommandCheck, reason: ApprovalReason }` และ `enum ApprovalReason { NeedsReview, LimitReached { count: u32 } }`
  - `enum ApprovalResponse { Run, RunEdited(String), Deny }`
  - `#[async_trait] trait Approver: Send + Sync { async fn request(&self, req: ApprovalRequest) -> ApprovalResponse; }`
  - `#[async_trait] trait EventSink: Send + Sync { fn emit(&self, ev: AppEvent); }` โดย `enum AppEvent { CommandStarted { conversation_id, command_id, command, cwd, approval }, CommandOutput { command_id, chunk: RunChunk }, CommandFinished { command_id, exit_code, timed_out }, CommandDenied { conversation_id, command } }`
  - `struct CommandService { gate, approver: Arc<dyn Approver>, store: Arc<Store>, sink: Arc<dyn EventSink>, limits: RunLimits, approval_timeout: Duration }`
  - `CommandService::authorize(&self, ctx: &CommandContext, command: &str) -> Authorization` โดย `enum Authorization { Approved { command: String, approval: Approval, check: CommandCheck }, Denied }`
  - `CommandService::execute(&self, ctx: &CommandContext, command: &str) -> CommandOutcome` โดย `enum CommandOutcome { Ran { command_id: i64, result: RunResult }, Denied }` และ `execute` = `authorize` + `run_powershell` + บันทึกลง store
- ลำดับใน `authorize`: เพิ่ม `count` ทีละ 1 ถ้าเกิน `max_per_turn` ให้ถาม `LimitReached` ก่อน จากนั้นเรียก `gate.check` ถ้าได้ `AutoRun` ให้ `Approval::Auto` นอกนั้นถาม `NeedsReview` ถ้า `RunEdited(cmd)` ให้ส่ง `cmd` ให้ gate ตรวจอีกครั้งเพื่อบันทึกผล แล้วถือเป็น `UserEdited` ถ้า approver ไม่ตอบภายใน `approval_timeout` (ค่าเริ่มต้น 300 วินาที) ให้ถือเป็น `Deny` ทุกกรณีต้องบันทึก `CommandRecord` แม้ถูก `Denied`

- [ ] **Step 1: เขียน test ที่ fail** โดยใช้ `FakeGate`/`FakeJev`, `ScriptedApprover` และ `VecSink`

```rust
#[tokio::test] async fn auto_run_does_not_ask() { /* gate AutoRun -> approver.calls()==0, Ran, record.approval==Auto */ }
#[tokio::test] async fn ask_user_deny_never_runs() { /* gate AskUser, approver Deny -> Denied, sink has CommandDenied, no CommandStarted */ }
#[tokio::test] async fn edited_command_runs_edited_text() { /* RunEdited("Write-Output ok") -> result.output contains "ok", approval UserEdited */ }
#[tokio::test] async fn approver_timeout_denies() { /* approver ไม่ตอบ, approval_timeout 50ms -> Denied */ }
#[tokio::test] async fn approver_dropped_denies() { /* approver คืน Deny เมื่อ oneshot sender ถูก drop (จำลองปิดแชต) -> Denied */ }
#[tokio::test] async fn limit_reached_asks_even_if_auto() { /* max 2, gate AutoRun, ครั้งที่ 3 -> approver ได้ LimitReached{count:3} */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test command_service` ต้อง FAIL
- [ ] **Step 3:** เขียน `CommandService` ตาม Interfaces
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): single command authorization and execution service`

---

### Task 10: MCP server ในเครื่อง (`run_command`)

**Files:**
- Create: `crates/core/src/mcp_server.rs`
- Test: `crates/core/tests/mcp_server.rs`

**Interfaces:**
- Consumes: `CommandService`, `CommandContext`, `CommandOutcome`, `for_model` (Task 8/9)
- Produces:
  - `struct McpServer` + `McpServer::start(addr: SocketAddr, service: Arc<CommandService>, hermes_key: String) -> Result<McpServerHandle>` (ใช้ rmcp streamable HTTP บน axum)
  - `McpServerHandle::register_turn(ctx: CommandContext) -> TurnGuard` (drop `TurnGuard` เพื่อยกเลิกการลงทะเบียน), `McpServerHandle::url_for_turn(token) -> String` = `http://127.0.0.1:<port>/mcp/t/<token>` และ `McpServerHandle::hermes_url() -> String` = `http://127.0.0.1:<port>/mcp/hermes`
  - `McpServerHandle::set_active_hermes_turn(token: Option<String>)`
  - tool `run_command` (input `{ command: string, cwd?: string }`) คืนข้อความ `for_model(result)` หรือ `"คำสั่งถูกปฏิเสธโดยผู้ใช้หรือระบบตรวจสอบ"` เมื่อ Denied
- route `/mcp/t/{token}`: token ต้องลงทะเบียนอยู่ ไม่อย่างนั้นตอบ 404 ส่วน route `/mcp/hermes` ต้องมี `Authorization: Bearer <hermes_key>` (ค่านี้แอปสุ่มเองแล้วเก็บใน keyring เพื่อให้ผู้ใช้นำไปใส่ใน config ของ Hermes) และใช้ turn ที่ `set_active_hermes_turn` กำหนดไว้ ถ้าไม่มี turn ที่ active ให้ตอบ tool error ทันที
- ถ้าเจอ `cwd` ที่ไม่มีอยู่จริง หรืออยู่นอกโฟลเดอร์ของแชต ให้ใช้ `ctx.cwd` แทน

- [ ] **Step 1: เขียน test ที่ fail** โดยใช้ rmcp client ต่อเข้ามาจริง

```rust
#[tokio::test] async fn lists_only_run_command() { /* tools/list -> ["run_command"] */ }
#[tokio::test] async fn unknown_turn_token_rejected() { /* /mcp/t/nope -> 404 */ }
#[tokio::test] async fn hermes_route_requires_bearer() { /* ไม่มี header -> 401 */ }
#[tokio::test] async fn hermes_without_active_turn_is_tool_error_and_never_runs() { /* sink ไม่มี CommandStarted */ }
#[tokio::test] async fn call_goes_through_command_service() { /* FakeGate AutoRun, "Write-Output hi" -> text contains "hi"; store มี record 1 รายการ */ }
#[tokio::test] async fn dropped_turn_guard_unregisters() { /* drop guard -> 404 */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test mcp_server` ต้อง FAIL
- [ ] **Step 3:** เขียน server ตาม Interfaces (ใช้ rmcp รุ่นล่าสุดที่มี feature `transport-streamable-http-server` และ pin เวอร์ชันใน `Cargo.toml`)
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): local MCP server exposing gated run_command`

---

### Task 11: ChatProvider trait + Anthropic API provider

**Files:**
- Create: `crates/core/src/providers/mod.rs`, `crates/core/src/providers/anthropic.rs`
- Test: `crates/core/tests/provider_anthropic.rs`

**Interfaces:**
- Consumes: `TurnRequest`, `ChatEvent`, `Availability` (Task 1), `to_anthropic`, `trim_history` (Task 7)
- Produces:
  - `#[async_trait] trait ChatProvider: Send + Sync { fn id(&self) -> ProviderId; async fn availability(&self) -> Availability; async fn start(&self, req: TurnRequest) -> Result<Box<dyn ProviderTurn>>; }`
  - `#[async_trait] trait ProviderTurn: Send { async fn next(&mut self) -> Option<ChatEvent>; async fn submit_command_result(&mut self, call_id: &str, text: String) -> Result<()>; async fn cancel(&mut self); }` (`submit_command_result` ใช้เฉพาะ provider ที่ส่ง `CommandProposed` ตัวอื่นให้คืน `Ok(())`)
  - `struct AnthropicProvider { base_url, api_key: Option<String>, model: String }` เมื่อไม่มี key ให้ `availability` คืน `Unavailable("ยังไม่ได้ตั้ง API key")`
- request: `POST {base}/v1/messages` header `x-api-key`, `anthropic-version: 2023-06-01`, body `{model, max_tokens: 8192, stream: true, system, messages, tools:[run_command]}` โดย tool `run_command` มี schema `{command: string, cwd?: string}`
- streaming: `content_block_delta` + `text_delta` ส่งออกเป็น `TextDelta` ส่วน `tool_use` ที่ประกอบจาก `input_json_delta` ครบแล้วส่งออกเป็น `CommandProposed { call_id = tool_use.id }` เมื่อ `stop_reason == "tool_use"` ให้รอ `submit_command_result` ทุก call แล้วส่ง request ต่อ ข้อความ native ของ turn ที่กำลังทำอยู่ให้เก็บไว้ในตัว `ProviderTurn` และเมื่อ `stop_reason == "end_turn"` ให้ส่ง `Done`

- [ ] **Step 1: เขียน test ที่ fail** โดย mock ด้วย wiremock ที่ตอบเป็น SSE

```rust
#[tokio::test] async fn streams_text_then_done() { /* deltas "สวัส","ดี" -> [TextDelta("สวัส"),TextDelta("ดี"),Done] */ }
#[tokio::test] async fn tool_use_round_trip() { /* รอบแรก stop tool_use -> CommandProposed{command:"dir"}; submit -> request ที่ 2 มี tool_result content ตรงกัน; รอบ 2 end_turn -> Done */ }
#[tokio::test] async fn http_401_yields_error_event() { /* -> ChatEvent::Error contains "401" */ }
#[tokio::test] async fn no_key_is_unavailable() {}
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test provider_anthropic` ต้อง FAIL
- [ ] **Step 3:** เขียน provider ตาม Interfaces
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): ChatProvider trait and Anthropic streaming provider`

---

### Task 12: Claude CLI provider

**Files:**
- Create: `crates/core/src/providers/claude_cli.rs`, `crates/core/tests/fixtures/claude_stream.jsonl`
- Test: `crates/core/tests/provider_claude_cli.rs`

**Interfaces:**
- Consumes: `ChatProvider`, `ProviderTurn` (Task 11), `to_cli_prompt`, `trim_history` (Task 7), `McpServerHandle::{register_turn,url_for_turn}` (Task 10)
- Produces:
  - `struct ClaudeCliProvider { exe: PathBuf, mcp: McpServerHandle, model: Option<String> }`
  - `fn build_args(mcp_config_path: &Path, model: Option<&str>) -> Vec<String>`
  - `fn parse_stream_line(line: &str) -> Option<ChatEvent>`
- args (ต้องตรวจกับ `claude --help` ของเวอร์ชันที่ติดตั้ง ตอนนี้ 2.1.302): `-p --output-format stream-json --verbose --include-partial-messages --no-session-persistence --strict-mcp-config --mcp-config <tmp.json> --tools "" --allowedTools mcp__assistant__run_command` และ `--model <m>` ถ้ามีการตั้งค่า
- ไฟล์ `tmp.json` = `{"mcpServers":{"assistant":{"type":"http","url":"<url_for_turn>"}}}` เขียนไว้ใน temp dir และลบเมื่อจบ turn ส่วน prompt ส่งทาง stdin
- `availability`: หา `claude` ใน PATH ด้วย `which::which` แล้วรัน `claude --version` ต้องสำเร็จภายใน 5 วินาที
- parse: event `stream_event` ที่เป็น `content_block_delta`/`text_delta` แปลงเป็น `TextDelta` ส่วน `result` แปลงเป็น `Done` และถ้า `is_error` เป็น true ให้แปลงเป็น `Error(result)` provider นี้ไม่ส่ง `CommandProposed` เพราะคำสั่งจะเข้ามาทาง MCP ของเราเอง

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[test] fn args_disable_builtin_tools_and_allow_only_our_mcp() {
    let a = build_args(Path::new("C:\\t\\m.json"), None);
    for must in ["-p","--strict-mcp-config","--no-session-persistence","--allowedTools","mcp__assistant__run_command"] { assert!(a.contains(&must.to_string())); }
    let i = a.iter().position(|x| x=="--tools").unwrap(); assert_eq!(a[i+1], "");
    assert!(!a.iter().any(|x| x.contains("dangerously") || x=="bypassPermissions"));
}
#[test] fn parses_fixture_stream() { /* fixture -> TextDelta ต่อกันได้ข้อความถูกต้อง แล้วจบด้วย Done */ }
#[test] fn error_result_maps_to_error() {}
#[test] fn unknown_lines_ignored() { /* "{\"type\":\"system\"}" -> None, "not json" -> None */ }
```

- [ ] **Step 2:** สร้าง fixture จากการรันจริงหนึ่งครั้งด้วย `echo hi | claude -p --output-format stream-json --verbose --include-partial-messages --tools "" > crates/core/tests/fixtures/claude_stream.jsonl` แล้วรัน test ต้อง FAIL
- [ ] **Step 3:** เขียน provider ตาม Interfaces เมื่อ `cancel()` ให้ kill process tree และ drop `TurnGuard`
- [ ] **Step 4:** รัน `cargo test -p assistant-core --test provider_claude_cli` ต้อง PASS
- [ ] **Step 5:** ทดสอบด้วยมือ: เริ่ม MCP server ของ Task 10 แล้วรัน `claude` ด้วย args ข้างบน ขอให้ "รัน dir" ต้องเห็นการเรียก `run_command` ผ่าน server และ Claude ต้องไม่มี Bash tool ของตัวเอง ถ้า flag ใดใช้ไม่ได้ ให้แก้ `build_args` และ test ให้ตรงกับเวอร์ชันที่ติดตั้ง
- [ ] **Step 6:** Commit `feat(core): Claude CLI provider routed through local MCP`

---

### Task 13: Hermes Agent provider

**Files:**
- Create: `crates/core/src/providers/hermes.rs`, `crates/core/tests/fixtures/hermes_run_events.txt`
- Test: `crates/core/tests/provider_hermes.rs`

**Interfaces:**
- Consumes: `ChatProvider`, `ProviderTurn` (Task 11), `to_hermes`, `trim_history` (Task 7), `McpServerHandle::set_active_hermes_turn` (Task 10), `CommandService::authorize`, `CommandContext` (Task 9)
- Produces:
  - `struct HermesProvider { base_url: Url, api_key: Option<String>, mcp: McpServerHandle, commands: Arc<CommandService>, forbidden_toolsets: Vec<String> }`
  - `fn parse_sse(buf: &str) -> Vec<HermesEvent>` (ข้ามบรรทัดที่ขึ้นต้นด้วย `:`) โดย `enum HermesEvent { Delta(String), ApprovalRequest { request_id: String, execution_generation: u64, command: String }, Completed, Interrupted, Other }`
  - `async fn check_toolsets(&self) -> Result<(), String>` จะคืน Err พร้อมชื่อ toolset เมื่อ toolset ใน `forbidden_toolsets` ยังเปิดอยู่ ให้ตรวจชื่อ toolset ที่แน่นอนจาก `/docs/reference/toolsets-reference` แล้วตั้งค่าเริ่มต้นใน `Settings` (Task 14)
- `availability`: เรียก `GET /health` ภายใน 3 วินาที ถ้าไม่ตอบ ให้คืน `Unavailable("Hermes ไม่ได้เปิด API server")` ถ้าตอบแต่ `check_toolsets` ไม่ผ่าน ให้คืน `Unavailable("ต้องปิด toolset: ..")`
- turn: ใช้ semaphore ให้รัน Hermes ได้ครั้งละหนึ่ง turn แล้วเรียก `set_active_hermes_turn(Some(token))` จากนั้น `POST /v1/runs {input, conversation_history, session_id: "conv-<id>"}` และอ่าน `/v1/runs/{id}/events` เมื่อเจอ `ApprovalRequest` ให้เรียก `commands.authorize` แล้ว `POST /v1/runs/{id}/approval {request_id, execution_generation, choice: "once"|"deny"}` (แบบ Approved ที่ผู้ใช้แก้คำสั่งให้ตอบ `deny` เพราะ Hermes จะรันคำสั่งเดิม) เมื่อ `cancel` ให้เรียก `POST /v1/runs/{id}/stop` และเมื่อจบ turn ให้ `set_active_hermes_turn(None)`

- [ ] **Step 1: เขียน test ที่ fail** โดย mock ด้วย wiremock

```rust
#[test] fn sse_skips_keepalive_and_parses_events() { /* fixture มี ": keepalive" -> ไม่มี event ว่าง; Delta/ApprovalRequest/Completed ครบ */ }
#[tokio::test] async fn approval_request_goes_through_authorize() { /* FakeGate AskUser + approver Deny -> POST approval body choice=="deny" */ }
#[tokio::test] async fn edited_approval_answers_deny() { /* RunEdited -> choice "deny" */ }
#[tokio::test] async fn health_down_is_unavailable() {}
#[tokio::test] async fn forbidden_toolset_enabled_is_unavailable() { /* /v1/toolsets ตอบว่า terminal enabled -> Unavailable contains "terminal" */ }
#[tokio::test] async fn active_turn_cleared_after_done_and_on_error() {}
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test provider_hermes` ต้อง FAIL
- [ ] **Step 3:** เขียน provider ตาม Interfaces ต้องตรวจรูปแบบ JSON ของ `/v1/toolsets` และ `approval.request` กับ Hermes ที่รันอยู่จริง แล้วอัปเดต fixture ให้ตรงกับของจริง
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): Hermes Agent provider via runs API with gated approvals`

---

### Task 14: Settings และ SecretStore

**Files:**
- Create: `crates/core/src/settings.rs`
- Test: ใน `settings.rs`

**Interfaces:**
- Produces:
  - `struct Settings { thresholds: Thresholds, max_commands_per_turn: u32 (=20), approval_timeout_secs: u64 (=300), auto_route: bool (=true), default_provider: Option<ProviderId>, provider_descriptions: HashMap<ProviderId, String>, anthropic_model: String (="claude-opus-5-5"), claude_model: Option<String>, hermes_base_url: String (="http://127.0.0.1:8642/v1"), hermes_forbidden_toolsets: Vec<String>, mcp_port: u16 (=8765), run_timeout_secs: u64 (=120) }`
  - `Settings::load(path) -> Settings` (ถ้าไฟล์ไม่มีหรือเสีย ให้ใช้ค่าเริ่มต้นแล้วบันทึกไฟล์ที่เสียเป็น `.bak`) และ `Settings::save(&self, path)`
  - `Settings::validate(&self) -> Result<(), String>` (threshold ต้องอยู่ใน 0..=1 และ `max_commands_per_turn >= 1`)
  - `enum SecretKey { Jev, Anthropic, HermesApi, McpHermes }` และ `trait SecretStore { fn get(&self, k: SecretKey) -> Option<String>; fn set(&self, k, v: &str) -> Result<()>; fn delete(&self, k) -> Result<()>; }` มี `KeyringStore` (service `com.local.assistant`) และ `MemorySecrets` ไว้ทดสอบ
- `provider_descriptions` ค่าเริ่มต้น: Hermes = "งานหลายขั้นตอน ใช้เครื่องมือ ค้นเว็บ จำบริบทระยะยาว", ClaudeCli = "งานเขียนโค้ด แก้ไฟล์ในโปรเจกต์", Api = "คำถามทั่วไป ตอบเร็ว ไม่ต้องใช้เครื่องมือ"

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[test] fn defaults_match_spec() { let s = Settings::default(); assert_eq!(s.thresholds.route, 0.75); assert_eq!(s.thresholds.command_safe, 0.9); assert_eq!(s.max_commands_per_turn, 20); assert_eq!(s.anthropic_model, "claude-opus-5-5"); }
#[test] fn settings_file_never_contains_secrets() { /* save -> ข้อความในไฟล์ไม่มี "key"/"secret" ที่มีค่า; struct ไม่มี field key */ }
#[test] fn corrupt_file_falls_back_and_backs_up() {}
#[test] fn validate_rejects_out_of_range() { /* route 1.2 -> Err */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core settings` ต้อง FAIL
- [ ] **Step 3:** เขียนตาม Interfaces
- [ ] **Step 4:** รัน test ต้อง PASS
- [ ] **Step 5:** Commit `feat(core): settings file and keyring-backed secrets`

---

### Task 15: Orchestrator (หนึ่ง turn)

**Files:**
- Create: `crates/core/src/orchestrator.rs`
- Test: `crates/core/tests/orchestrator.rs`

**Interfaces:**
- Consumes: Router (Task 4), Store (5), CommandService/AppEvent/EventSink (9), McpServerHandle (10), ChatProvider (11–13), Settings (14)
- Produces:
  - `struct Orchestrator { store, router, providers: HashMap<ProviderId, Arc<dyn ChatProvider>>, commands: Arc<CommandService>, sink: Arc<dyn EventSink>, settings: Arc<RwLock<Settings>> }`
  - `async fn send(&self, conv_id: i64, text: String, choice: ProviderChoice) -> Result<TurnOutcome>` โดย `enum ProviderChoice { Auto, Manual(ProviderId) }` และ `enum TurnOutcome { Completed { provider: ProviderId }, NeedsProviderChoice { user_message_id: i64, ranked: Vec<(ProviderId, f64)> }, Failed { provider: ProviderId, error: String } }`
  - `async fn continue_with(&self, conv_id: i64, user_message_id: i64, provider: ProviderId) -> Result<TurnOutcome>` ใช้ต่อหลังผู้ใช้เลือกตัวคุย โดยไม่บันทึกข้อความผู้ใช้ซ้ำ
  - `fn cancel(&self, conv_id: i64)`
  - เพิ่ม `AppEvent::{RouteDecided { conversation_id, provider, source, probability }, AssistantDelta { conversation_id, text }, TurnFinished { conversation_id }, TurnFailed { conversation_id, provider, error }}`
- ลำดับใน `send`:
  1. บันทึกข้อความผู้ใช้
  2. หาตัวคุยที่ `availability() == Ready`
  3. ถ้า `Manual` หรือ `auto_route == false` ให้ใช้ตัวที่เลือก ถ้าไม่ใช่ ให้เรียก `router.decide` แล้วบันทึก decision
  4. ถ้าผล route เป็น `AskUser` ให้คืน `NeedsProviderChoice`
  5. สร้าง `CommandContext` (นับจาก 0)
  6. `provider.start` แล้ววนอ่าน `next()` ถ้าเป็น `TextDelta` ให้ส่ง event และต่อข้อความไว้ ถ้าเป็น `CommandProposed` ให้เรียก `commands.execute` แล้วบันทึก `CommandResult` และ `submit_command_result(for_model)` ถ้าเป็น `Error` ให้ส่ง `TurnFailed` และบันทึก `Notice`
  7. บันทึกข้อความ assistant พร้อม provider

- [ ] **Step 1: เขียน test ที่ fail** โดยใช้ `ScriptedProvider` ที่ส่ง event ตามสคริปต์

```rust
#[tokio::test] async fn auto_route_low_confidence_returns_choice_and_keeps_message() { /* NeedsProviderChoice; messages() มีข้อความผู้ใช้ 1 */ }
#[tokio::test] async fn continue_with_does_not_duplicate_user_message() {}
#[tokio::test] async fn chosen_provider_unavailable_fails_gracefully() { /* Manual(Hermes) Unavailable -> Failed; ข้อความผู้ใช้ยังอยู่; continue_with(Api) สำเร็จ */ }
#[tokio::test] async fn provider_error_mid_stream_keeps_partial_and_notice() {}
#[tokio::test] async fn command_proposed_runs_through_service_and_result_is_stored() { /* store มี CommandResult message ตามด้วย assistant message */ }
#[tokio::test] async fn switching_providers_passes_full_history() { /* ข้อความ 1 -> Api, ข้อความ 2 -> Manual(Hermes): ScriptedProvider(Hermes).last_request().history มีคำตอบของ Api */ }
#[tokio::test] async fn manual_mode_skips_jev() { /* auto_route false -> FakeJev.calls()==0 */ }
```

- [ ] **Step 2:** รัน `cargo test -p assistant-core --test orchestrator` ต้อง FAIL
- [ ] **Step 3:** เขียน `Orchestrator` ตาม Interfaces
- [ ] **Step 4:** รัน `cargo test -p assistant-core` ทั้งหมดต้อง PASS
- [ ] **Step 5:** Commit `feat(core): turn orchestrator with per-message routing`

---

### Task 16: แอป Tauri และ UI

**Files:**
- Create: `src-tauri/` (`Cargo.toml`, `tauri.conf.json`, `src/main.rs`, `src/commands.rs`, `src/approver.rs`, `src/sink.rs`, `capabilities/default.json`), `ui/` (Vite + Svelte 5 + TS): `src/lib/api.ts`, `src/lib/state.ts`, `src/routes/Chat.svelte`, `Sidebar.svelte`, `Settings.svelte`, `ApprovalDialog.svelte`, `ProviderPicker.svelte`
- Test: `src-tauri/src/approver.rs` (`#[cfg(test)]`), `ui/src/lib/state.test.ts`

**Interfaces:**
- Consumes: ทุกอย่างใน `assistant-core`
- Produces:
  - Tauri commands: `list_conversations`, `create_conversation(title, cwd)`, `rename_conversation`, `delete_conversation`, `search_conversations(q)`, `get_messages(id)`, `send_message(conversation_id, text, provider: Option<String>)`, `continue_with(conversation_id, user_message_id, provider)`, `cancel_turn(conversation_id)`, `answer_approval(id, response: "run" | "deny" | {edited: string})`, `get_settings`, `save_settings`, `set_secret(key, value)`, `has_secret(key) -> bool`, `provider_status() -> {hermes, claude_cli, api}`, `export_conversation(id, format: "md"|"json")` (เปิด dialog บันทึกไฟล์ของระบบ), `hermes_setup_snippet() -> String`
  - Events: `app://event` ส่ง payload `AppEvent` เป็น JSON tagged ด้วย `kind`, และ `app://approval` ส่ง payload `ApprovalRequest`
  - `TauriApprover` implement `Approver`: เก็บ `oneshot::Sender` ตาม id ใน map แล้ว emit `app://approval` ถ้า sender ถูก drop (แชตถูกปิดหรือลบ หรือแอปปิด) ให้ถือเป็น `Deny`
  - `ui/src/lib/state.ts`: `applyEvent(state: ChatState, ev: AppEvent): ChatState` (ฟังก์ชันบริสุทธิ์)
- UI ที่ต้องมี: แถบรายการแชต (ค้นหา เปลี่ยนชื่อ ลบ), หน้าแชต (ข้อความแบบ streaming, ป้ายบอกตัวคุยและความมั่นใจของ Jev ทุกข้อความ, กล่องคำสั่งที่มี output แบบ streaming, ปุ่มหยุด), ตัวเลือกตัวคุย (Auto/Hermes/Claude CLI/API), กล่องอนุมัติ (แสดงคำสั่ง, cwd, ผล `is_safe`/`risk` และเหตุผล พร้อมปุ่มรัน แก้ไข ปฏิเสธ), หน้าตั้งค่า (key ทุกตัวแสดงเป็น "ตั้งค่าแล้ว" โดยไม่แสดงค่า, threshold, จำนวนคำสั่งต่อ turn, toggle auto-route, คำอธิบายตัวคุย, สถานะของแต่ละตัวคุย, snippet สำหรับใส่ใน config ของ Hermes และคำเตือนเรื่องการส่งข้อมูลไป TypeSafe) ดีไซน์ต้องเป็นของตัวเอง
- `tauri.conf.json`: productName `Assistant`, identifier `com.local.assistant`, CSP ห้ามโหลด script จากภายนอก และ capabilities เปิดเฉพาะ `dialog:allow-save` กับ core events

- [ ] **Step 1: เขียน test ที่ fail**

```rust
#[tokio::test] async fn approver_drop_is_deny() { /* request แล้ว drop pending map -> Deny */ }
#[tokio::test] async fn approver_answer_routes_by_id() { /* 2 requests, answer id2 Run -> req2 Run, req1 ยังรอ */ }
```
```ts
test("deltas append to the streaming assistant message", () => { /* AssistantDelta x2 -> last message text concatenated */ });
test("CommandOutput attaches to its command block", () => {});
test("TurnFailed shows notice and clears streaming flag", () => {});
```

- [ ] **Step 2:** รัน `cargo test -p assistant-app` และ `npm --prefix ui test` ทั้งสองต้อง FAIL
- [ ] **Step 3:** scaffold ด้วย `npm create tauri-app@latest` (เลือก Svelte + TS) แล้วย้ายไฟล์เข้า `src-tauri/` กับ `ui/` เขียน commands, approver และ sink ใน `main.rs` ให้ setup `Store::open(app_data_dir/"assistant.db")`, `Settings::load`, `KeyringStore` และสุ่ม `McpHermes` key ถ้ายังไม่มี จากนั้น `McpServer::start(127.0.0.1:mcp_port)` และสร้าง `Orchestrator`
- [ ] **Step 4:** เขียน UI ตามรายการข้างบน
- [ ] **Step 5:** รัน test ทั้งสองชุดต้อง PASS
- [ ] **Step 6:** Smoke test ด้วยมือใน `cargo tauri dev`:
  1. ตั้ง Anthropic key แล้วส่ง "สวัสดี" ต้องได้คำตอบแบบ streaming และป้ายบอกตัวคุย
  2. ขอให้ "แสดงไฟล์ในโฟลเดอร์นี้" คำสั่ง read-only ต้องรันเอง หรือมีกล่องอนุมัติขึ้นมาถ้า Jev ไม่มั่นใจ
  3. ขอให้ "ลบโฟลเดอร์ X" ต้องมีกล่องอนุมัติ และเมื่อกดปฏิเสธต้องไม่มีอะไรถูกลบ
  4. สลับไป Claude CLI ในแชตเดิม ต้องเห็นว่าจำบริบทเดิมได้ และคำสั่งต้องผ่านกล่องอนุมัติเดียวกัน
  5. ปิด Hermes แล้วเลือก Hermes ต้องเห็น error และข้อความยังอยู่
  6. ลบ Jev key แล้วส่งข้อความ ต้องมีตัวเลือกตัวคุยขึ้นมา และคำสั่งทุกคำสั่งต้องถามผู้ใช้
  7. Export เป็น md และ json ไปยัง path ที่มีภาษาไทยและช่องว่าง แล้วเปิดดู ภาษาไทยต้องถูกต้องและไม่มี key
- [ ] **Step 7:** Commit `feat(app): Tauri shell and Svelte UI`
