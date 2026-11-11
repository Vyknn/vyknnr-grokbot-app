use crate::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub route: f64,
    pub command_safe: f64,
    pub command_risk: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            route: 0.75,
            command_safe: 0.9,
            command_risk: 0.9,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    ReadOnly,
    ModifiesFiles,
    Destructive,
    Network,
    SystemChange,
    Other,
}

impl Risk {
    pub const ALL: [Risk; 6] = [
        Risk::ReadOnly,
        Risk::ModifiesFiles,
        Risk::Destructive,
        Risk::Network,
        Risk::SystemChange,
        Risk::Other,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Risk::ReadOnly => "read_only",
            Risk::ModifiesFiles => "modifies_files",
            Risk::Destructive => "destructive",
            Risk::Network => "network",
            Risk::SystemChange => "system_change",
            Risk::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionSource {
    Jev { model: String },
    Fallback { reason: String },
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteOutcome {
    Auto(ProviderId),
    AskUser { ranked: Vec<(ProviderId, f64)> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandVerdict {
    AutoRun,
    AskUser,
}

/// Finite and within 0..=1.
fn valid(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}

pub fn route_outcome(
    choice: &str,
    probs: &HashMap<String, f64>,
    available: &[ProviderId],
    t: &Thresholds,
) -> RouteOutcome {
    let prob = |id: ProviderId| -> f64 {
        match probs.get(id.as_str()) {
            Some(&x) if valid(x) => x,
            _ => 0.0,
        }
    };
    if let Ok(id) = ProviderId::from_str(choice) {
        if available.contains(&id) {
            if let Some(&x) = probs.get(choice) {
                if valid(x) && x >= t.route {
                    return RouteOutcome::Auto(id);
                }
            }
        }
    }
    let mut ranked: Vec<(ProviderId, f64)> = available.iter().map(|&id| (id, prob(id))).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1)); // stable: ties keep `available` order
    RouteOutcome::AskUser { ranked }
}

pub fn command_outcome(
    is_safe: f64,
    risk: &str,
    risk_probs: &HashMap<String, f64>,
    t: &Thresholds,
) -> CommandVerdict {
    let risk_ok = risk == Risk::ReadOnly.as_str()
        && risk_probs
            .get(risk)
            .is_some_and(|&x| valid(x) && x >= t.command_risk);
    if valid(is_safe) && is_safe >= t.command_safe && risk_ok {
        CommandVerdict::AutoRun
    } else {
        CommandVerdict::AskUser
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProviderId::*;

    const T: fn() -> Thresholds = Thresholds::default;

    fn p(v: &[(&str, f64)]) -> HashMap<String, f64> {
        v.iter().map(|(k, x)| (k.to_string(), *x)).collect()
    }

    #[test]
    fn route_auto_at_threshold() {
        assert_eq!(
            route_outcome(
                "api",
                &p(&[("api", 0.75), ("hermes", 0.25)]),
                &[Api, Hermes],
                &T()
            ),
            RouteOutcome::Auto(Api)
        );
    }

    #[test]
    fn route_below_threshold_asks_ranked() {
        assert_eq!(
            route_outcome(
                "api",
                &p(&[("api", 0.6), ("hermes", 0.4)]),
                &[Hermes, Api],
                &T()
            ),
            RouteOutcome::AskUser {
                ranked: vec![(Api, 0.6), (Hermes, 0.4)]
            }
        );
    }

    #[test]
    fn route_choice_not_available_asks() {
        assert_eq!(
            route_outcome(
                "hermes",
                &p(&[("hermes", 0.95), ("api", 0.05)]),
                &[Api],
                &T()
            ),
            RouteOutcome::AskUser {
                ranked: vec![(Api, 0.05)]
            }
        );
    }

    #[test]
    fn route_unknown_label_asks() {
        assert_eq!(
            route_outcome(
                "grok",
                &p(&[("grok", 0.99), ("api", 0.01)]),
                &[Api, Hermes],
                &T()
            ),
            RouteOutcome::AskUser {
                ranked: vec![(Api, 0.01), (Hermes, 0.0)]
            }
        );
    }

    #[test]
    fn route_missing_prob_is_zero_and_ties_keep_order() {
        assert_eq!(
            route_outcome("api", &p(&[]), &[Hermes, ClaudeCli, Api], &T()),
            RouteOutcome::AskUser {
                ranked: vec![(Hermes, 0.0), (ClaudeCli, 0.0), (Api, 0.0)]
            }
        );
    }

    #[test]
    fn route_nan_and_out_of_range_never_pass() {
        let out = route_outcome(
            "api",
            &p(&[("api", f64::NAN), ("hermes", 1.5)]),
            &[Api, Hermes],
            &T(),
        );
        assert_eq!(
            out,
            RouteOutcome::AskUser {
                ranked: vec![(Api, 0.0), (Hermes, 0.0)]
            }
        );
        let out = route_outcome("hermes", &p(&[("hermes", 1.5)]), &[Hermes], &T());
        assert_eq!(
            out,
            RouteOutcome::AskUser {
                ranked: vec![(Hermes, 0.0)]
            }
        );
    }

    #[test]
    fn command_auto_only_when_safe_and_read_only() {
        assert_eq!(
            command_outcome(0.95, "read_only", &p(&[("read_only", 0.92)]), &T()),
            CommandVerdict::AutoRun
        );
        assert_eq!(
            command_outcome(0.89, "read_only", &p(&[("read_only", 0.99)]), &T()),
            CommandVerdict::AskUser
        );
        assert_eq!(
            command_outcome(0.99, "read_only", &p(&[("read_only", 0.89)]), &T()),
            CommandVerdict::AskUser
        );
        assert_eq!(
            command_outcome(
                0.99,
                "modifies_files",
                &p(&[("modifies_files", 0.99)]),
                &T()
            ),
            CommandVerdict::AskUser
        );
    }

    #[test]
    fn command_nan_asks() {
        assert_eq!(
            command_outcome(f64::NAN, "read_only", &p(&[("read_only", 1.0)]), &T()),
            CommandVerdict::AskUser
        );
        assert_eq!(
            command_outcome(0.99, "read_only", &p(&[("read_only", f64::NAN)]), &T()),
            CommandVerdict::AskUser
        );
        assert_eq!(
            command_outcome(1.5, "read_only", &p(&[("read_only", 1.0)]), &T()),
            CommandVerdict::AskUser
        );
        assert_eq!(
            command_outcome(0.99, "read_only", &p(&[]), &T()),
            CommandVerdict::AskUser
        );
    }

    #[test]
    fn risk_strings_and_serde() {
        let names: Vec<_> = Risk::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            [
                "read_only",
                "modifies_files",
                "destructive",
                "network",
                "system_change",
                "other"
            ]
        );
        for r in Risk::ALL {
            assert_eq!(
                serde_json::to_string(&r).unwrap(),
                format!("\"{}\"", r.as_str())
            );
        }
        let s = serde_json::to_value(DecisionSource::Jev { model: "m".into() }).unwrap();
        assert_eq!(s, serde_json::json!({"kind":"jev","model":"m"}));
        assert_eq!(
            serde_json::to_value(DecisionSource::Manual).unwrap(),
            serde_json::json!({"kind":"manual"})
        );
        assert_eq!(
            Thresholds::default(),
            Thresholds {
                route: 0.75,
                command_safe: 0.9,
                command_risk: 0.9
            }
        );
    }
}
