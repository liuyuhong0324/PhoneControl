use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub enabled: bool,
}

/// The daemon this app starts itself: it holds the USB devices, and a sweep
/// attaches the wireless ones to it. Kept in the config like any other server so
/// it survives a restart — the UI shows it but never adds or removes it, since
/// the app would then have nothing to reach the devices through.
pub const LOCAL_HOST: &str = "127.0.0.1";
pub const LOCAL_ADB_PORT: u16 = 5037;

/// Is this the local daemon rather than one the user added?
pub fn is_local_server(host: &str, port: u16) -> bool {
    (host == LOCAL_HOST || host == "localhost") && port == LOCAL_ADB_PORT
}

fn default_servers() -> Vec<ServerConfig> {
    vec![ServerConfig {
        host: LOCAL_HOST.into(),
        port: LOCAL_ADB_PORT,
        enabled: true,
    }]
}

#[derive(Debug, Serialize, Deserialize)]
struct ConfigFile {
    servers: Vec<ServerConfig>,
}

fn config_path() -> PathBuf {
    let mut p = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push(".phone_control");
    fs::create_dir_all(&p).ok();
    p.push("servers.json");
    p
}

pub fn load_servers() -> Vec<ServerConfig> {
    let path = config_path();
    if !path.exists() {
        return default_servers();
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str::<ConfigFile>(&text)
        .map(|c| c.servers)
        .unwrap_or_else(|_| default_servers())
}

pub fn save_servers(servers: &[ServerConfig]) -> Result<(), String> {
    let path = config_path();
    let data = ConfigFile {
        servers: servers.to_vec(),
    };
    let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_roundtrip() {
        let tmp = env::temp_dir().join("phone_control_test");
        fs::create_dir_all(&tmp).unwrap();
        let servers = vec![
            ServerConfig {
                host: "192.168.1.1".into(),
                port: 5037,
                enabled: true,
            },
            ServerConfig {
                host: "10.0.0.1".into(),
                port: 5555,
                enabled: false,
            },
        ];
        let data = ConfigFile {
            servers: servers.clone(),
        };
        let text = serde_json::to_string_pretty(&data).unwrap();
        let path = tmp.join("servers.json");
        fs::write(&path, &text).unwrap();
        let loaded: ConfigFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.servers.len(), 2);
        assert_eq!(loaded.servers[0].host, "192.168.1.1");
        assert_eq!(loaded.servers[1].port, 5555);
        assert!(!loaded.servers[1].enabled);
    }

    /// A config file written by an older build — one that still carried the
    /// `scan` field — must load: it is a hand-editable file in the user's home
    /// directory, and the sweep no longer needs that key.
    #[test]
    fn test_ignores_a_leftover_scan_field() {
        let text =
            r#"{"servers":[{"host":"127.0.0.1","port":5037,"enabled":true,"scan":"192.168.101.1"}]}"#;
        let loaded: ConfigFile = serde_json::from_str(text).unwrap();
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.servers[0].host, "127.0.0.1");
    }

    /// The local daemon is recognised by host *and* port, so the UI can keep it
    /// out of the list without hiding a real remote server on the same port.
    #[test]
    fn recognises_the_local_entry() {
        assert!(is_local_server("127.0.0.1", 5037));
        assert!(is_local_server("localhost", 5037));
        assert!(!is_local_server("192.168.101.1", 5037));
        assert!(!is_local_server("127.0.0.1", 5555));
    }
}
