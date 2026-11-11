use crate::store::{Store, StoreError};
use crate::Role;
use regex::Regex;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ExportError>;

const MIN_KNOWN_LEN: usize = 8;

fn patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"sk-ant-[A-Za-z0-9_-]{8,}|ts_(?:live|test)_[A-Za-z0-9_-]{8,}|(?i:bearer)\s+[A-Za-z0-9._~+/=-]{8,}",
        )
        .expect("valid regex")
    })
}

/// Replace known secrets and secret-shaped tokens with `[REDACTED]`.
pub fn redact_secrets<'a>(s: &'a str, known: &[String]) -> Cow<'a, str> {
    let mut keys: Vec<&str> = known
        .iter()
        .map(String::as_str)
        .filter(|k| k.len() >= MIN_KNOWN_LEN)
        .collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    let mut cur = Cow::Borrowed(s);
    for k in keys {
        if cur.contains(k) {
            cur = Cow::Owned(cur.replace(k, "[REDACTED]"));
        }
    }
    if let Cow::Owned(o) = patterns().replace_all(&cur, "[REDACTED]") {
        cur = Cow::Owned(o);
    }
    cur
}

fn redact_value(v: &mut Value, known: &[String]) {
    match v {
        Value::String(s) => {
            if let Cow::Owned(o) = redact_secrets(s, known) {
                *s = o;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| redact_value(x, known)),
        Value::Object(m) => m.values_mut().for_each(|x| redact_value(x, known)),
        _ => {}
    }
}

fn fence_for(content: &str) -> String {
    let mut max = 0;
    let mut run = 0;
    for c in content.chars() {
        if c == '`' {
            run += 1;
            max = max.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((max + 1).max(3))
}

fn block(out: &mut String, lang: &str, content: &str) {
    let f = fence_for(content);
    out.push_str(&format!("{f}{lang}\n{content}\n{f}\n\n"));
}

pub fn export_markdown(store: &Store, conv_id: i64, known_secrets: &[String]) -> Result<String> {
    let conv = store.conversation(conv_id)?;
    let messages = store.messages(conv_id)?;
    let commands = store.commands(conv_id)?;
    let r = |s: &str| redact_secrets(s, known_secrets).into_owned();
    let mut out = format!("# {}\n\n", r(&conv.title).replace('\n', " "));
    for m in &messages {
        let label = match m.role {
            Role::User => "ผู้ใช้".to_string(),
            Role::Assistant => match m.provider {
                Some(p) => format!("ผู้ช่วย ({})", p.as_str()),
                None => "ผู้ช่วย".to_string(),
            },
            Role::CommandResult => "ผลคำสั่ง".to_string(),
            Role::Notice => "หมายเหตุ".to_string(),
        };
        out.push_str(&format!("## {label}\n\n"));
        let body = r(&m.content);
        if matches!(m.role, Role::CommandResult) {
            block(&mut out, "", &body);
        } else {
            out.push_str(&body);
            out.push_str("\n\n");
        }
    }
    if !commands.is_empty() {
        out.push_str("## คำสั่ง\n\n");
        for c in &commands {
            block(&mut out, "powershell", &r(&c.command));
            let code = c.exit_code.map_or("-".to_string(), |x| x.to_string());
            let appr = serde_json::to_value(c.approval)?;
            out.push_str(&format!("exit: {code}\n\n"));
            out.push_str(&format!("การอนุมัติ: {}\n\n", appr.as_str().unwrap_or("-")));
            out.push_str(&format!("cwd: {}\n\n", r(&c.cwd.to_string_lossy())));
            if !c.output.is_empty() {
                block(&mut out, "", &r(&c.output));
            }
        }
    }
    Ok(out)
}

pub fn export_json(store: &Store, conv_id: i64, known_secrets: &[String]) -> Result<String> {
    let mut v = json!({
        "conversation": store.conversation(conv_id)?,
        "messages": store.messages(conv_id)?,
        "commands": store.commands(conv_id)?,
        "decisions": store.decisions(conv_id)?,
    });
    redact_value(&mut v, known_secrets);
    Ok(serde_json::to_string_pretty(&v)?)
}

/// Write UTF-8 (no BOM), creating parent directories.
pub fn write_export(path: &Path, content: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    std::fs::write(path, content.as_bytes())?;
    Ok(())
}
