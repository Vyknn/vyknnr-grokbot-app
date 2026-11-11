//! Typed client for TypeSafe AI's Jev decision model.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum JevQuestion {
    Choice {
        instructions: String,
        criteria: IndexMap<String, String>,
    },
    Noul {
        instructions: String,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum JevAnswer {
    Choice {
        choice: String,
        probabilities: HashMap<String, f64>,
        confidence: f64,
    },
    Noul {
        noul: f64,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JevResponse {
    pub model: String,
    pub answers: HashMap<String, JevAnswer>,
}

impl JevResponse {
    pub fn choice(&self, id: &str) -> Result<(&str, &HashMap<String, f64>), JevError> {
        match self.answers.get(id) {
            Some(JevAnswer::Choice {
                choice,
                probabilities,
                ..
            }) => Ok((choice.as_str(), probabilities)),
            _ => Err(JevError::MissingAnswer(id.to_string())),
        }
    }

    pub fn noul(&self, id: &str) -> Result<f64, JevError> {
        match self.answers.get(id) {
            Some(JevAnswer::Noul { noul }) => Ok(*noul),
            _ => Err(JevError::MissingAnswer(id.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error("jev http error {0}: {1}")]
    Http(u16, String),
    #[error("jev transport error: {0}")]
    Transport(String),
    #[error("jev decode error: {0}")]
    Decode(String),
    #[error("jev missing answer: {0}")]
    MissingAnswer(String),
}

#[async_trait]
pub trait JevApi: Send + Sync {
    async fn ask(
        &self,
        state: &str,
        questions: IndexMap<String, JevQuestion>,
    ) -> Result<JevResponse, JevError>;
}

pub struct JevClient {
    base_url: Url,
    api_key: String,
    http: reqwest::Client,
}

impl JevClient {
    pub fn new(base_url: Url, api_key: String) -> Self {
        Self::with_timeout(base_url, api_key, Duration::from_secs(10))
    }

    pub fn with_timeout(base_url: Url, api_key: String, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build");
        Self {
            base_url,
            api_key,
            http,
        }
    }
}

#[derive(Serialize)]
struct Request<'a> {
    state: &'a str,
    model: &'static str,
    questions: &'a IndexMap<String, JevQuestion>,
}

#[async_trait]
impl JevApi for JevClient {
    async fn ask(
        &self,
        state: &str,
        questions: IndexMap<String, JevQuestion>,
    ) -> Result<JevResponse, JevError> {
        let url = self
            .base_url
            .join("v1/systemone")
            .map_err(|e| JevError::Transport(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&Request {
                state,
                model: "jev-latest",
                questions: &questions,
            })
            .send()
            .await
            .map_err(|e| JevError::Transport(e.without_url().to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| JevError::Transport(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(JevError::Http(status.as_u16(), text));
        }
        serde_json::from_str(&text).map_err(|e| JevError::Decode(e.to_string()))
    }
}
