use crate::decision::policy::Thresholds;
use crate::providers::hermes::DEFAULT_ALLOWED_TOOLSETS;
use crate::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Non-secret app settings. Secrets live in a [`SecretStore`], never here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub thresholds: Thresholds,
    pub max_commands_per_turn: u32,
    pub approval_timeout_secs: u64,
    pub auto_route: bool,
    pub default_provider: Option<ProviderId>,
    pub provider_descriptions: HashMap<ProviderId, String>,
    pub anthropic_model: String,
    pub anthropic_base_url: String,
    pub jev_base_url: String,
    pub claude_model: Option<String>,
    pub hermes_base_url: String,
    pub hermes_allowed_toolsets: Vec<String>,
    pub mcp_port: u16,
    pub run_timeout_secs: u64,
}

fn default_descriptions() -> HashMap<ProviderId, String> {
    HashMap::from([
        (
            ProviderId::Hermes,
            "งานหลายขั้นตอน ใช้เครื่องมือ ค้นเว็บ จำบริบทระยะยาว".to_string(),
        ),
        (
            ProviderId::ClaudeCli,
            "งานเขียนโค้ด แก้ไฟล์ในโปรเจกต์".to_string(),
        ),
        (
            ProviderId::Api,
            "คำถามทั่วไป ตอบเร็ว ไม่ต้องใช้เครื่องมือ".to_string(),
        ),
    ])
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            thresholds: Thresholds::default(),
            max_commands_per_turn: 20,
            approval_timeout_secs: 300,
            auto_route: true,
            default_provider: None,
            provider_descriptions: default_descriptions(),
            anthropic_model: "claude-opus-5-5".into(),
            anthropic_base_url: "https://api.anthropic.com".into(),
            jev_base_url: "https://api.typesafe.ai".into(),
            claude_model: None,
            hermes_base_url: "http://127.0.0.1:8642/v1".into(),
            hermes_allowed_toolsets: DEFAULT_ALLOWED_TOOLSETS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            mcp_port: 8765,
            run_timeout_secs: 120,
        }
    }
}

fn parse_http_url(field: &str, v: &str) -> Result<url::Url, String> {
    let u = url::Url::parse(v).map_err(|_| format!("{field} ไม่ใช่ URL ที่ถูกต้อง"))?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return Err(format!("{field} ต้องใช้ http หรือ https"));
    }
    if u.host().is_none() {
        return Err(format!("{field} ต้องมีชื่อโฮสต์"));
    }
    Ok(u)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

impl Settings {
    pub fn description(&self, id: ProviderId) -> &str {
        self.provider_descriptions
            .get(&id)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn validate(&self) -> Result<(), String> {
        let t = &self.thresholds;
        for (name, v) in [
            ("thresholds.route", t.route),
            ("thresholds.command_safe", t.command_safe),
            ("thresholds.command_risk", t.command_risk),
        ] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(format!("{name} ต้องอยู่ระหว่าง 0 ถึง 1"));
            }
        }
        if self.max_commands_per_turn < 1 {
            return Err("max_commands_per_turn ต้องไม่น้อยกว่า 1".into());
        }
        if !(10..=3600).contains(&self.approval_timeout_secs) {
            return Err("approval_timeout_secs ต้องอยู่ระหว่าง 10 ถึง 3600".into());
        }
        if !(1..=3600).contains(&self.run_timeout_secs) {
            return Err("run_timeout_secs ต้องอยู่ระหว่าง 1 ถึง 3600".into());
        }
        if self.mcp_port == 0 {
            return Err("mcp_port ต้องไม่เป็น 0".into());
        }
        let h = parse_http_url("hermes_base_url", &self.hermes_base_url)?;
        let loopback = match h.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if !loopback {
            return Err("hermes_base_url ต้องเป็นที่อยู่ loopback (127.0.0.1, localhost, ::1)".into());
        }
        parse_http_url("anthropic_base_url", &self.anthropic_base_url)?;
        parse_http_url("jev_base_url", &self.jev_base_url)?;
        Ok(())
    }

    /// Missing file → defaults (nothing written). Corrupt or invalid file →
    /// moved to `<name>.bak` (overwriting any older backup), defaults returned.
    pub fn load(path: &Path) -> Settings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Settings::default(),
            Err(_) => {
                Self::back_up(path);
                return Settings::default();
            }
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut s) if s.validate().is_ok() => {
                for (id, d) in default_descriptions() {
                    s.provider_descriptions.entry(id).or_insert(d);
                }
                s
            }
            _ => {
                Self::back_up(path);
                Settings::default()
            }
        }
    }

    fn back_up(path: &Path) {
        let mut b = path.as_os_str().to_owned();
        b.push(".bak");
        let b = PathBuf::from(b);
        let _ = std::fs::remove_file(&b);
        let _ = std::fs::rename(path, &b);
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = tmp_path(path);
        let res = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path));
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKey {
    Jev,
    Anthropic,
    HermesApi,
    McpHermes,
}

impl SecretKey {
    pub const ALL: [SecretKey; 4] = [
        SecretKey::Jev,
        SecretKey::Anthropic,
        SecretKey::HermesApi,
        SecretKey::McpHermes,
    ];

    pub fn account(&self) -> &'static str {
        match self {
            SecretKey::Jev => "jev",
            SecretKey::Anthropic => "anthropic",
            SecretKey::HermesApi => "hermes_api",
            SecretKey::McpHermes => "mcp_hermes",
        }
    }
}

pub type SecretResult<T> = Result<T, String>;

pub trait SecretStore: Send + Sync {
    /// `None` on missing value or any backend error.
    fn get(&self, k: SecretKey) -> Option<String>;
    /// Empty value deletes.
    fn set(&self, k: SecretKey, v: &str) -> SecretResult<()>;
    fn delete(&self, k: SecretKey) -> SecretResult<()>;

    /// All stored secret values (for output redaction).
    fn all_values(&self) -> Vec<String> {
        SecretKey::ALL
            .iter()
            .filter_map(|k| self.get(*k))
            .filter(|v| !v.is_empty())
            .collect()
    }
}

const KEYRING_SERVICE: &str = "com.local.assistant";

pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self {
            service: KEYRING_SERVICE.into(),
        }
    }

    fn entry(&self, k: SecretKey) -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(&self.service, k.account())
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, k: SecretKey) -> Option<String> {
        self.entry(k).ok()?.get_password().ok()
    }

    fn set(&self, k: SecretKey, v: &str) -> SecretResult<()> {
        if v.is_empty() {
            return self.delete(k);
        }
        self.entry(k)
            .and_then(|e| e.set_password(v))
            .map_err(|e| format!("keyring set {}: {e}", k.account()))
    }

    fn delete(&self, k: SecretKey) -> SecretResult<()> {
        match self.entry(k).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("keyring delete {}: {e}", k.account())),
        }
    }
}

#[derive(Default)]
pub struct MemorySecrets {
    map: Mutex<HashMap<&'static str, String>>,
}

impl SecretStore for MemorySecrets {
    fn get(&self, k: SecretKey) -> Option<String> {
        self.map.lock().ok()?.get(k.account()).cloned()
    }

    fn set(&self, k: SecretKey, v: &str) -> SecretResult<()> {
        if v.is_empty() {
            return self.delete(k);
        }
        self.map
            .lock()
            .map_err(|_| "lock poisoned".to_string())?
            .insert(k.account(), v.to_string());
        Ok(())
    }

    fn delete(&self, k: SecretKey) -> SecretResult<()> {
        self.map
            .lock()
            .map_err(|_| "lock poisoned".to_string())?
            .remove(k.account());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.thresholds.route, 0.75);
        assert_eq!(s.thresholds.command_safe, 0.9);
        assert_eq!(s.max_commands_per_turn, 20);
        assert_eq!(s.approval_timeout_secs, 300);
        assert_eq!(s.anthropic_model, "claude-opus-5-5");
        assert_eq!(s.hermes_base_url, "http://127.0.0.1:8642/v1");
        assert_eq!(s.anthropic_base_url, "https://api.anthropic.com");
        assert_eq!(s.jev_base_url, "https://api.typesafe.ai");
        assert_eq!(s.mcp_port, 8765);
        assert_eq!(s.run_timeout_secs, 120);
        assert!(s.auto_route);
        assert_eq!(s.default_provider, None);
        assert_eq!(s.claude_model, None);
        assert_eq!(
            s.hermes_allowed_toolsets.len(),
            crate::providers::hermes::DEFAULT_ALLOWED_TOOLSETS.len()
        );
        assert_eq!(
            s.description(ProviderId::Hermes),
            "งานหลายขั้นตอน ใช้เครื่องมือ ค้นเว็บ จำบริบทระยะยาว"
        );
        assert_eq!(
            s.description(ProviderId::ClaudeCli),
            "งานเขียนโค้ด แก้ไฟล์ในโปรเจกต์"
        );
        assert_eq!(
            s.description(ProviderId::Api),
            "คำถามทั่วไป ตอบเร็ว ไม่ต้องใช้เครื่องมือ"
        );
        assert!(s.validate().is_ok());
    }

    #[test]
    fn settings_file_never_contains_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let secrets = MemorySecrets::default();
        secrets.set(SecretKey::Jev, "sk-jev-111").unwrap();
        secrets.set(SecretKey::Anthropic, "sk-ant-222").unwrap();
        secrets.set(SecretKey::HermesApi, "hermes-333").unwrap();
        secrets.set(SecretKey::McpHermes, "mcp-444").unwrap();
        Settings::default().save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let vals = secrets.all_values();
        assert_eq!(vals.len(), 4);
        for v in vals {
            assert!(!text.contains(&v), "secret leaked: {v}");
        }
    }

    #[test]
    fn corrupt_file_falls_back_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let bak = dir.path().join("settings.json.bak");
        fs::write(&bak, "old").unwrap();
        fs::write(&path, "{ not json").unwrap();
        let s = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&bak).unwrap(), "{ not json");
    }

    #[test]
    fn invalid_values_file_treated_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"mcp_port":0}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn validate_rejects_out_of_range() {
        type Mutator = Box<dyn Fn(&mut Settings)>;
        let bad: Vec<Mutator> = vec![
            Box::new(|s| s.thresholds.route = 1.2),
            Box::new(|s| s.thresholds.command_risk = -0.1),
            Box::new(|s| s.thresholds.command_safe = f64::NAN),
            Box::new(|s| s.max_commands_per_turn = 0),
            Box::new(|s| s.approval_timeout_secs = 9),
            Box::new(|s| s.approval_timeout_secs = 3601),
            Box::new(|s| s.run_timeout_secs = 0),
            Box::new(|s| s.mcp_port = 0),
            Box::new(|s| s.hermes_base_url = "http://example.com/v1".into()),
            Box::new(|s| s.hermes_base_url = "ftp://127.0.0.1/".into()),
            Box::new(|s| s.anthropic_base_url = "not a url".into()),
            Box::new(|s| s.jev_base_url = "file:///x".into()),
        ];
        for (i, f) in bad.iter().enumerate() {
            let mut s = Settings::default();
            f(&mut s);
            assert!(s.validate().is_err(), "case {i} accepted");
        }
        let mut s = Settings::default();
        s.thresholds.route = 1.2;
        assert!(s.validate().unwrap_err().contains("route"));
        let mut s = Settings {
            hermes_base_url: "http://[::1]:8642/v1".into(),
            ..Settings::default()
        };
        assert!(s.validate().is_ok());
        s.hermes_base_url = "http://localhost:8642/v1".into();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn missing_file_gives_defaults_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(Settings::load(&path), Settings::default());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn older_file_missing_fields_takes_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"max_commands_per_turn":5,"unknown_field":1,"provider_descriptions":{"api":"custom"}}"#,
        )
        .unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.max_commands_per_turn, 5);
        assert_eq!(s.mcp_port, 8765);
        assert_eq!(s.jev_base_url, "https://api.typesafe.ai");
        assert_eq!(s.description(ProviderId::Api), "custom");
        assert_eq!(
            s.description(ProviderId::Hermes),
            Settings::default().description(ProviderId::Hermes)
        );
        assert!(!dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn save_round_trips_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        let path = sub.join("settings.json");
        let mut s = Settings {
            default_provider: Some(ProviderId::Api),
            claude_model: Some("m".into()),
            ..Settings::default()
        };
        s.save(&path).unwrap();
        assert!(!sub.join("settings.json.tmp").exists());
        assert_eq!(Settings::load(&path), s);
        s.max_commands_per_turn = 3;
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).max_commands_per_turn, 3);
        assert!(!sub.join("settings.json.tmp").exists());
    }

    #[test]
    fn save_rejects_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = Settings {
            mcp_port: 0,
            ..Settings::default()
        };
        let e = s.save(&path).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!path.exists());
    }

    #[test]
    fn memory_secrets_behave() {
        let m = MemorySecrets::default();
        assert_eq!(m.get(SecretKey::Jev), None);
        m.set(SecretKey::Jev, "a").unwrap();
        assert_eq!(m.get(SecretKey::Jev).as_deref(), Some("a"));
        m.set(SecretKey::Anthropic, "b").unwrap();
        let mut v = m.all_values();
        v.sort();
        assert_eq!(v, vec!["a", "b"]);
        m.set(SecretKey::Jev, "").unwrap();
        assert_eq!(m.get(SecretKey::Jev), None);
        m.delete(SecretKey::Anthropic).unwrap();
        m.delete(SecretKey::Anthropic).unwrap();
        assert!(m.all_values().is_empty());
    }

    #[test]
    fn secret_account_names() {
        assert_eq!(SecretKey::Jev.account(), "jev");
        assert_eq!(SecretKey::Anthropic.account(), "anthropic");
        assert_eq!(SecretKey::HermesApi.account(), "hermes_api");
        assert_eq!(SecretKey::McpHermes.account(), "mcp_hermes");
    }

    #[test]
    #[ignore = "touches the real Windows Credential Manager"]
    fn real_keyring_round_trip() {
        let k = KeyringStore::new();
        k.set(SecretKey::McpHermes, "test-value").unwrap();
        assert_eq!(k.get(SecretKey::McpHermes).as_deref(), Some("test-value"));
        k.delete(SecretKey::McpHermes).unwrap();
        assert_eq!(k.get(SecretKey::McpHermes), None);
    }
}
