use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use uuid::Uuid;

use futures_util::StreamExt;

use super::device::{parse_adb_devices, server_args, Device};
use super::path::{adb_path, ensure_adb_server, hide_window};
use crate::config::ServerConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdbServer {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub enabled: bool,
}

impl AdbServer {
    pub fn new(host: String, port: u16) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            host,
            port,
            enabled: true,
        }
    }

    pub fn from_config(cfg: &ServerConfig) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            host: cfg.host.clone(),
            port: cfg.port,
            enabled: cfg.enabled,
        }
    }
}

fn run_adb_timeout(args: &[String], timeout_secs: u64) -> String {
    let mut cmd = Command::new(adb_path());
    hide_window(&mut cmd);
    let mut child = match cmd
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return String::new();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
        }
    }

    child
        .wait_with_output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn fallback_device(serial: String, status: String, srv: &AdbServer) -> Device {
    Device {
        serial,
        status,
        model: String::new(),
        battery: -1,
        screen_width: 0,
        screen_height: 0,
        server_host: srv.host.clone(),
        server_port: srv.port,
    }
}

fn fetch_device_info(serial: &str, srv: &AdbServer) -> Device {
    let mut screen_width: u32 = 0;
    let mut screen_height: u32 = 0;
    let mut model = String::new();
    let mut battery: i32 = -1;

    // Single adb shell call combining all 3 queries, separated by a sentinel.
    // This reduces 3 sequential process spawns + round-trips to 1.
    {
        let mut args = server_args(&srv.host, srv.port);
        args.extend([
            "-s".into(), serial.into(), "shell".into(),
            "wm size; echo '---DELIM---'; getprop ro.product.model; echo '---DELIM---'; dumpsys battery".into(),
        ]);
        let output = run_adb_timeout(&args, 4);
        let sections: Vec<&str> = output.split("---DELIM---").collect();

        // Section 0: wm size
        if let Some(wm_output) = sections.first() {
            for line in wm_output.lines().rev() {
                if line.contains("size:") {
                    if let Some(dims) = line.split(':').last() {
                        let parts: Vec<&str> = dims.trim().split('x').collect();
                        if parts.len() == 2 {
                            screen_width = parts[0].trim().parse().unwrap_or(0);
                            screen_height = parts[1].trim().parse().unwrap_or(0);
                            break;
                        }
                    }
                }
            }
        }

        // Section 1: getprop ro.product.model
        if let Some(model_output) = sections.get(1) {
            model = model_output.trim().to_string();
        }

        // Section 2: dumpsys battery
        if let Some(battery_output) = sections.get(2) {
            for line in battery_output.lines() {
                let line = line.trim();
                if line.starts_with("level:") {
                    battery = line
                        .split(':')
                        .last()
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(-1);
                    break;
                }
            }
        }
    }

    println!(
        "[DEVICE] serial={} model={} battery={} screen={}x{}",
        serial, model, battery, screen_width, screen_height
    );

    Device {
        serial: serial.to_string(),
        status: "online".into(),
        model,
        battery,
        screen_width,
        screen_height,
        server_host: srv.host.clone(),
        server_port: srv.port,
    }
}

pub async fn poll_all_servers(servers: Arc<Mutex<Vec<AdbServer>>>, app: AppHandle) {
    let servers = servers.lock().await.clone();

    // Snapshot of the last emitted device list. Devices already enriched in a
    // previous poll skip the info queries entirely — startup fires several
    // polls back-to-back, and re-querying every device each time floods the
    // ADB server and starves `adb devices` itself.
    static LAST_DEVICES: OnceLock<std::sync::Mutex<Vec<Device>>> = OnceLock::new();
    let last_devices = LAST_DEVICES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let cached = Arc::new(last_devices.lock().unwrap().clone());

    let mut tasks = futures_util::stream::FuturesUnordered::new();

    // The adb daemon may not be running yet (nothing listens on 5037 after a
    // reboot, and `adb devices` then returns nothing). Bring it up first.
    if servers
        .iter()
        .any(|s| s.enabled && (s.host == "127.0.0.1" || s.host == "localhost") && s.port == 5037)
    {
        tokio::task::spawn_blocking(ensure_adb_server)
            .await
            .ok();
    }

    for srv in servers.into_iter().filter(|s| s.enabled) {
        let srv = srv.clone();
        let cached = Arc::clone(&cached);
        tasks.push(tokio::spawn(async move {
            let mut args = server_args(&srv.host, srv.port);
            args.push("devices".into());
            // Keep startup responsive: slow/offline ADB servers should not
            // hold back devices discovered from healthy servers.
            let output = tokio::task::spawn_blocking(move || run_adb_timeout(&args, 5))
                .await
                .unwrap_or_default();
            let pairs = parse_adb_devices(&output);

            let mut devices = Vec::new();
            let mut online_serials = Vec::new();

            for (serial, status) in pairs {
                if status == "device" {
                    online_serials.push(serial.clone());
                    // Reuse info from the previous poll when we have it; only
                    // genuinely new devices need the full enrich pass.
                    let cached_dev = cached.iter().find(|d| {
                        d.serial == serial
                            && d.server_host == srv.host
                            && d.server_port == srv.port
                            && d.status == "online"
                            && !d.model.is_empty()
                    });
                    match cached_dev {
                        Some(d) => devices.push(d.clone()),
                        None => devices.push(fallback_device(serial, "online".into(), &srv)),
                    }
                } else {
                    devices.push(fallback_device(serial, status, &srv));
                }
            }

            (devices, srv, online_serials)
        }));
    }

    let mut all_devices = Vec::new();
    if tasks.is_empty() {
        let _ = app.emit("devices-updated", &all_devices);
        *last_devices.lock().unwrap() = all_devices;
        return;
    }

    let mut enrich_jobs = Vec::new();
    while let Some(result) = tasks.next().await {
        if let Ok((devices, srv, online_serials)) = result {
            all_devices.extend(devices);
            let _ = app.emit("devices-updated", &all_devices);
            // Only devices without cached info need the enrich pass.
            let fresh: Vec<String> = online_serials
                .into_iter()
                .filter(|serial| {
                    !all_devices.iter().any(|d| {
                        d.serial == *serial
                            && d.server_host == srv.host
                            && d.server_port == srv.port
                            && !d.model.is_empty()
                    })
                })
                .collect();
            if !fresh.is_empty() {
                enrich_jobs.push((srv, fresh));
            }
        }
    }

    let _ = app.emit("devices-updated", &all_devices);

    let app_for_enrich = app.clone();
    tokio::spawn(async move {
        let mut enriched = all_devices;
        let mut changed = false;

        for (srv, serials) in enrich_jobs {
            for chunk in serials.chunks(4) {
                let mut handles = Vec::new();
                for serial in chunk {
                    let serial = serial.clone();
                    let srv = srv.clone();
                    handles.push(tokio::task::spawn_blocking(move || {
                        fetch_device_info(&serial, &srv)
                    }));
                }

                for handle in handles {
                    let Ok(dev) = handle.await else {
                        continue;
                    };
                    if let Some(existing) = enriched.iter_mut().find(|d| {
                        d.serial == dev.serial
                            && d.server_host == dev.server_host
                            && d.server_port == dev.server_port
                    }) {
                        *existing = dev;
                        changed = true;
                    }
                }

                if changed {
                    let _ = app_for_enrich.emit("devices-updated", &enriched);
                    changed = false;
                }
            }
        }

        // Persist for the next poll so cached devices skip re-enrichment.
        *last_devices.lock().unwrap() = enriched.clone();
    });
}
