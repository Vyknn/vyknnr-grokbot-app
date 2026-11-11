use super::policy::{command_outcome, CommandVerdict, DecisionSource, Risk, Thresholds};
use crate::jev::{JevApi, JevError, JevQuestion, JevResponse};
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

const IS_SAFE_INSTRUCTIONS: &str =
    "คำสั่งนี้รันได้อย่างปลอดภัยโดยไม่ทำลายข้อมูล ไม่ส่งข้อมูลออกนอกเครื่อง และไม่เปลี่ยนการตั้งค่าระบบหรือไม่";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandCheck {
    pub verdict: CommandVerdict,
    pub is_safe: Option<f64>,
    pub risk: Option<String>,
    pub risk_p: Option<f64>,
    pub source: DecisionSource,
}

impl CommandCheck {
    fn fallback(reason: String) -> Self {
        Self {
            verdict: CommandVerdict::AskUser,
            is_safe: None,
            risk: None,
            risk_p: None,
            source: DecisionSource::Fallback { reason },
        }
    }
}

#[async_trait]
pub trait CommandChecker: Send + Sync {
    async fn check(&self, command: &str, cwd: &Path) -> CommandCheck;
}

pub struct CommandGate {
    jev: Option<Arc<dyn JevApi>>,
    thresholds: Thresholds,
}

fn risk_description(r: Risk) -> &'static str {
    match r {
        Risk::ReadOnly => "Only reads or lists data; changes nothing and sends nothing out",
        Risk::ModifiesFiles => "Creates, edits, moves or overwrites files",
        Risk::Destructive => "Deletes or irreversibly destroys data",
        Risk::Network => "Sends or fetches data over the network",
        Risk::SystemChange => "Changes system settings, registry, services or installed software",
        Risk::Other => "Anything else, or unclear",
    }
}

fn extract(
    resp: &JevResponse,
    t: &Thresholds,
) -> Result<(CommandVerdict, f64, String, f64), JevError> {
    let is_safe = resp.noul("is_safe")?;
    let (risk, probs) = resp.choice("risk")?;
    let verdict = command_outcome(is_safe, risk, probs, t);
    let risk_p = probs.get(risk).copied().unwrap_or(0.0);
    Ok((verdict, is_safe, risk.to_string(), risk_p))
}

impl CommandGate {
    pub fn new(jev: Option<Arc<dyn JevApi>>, thresholds: Thresholds) -> Self {
        Self { jev, thresholds }
    }

    pub async fn check(&self, command: &str, cwd: &Path) -> CommandCheck {
        let Some(jev) = &self.jev else {
            return CommandCheck::fallback("jev_not_configured".into());
        };
        let mut qs = IndexMap::new();
        qs.insert(
            "is_safe".to_string(),
            JevQuestion::Noul {
                instructions: IS_SAFE_INSTRUCTIONS.to_string(),
            },
        );
        qs.insert(
            "risk".to_string(),
            JevQuestion::Choice {
                instructions: "What kind of effect does this command have?".to_string(),
                criteria: Risk::ALL
                    .iter()
                    .map(|r| (r.as_str().to_string(), risk_description(*r).to_string()))
                    .collect(),
            },
        );
        let state = format!("cwd: {}\ncommand: {}", cwd.display(), command);
        let resp = match jev.ask(&state, qs).await {
            Ok(r) => r,
            Err(e) => return CommandCheck::fallback(format!("jev_error: {e}")),
        };
        match extract(&resp, &self.thresholds) {
            Ok((verdict, is_safe, risk, risk_p)) => CommandCheck {
                verdict,
                is_safe: Some(is_safe),
                risk: Some(risk),
                risk_p: Some(risk_p),
                source: DecisionSource::Jev { model: resp.model },
            },
            Err(e) => CommandCheck::fallback(format!("jev_error: {e}")),
        }
    }
}

#[async_trait]
impl CommandChecker for CommandGate {
    async fn check(&self, command: &str, cwd: &Path) -> CommandCheck {
        CommandGate::check(self, command, cwd).await
    }
}
