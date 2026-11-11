use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Hermes,
    ClaudeCli,
    Api,
}

impl ProviderId {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderId::Hermes => "hermes",
            ProviderId::ClaudeCli => "claude_cli",
            ProviderId::Api => "api",
        }
    }
}

impl FromStr for ProviderId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "hermes" => Ok(ProviderId::Hermes),
            "claude_cli" => Ok(ProviderId::ClaudeCli),
            "api" => Ok(ProviderId::Api),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    CommandResult,
    Notice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: i64,
    pub conversation_id: i64,
    pub role: Role,
    pub provider: Option<ProviderId>,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewMessage {
    pub role: Role,
    pub provider: Option<ProviderId>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChatEvent {
    TextDelta(String),
    CommandProposed {
        call_id: String,
        command: String,
        cwd: Option<PathBuf>,
    },
    Done,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnRequest {
    pub conversation_id: i64,
    pub turn_token: String,
    pub history: Vec<Message>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Ready,
    Unavailable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_round_trips_as_snake_case() {
        for (p, s) in [(ProviderId::Hermes,"hermes"),(ProviderId::ClaudeCli,"claude_cli"),(ProviderId::Api,"api")] {
            assert_eq!(p.as_str(), s);
            assert_eq!(s.parse::<ProviderId>().unwrap(), p);
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{s}\""));
        }
        assert!("grok".parse::<ProviderId>().is_err());
    }
}
