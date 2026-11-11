use crate::{Message, NewMessage, ProviderId, Role};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const MIGRATION_V1: &str = include_str!("migrations.sql");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Route,
    Command,
}

impl DecisionKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Command => "command",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "route" => Ok(Self::Route),
            "command" => Ok(Self::Command),
            o => Err(StoreError::Corrupt(format!("decision kind {o}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    Auto,
    User,
    UserEdited,
    Denied,
}

impl Approval {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::User => "user",
            Self::UserEdited => "user_edited",
            Self::Denied => "denied",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "user" => Ok(Self::User),
            "user_edited" => Ok(Self::UserEdited),
            "denied" => Ok(Self::Denied),
            o => Err(StoreError::Corrupt(format!("approval {o}"))),
        }
    }
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::CommandResult => "command_result",
        Role::Notice => "notice",
    }
}

fn parse_role(s: &str) -> Result<Role> {
    match s {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "command_result" => Ok(Role::CommandResult),
        "notice" => Ok(Role::Notice),
        o => Err(StoreError::Corrupt(format!("role {o}"))),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRecord {
    pub id: i64,
    pub message_id: i64,
    pub kind: DecisionKind,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub command: String,
    pub cwd: PathBuf,
    pub check: serde_json::Value,
    pub approval: Approval,
    pub exit_code: Option<i32>,
    pub output: String,
    pub truncated: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| StoreError::Corrupt(format!("timestamp {s}: {e}")))
}

fn escape_like(q: &str) -> String {
    let mut o = String::with_capacity(q.len());
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

fn conv_from_row(r: &Row) -> Result<Conversation> {
    Ok(Conversation {
        id: r.get(0)?,
        title: r.get(1)?,
        cwd: PathBuf::from(r.get::<_, String>(2)?),
        created_at: parse_ts(&r.get::<_, String>(3)?)?,
        updated_at: parse_ts(&r.get::<_, String>(4)?)?,
    })
}

const CONV_COLS: &str = "id, title, cwd, created_at, updated_at";

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v < 1 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(MIGRATION_V1)?;
            tx.execute_batch("PRAGMA user_version = 1;")?;
            tx.commit()?;
        }
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn create_conversation(&self, title: &str, cwd: &Path) -> Result<Conversation> {
        let ts = now();
        let db = self.db();
        db.execute(
            "INSERT INTO conversations (title, cwd, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![title, cwd.to_string_lossy(), ts],
        )?;
        let t = parse_ts(&ts)?;
        Ok(Conversation {
            id: db.last_insert_rowid(),
            title: title.into(),
            cwd: PathBuf::from(cwd.to_string_lossy().into_owned()),
            created_at: t,
            updated_at: t,
        })
    }

    pub fn list_conversations(&self) -> Result<Vec<Conversation>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {CONV_COLS} FROM conversations ORDER BY updated_at DESC, id DESC"
        ))?;
        let rows = st.query_map([], |r| Ok(conv_from_row(r)))?;
        rows.map(|r| r?).collect()
    }

    pub fn rename_conversation(&self, id: i64, title: &str) -> Result<()> {
        let n = self.db().execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![title, id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn delete_conversation(&self, id: i64) -> Result<()> {
        let n = self
            .db()
            .execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn search(&self, query: &str) -> Result<Vec<Conversation>> {
        let pat = format!("%{}%", escape_like(query));
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {CONV_COLS} FROM conversations c
             WHERE c.title LIKE ?1 ESCAPE '\\'
                OR EXISTS (SELECT 1 FROM messages m
                           WHERE m.conversation_id = c.id AND m.content LIKE ?1 ESCAPE '\\')
             ORDER BY c.updated_at DESC, c.id DESC"
        ))?;
        let rows = st.query_map(params![pat], |r| Ok(conv_from_row(r)))?;
        rows.map(|r| r?).collect()
    }

    pub fn append_message(&self, conv_id: i64, m: NewMessage) -> Result<Message> {
        let ts = now();
        let mut db = self.db();
        let tx = db.transaction()?;
        let n = tx.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![ts, conv_id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        tx.execute(
            "INSERT INTO messages (conversation_id, role, provider, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                conv_id,
                role_str(m.role),
                m.provider.map(|p| p.as_str()),
                m.content,
                ts
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Message {
            id,
            conversation_id: conv_id,
            role: m.role,
            provider: m.provider,
            content: m.content,
            created_at: parse_ts(&ts)?,
        })
    }

    pub fn messages(&self, conv_id: i64) -> Result<Vec<Message>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT id, conversation_id, role, provider, content, created_at
             FROM messages WHERE conversation_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map(params![conv_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, conversation_id, role, provider, content, ts) = r?;
            let provider = provider
                .map(|p| p.parse::<ProviderId>().map_err(StoreError::Corrupt))
                .transpose()?;
            out.push(Message {
                id,
                conversation_id,
                role: parse_role(&role)?,
                provider,
                content,
                created_at: parse_ts(&ts)?,
            });
        }
        Ok(out)
    }

    pub fn record_decision(
        &self,
        message_id: i64,
        kind: DecisionKind,
        payload: &serde_json::Value,
    ) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO decisions (message_id, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                message_id,
                kind.as_str(),
                serde_json::to_string(payload)?,
                now()
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn decisions(&self, conv_id: i64) -> Result<Vec<DecisionRecord>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT d.id, d.message_id, d.kind, d.payload, d.created_at
             FROM decisions d JOIN messages m ON m.id = d.message_id
             WHERE m.conversation_id = ?1 ORDER BY d.id",
        )?;
        let rows = st.query_map(params![conv_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, message_id, kind, payload, ts) = r?;
            out.push(DecisionRecord {
                id,
                message_id,
                kind: DecisionKind::parse(&kind)?,
                payload: serde_json::from_str(&payload)?,
                created_at: parse_ts(&ts)?,
            });
        }
        Ok(out)
    }

    pub fn insert_command(
        &self,
        conversation_id: i64,
        command: &str,
        cwd: &Path,
        check: &serde_json::Value,
        approval: Approval,
    ) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO commands (conversation_id, command, cwd, \"check\", approval, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                conversation_id,
                command,
                cwd.to_string_lossy(),
                serde_json::to_string(check)?,
                approval.as_str(),
                now()
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn finish_command(
        &self,
        id: i64,
        exit_code: Option<i32>,
        output: &str,
        truncated: bool,
    ) -> Result<()> {
        let n = self.db().execute(
            "UPDATE commands SET exit_code = ?1, output = ?2, truncated = ?3, finished_at = ?4
             WHERE id = ?5",
            params![exit_code, output, truncated, now(), id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn commands(&self, conv_id: i64) -> Result<Vec<CommandRecord>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT id, conversation_id, command, cwd, \"check\", approval, exit_code, output,
                    truncated, started_at, finished_at
             FROM commands WHERE conversation_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map(params![conv_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<i32>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, bool>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (
                id,
                conversation_id,
                command,
                cwd,
                check,
                approval,
                exit_code,
                output,
                truncated,
                started,
                finished,
            ) = r?;
            out.push(CommandRecord {
                id,
                conversation_id,
                command,
                cwd: PathBuf::from(cwd),
                check: serde_json::from_str(&check)?,
                approval: Approval::parse(&approval)?,
                exit_code,
                output,
                truncated,
                started_at: parse_ts(&started)?,
                finished_at: finished.map(|f| parse_ts(&f)).transpose()?,
            });
        }
        Ok(out)
    }
}
