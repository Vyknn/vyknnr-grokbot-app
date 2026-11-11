use super::policy::{route_outcome, DecisionSource, RouteOutcome, Thresholds};
use crate::jev::{JevApi, JevQuestion};
use crate::{Message, ProviderId, Role};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const RECENT_MAX: usize = 6;
const STATE_MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub id: ProviderId,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub outcome: RouteOutcome,
    pub source: DecisionSource,
    /// Jev's probability for the chosen provider, only when the outcome is
    /// `Auto` decided by Jev.
    pub probability: Option<f64>,
}

pub struct Router {
    jev: Option<Arc<dyn JevApi>>,
    thresholds: Thresholds,
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::CommandResult => "command_result",
        Role::Notice => "notice",
    }
}

fn build_state(user_text: &str, recent: &[Message]) -> String {
    let start = recent.len().saturating_sub(RECENT_MAX);
    let mut s = String::new();
    for m in &recent[start..] {
        s.push_str(role_str(m.role));
        s.push_str(": ");
        s.push_str(&m.content);
        s.push('\n');
    }
    s.push_str("user: ");
    s.push_str(user_text);
    let n = s.chars().count();
    if n > STATE_MAX_CHARS {
        s = s.chars().skip(n - STATE_MAX_CHARS).collect();
    }
    s
}

fn fallback(available: &[ProviderProfile], reason: String) -> RouteDecision {
    RouteDecision {
        outcome: RouteOutcome::AskUser {
            ranked: available.iter().map(|p| (p.id, 0.0)).collect(),
        },
        source: DecisionSource::Fallback { reason },
        probability: None,
    }
}

impl Router {
    pub fn new(jev: Option<Arc<dyn JevApi>>, thresholds: Thresholds) -> Self {
        Self { jev, thresholds }
    }

    pub async fn decide(
        &self,
        user_text: &str,
        recent: &[Message],
        available: &[ProviderProfile],
    ) -> RouteDecision {
        match available {
            [] => {
                return RouteDecision {
                    outcome: RouteOutcome::AskUser { ranked: vec![] },
                    source: DecisionSource::Manual,
                    probability: None,
                }
            }
            [only] => {
                return RouteDecision {
                    outcome: RouteOutcome::Auto(only.id),
                    source: DecisionSource::Manual,
                    probability: None,
                }
            }
            _ => {}
        }
        let Some(jev) = &self.jev else {
            return fallback(available, "jev_not_configured".into());
        };
        let mut qs = IndexMap::new();
        qs.insert(
            "route".to_string(),
            JevQuestion::Choice {
                instructions: "Which assistant should handle the user's latest message?".into(),
                criteria: available
                    .iter()
                    .map(|p| (p.id.as_str().to_string(), p.description.clone()))
                    .collect(),
            },
        );
        let resp = match jev.ask(&build_state(user_text, recent), qs).await {
            Ok(r) => r,
            Err(e) => return fallback(available, format!("jev_error: {e}")),
        };
        match resp.choice("route") {
            Ok((choice, probs)) => {
                let ids: Vec<ProviderId> = available.iter().map(|p| p.id).collect();
                let outcome = route_outcome(choice, probs, &ids, &self.thresholds);
                let probability = match &outcome {
                    RouteOutcome::Auto(p) => probs.get(p.as_str()).copied(),
                    RouteOutcome::AskUser { .. } => None,
                };
                RouteDecision {
                    outcome,
                    source: DecisionSource::Jev {
                        model: resp.model.clone(),
                    },
                    probability,
                }
            }
            Err(e) => fallback(available, format!("jev_error: {e}")),
        }
    }
}
