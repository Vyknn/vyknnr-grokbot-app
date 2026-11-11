use crate::types::{Message, ProviderId, Role};
use serde_json::{json, Value};

pub const DEFAULT_MAX_HISTORY_CHARS: usize = 200_000;
const FILLER_USER: &str = "(ต่อจากบทสนทนาก่อนหน้า)";

/// Format a command result message exactly as the history layer expects.
pub fn format_command_result(
    provider: ProviderId,
    cwd: &str,
    command: &str,
    exit_code: Option<i32>,
    output: &str,
) -> String {
    let exit = exit_code.map_or_else(|| "-".to_string(), |c| c.to_string());
    format!(
        "[คำสั่งที่รันโดย {}]\ncwd: {}\n> {}\nexit: {}\n{}",
        provider.as_str(),
        cwd,
        command,
        exit,
        output
    )
}

/// History entry for a command that was denied (never ran).
pub fn format_denied_command(command: &str) -> String {
    format!("[คำสั่งถูกปฏิเสธ] > {command}")
}

/// Translate to (role, content) pairs; adjacent same-role entries merged.
fn translate(history: &[Message]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for m in history {
        if m.content.is_empty() {
            continue;
        }
        let (role, content) = match m.role {
            Role::User | Role::CommandResult => ("user", m.content.clone()),
            Role::Assistant => ("assistant", m.content.clone()),
            Role::Notice => ("user", format!("[หมายเหตุ] {}", m.content)),
        };
        match out.last_mut() {
            Some((r, c)) if *r == role => {
                c.push_str("\n\n");
                c.push_str(&content);
            }
            _ => out.push((role, content)),
        }
    }
    out
}

fn to_values(pairs: Vec<(&'static str, String)>) -> Vec<Value> {
    pairs
        .into_iter()
        .map(|(r, c)| json!({"role": r, "content": c}))
        .collect()
}

pub fn to_anthropic(history: &[Message]) -> Vec<Value> {
    let mut pairs = translate(history);
    if matches!(pairs.first(), Some(("assistant", _))) {
        pairs.insert(0, ("user", FILLER_USER.to_string()));
    }
    to_values(pairs)
}

pub fn to_hermes(history: &[Message]) -> (Vec<Value>, String) {
    let Some(idx) = history.iter().rposition(|m| m.role == Role::User) else {
        return (to_values(translate(history)), String::new());
    };
    let hist = to_values(translate(&history[..idx]));
    let mut input = history[idx].content.clone();
    for m in &history[idx + 1..] {
        if m.content.is_empty() {
            continue;
        }
        let extra = match m.role {
            Role::Notice => format!("[หมายเหตุ] {}", m.content),
            _ => m.content.clone(),
        };
        input.push_str("\n\n");
        input.push_str(&extra);
    }
    (hist, input)
}

pub fn to_cli_prompt(history: &[Message]) -> String {
    let last_user = history.iter().rposition(|m| m.role == Role::User);
    let mut blocks: Vec<String> = Vec::new();
    for (i, m) in history.iter().enumerate() {
        if Some(i) == last_user {
            continue;
        }
        let header = match m.role {
            Role::User => "[ผู้ใช้]".to_string(),
            Role::Assistant => match m.provider {
                Some(p) => format!("[ผู้ช่วย: {}]", p.as_str()),
                None => "[ผู้ช่วย]".to_string(),
            },
            Role::CommandResult => "[ผลคำสั่ง]".to_string(),
            Role::Notice => "[หมายเหตุ]".to_string(),
        };
        blocks.push(format!("{header}\n{}", m.content));
    }
    if let Some(i) = last_user {
        blocks.push(format!("[ข้อความปัจจุบัน]\n{}", history[i].content));
    }
    blocks.join("\n\n")
}

pub fn trim_history(history: &[Message], max_chars: usize) -> Vec<Message> {
    let floor = history
        .iter()
        .rposition(|m| m.role == Role::User)
        .unwrap_or(history.len());
    let mut total: usize = history[floor..]
        .iter()
        .map(|m| m.content.chars().count())
        .sum();
    let mut start = floor;
    while start > 0 {
        let n = history[start - 1].content.chars().count();
        if total + n > max_chars {
            break;
        }
        total += n;
        start -= 1;
    }
    history[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn msg(role: Role, provider: Option<ProviderId>, content: &str) -> Message {
        Message {
            id: 0,
            conversation_id: 1,
            role,
            provider,
            content: content.to_string(),
            created_at: Utc::now(),
        }
    }
    fn roles(v: &[Value]) -> Vec<&str> {
        v.iter().map(|x| x["role"].as_str().unwrap()).collect()
    }

    #[test]
    fn anthropic_alternates_and_merges_context() {
        let cmd = format_command_result(ProviderId::Hermes, "C:\\x", "dir", Some(0), "ok");
        let h = vec![
            msg(Role::User, None, "hi"),
            msg(Role::Assistant, Some(ProviderId::Hermes), "running"),
            msg(Role::CommandResult, Some(ProviderId::Hermes), &cmd),
            msg(Role::User, None, "next"),
        ];
        let v = to_anthropic(&h);
        assert_eq!(roles(&v), ["user", "assistant", "user"]);
        let last = v[2]["content"].as_str().unwrap();
        assert_eq!(last, format!("{cmd}\n\nnext"));
    }

    #[test]
    fn anthropic_notice_prefix_skip_empty_and_empty_history() {
        assert!(to_anthropic(&[]).is_empty());
        let h = vec![
            msg(Role::Notice, None, "สลับ"),
            msg(Role::Assistant, None, ""),
            msg(Role::User, None, "q"),
        ];
        let v = to_anthropic(&h);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0]["content"], "[หมายเหตุ] สลับ\n\nq");
    }

    #[test]
    fn anthropic_starts_with_user() {
        let h = vec![
            msg(Role::Assistant, Some(ProviderId::Api), "a"),
            msg(Role::User, None, "u"),
        ];
        let v = to_anthropic(&h);
        assert_eq!(roles(&v), ["user", "assistant", "user"]);
        assert_eq!(v[0]["content"], "(ต่อจากบทสนทนาก่อนหน้า)");
    }

    #[test]
    fn hermes_splits_last_user_as_input() {
        let h = vec![
            msg(Role::User, None, "first"),
            msg(Role::Assistant, Some(ProviderId::Api), "a"),
            msg(Role::User, None, "last"),
        ];
        let (hist, input) = to_hermes(&h);
        assert_eq!(input, "last");
        assert_eq!(roles(&hist), ["user", "assistant"]);
        assert!(hist.iter().all(|x| x["content"] != "last"));
        let (hist, input) = to_hermes(&[msg(Role::Assistant, None, "a")]);
        assert_eq!(input, "");
        assert_eq!(hist.len(), 1);
    }

    #[test]
    fn cli_prompt_labels_other_providers() {
        let h = vec![
            msg(Role::User, None, "one"),
            msg(Role::Assistant, Some(ProviderId::Hermes), "two"),
            msg(Role::CommandResult, Some(ProviderId::Hermes), "res"),
            msg(Role::Notice, None, "note"),
            msg(Role::Assistant, None, "anon"),
            msg(Role::User, None, "now"),
        ];
        let p = to_cli_prompt(&h);
        assert!(p.contains("[ผู้ช่วย: hermes]\ntwo"));
        assert!(p.contains("[ผู้ใช้]\none"));
        assert!(p.contains("[ผลคำสั่ง]\nres"));
        assert!(p.contains("[หมายเหตุ]\nnote"));
        assert!(p.contains("[ผู้ช่วย]\nanon"));
        assert!(p.ends_with("[ข้อความปัจจุบัน]\nnow"));
        assert_eq!(p.matches("now").count(), 1);
    }

    #[test]
    fn trim_keeps_latest_user() {
        let h = vec![
            msg(Role::User, None, "old old"),
            msg(Role::Assistant, None, "reply"),
            msg(Role::User, None, "a very long latest message"),
        ];
        let t = trim_history(&h, 3);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].content, "a very long latest message");
    }

    #[test]
    fn trim_drops_oldest_and_counts_chars() {
        let h = vec![
            msg(Role::User, None, "กขค"),
            msg(Role::Assistant, None, "งจ"),
            msg(Role::User, None, "ฉ"),
        ];
        assert_eq!(trim_history(&h, 6).len(), 3);
        assert_eq!(trim_history(&h, 5).len(), 2);
        assert_eq!(trim_history(&h, 1).len(), 1);
        assert!(trim_history(&[], 5).is_empty());
    }

    #[test]
    fn format_command_result_exact() {
        let s = format_command_result(ProviderId::ClaudeCli, "C:\\w", "ls", None, "out");
        assert_eq!(s, "[คำสั่งที่รันโดย claude_cli]\ncwd: C:\\w\n> ls\nexit: -\nout");
        let s = format_command_result(ProviderId::Api, "..", "x", Some(2), "");
        assert!(s.contains("exit: 2\n"));
    }
}
