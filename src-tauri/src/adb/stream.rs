use std::{
    collections::HashSet,
    io::Read,
    net::TcpStream,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tauri::Manager;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::ws::WsHub;

// (placeholder) server_args will be used by protocol-level client
// use super::device::server_args;

/// Render a scrcpy codec_id FourCC as a short string for logs.
///
/// scrcpy 3.x encodes `codec_id` as a 4-byte ASCII FourCC in big-endian order.
/// Known video codecs: `h264` (0x68323634), `h265` (0x68323635), `\x00av1`
/// (0x00617631). Unknown values render in hex.
pub(crate) fn parse_codec_fourcc(bytes: &[u8]) -> String {
    if bytes.len() < 4 {
        return format!("<{} bytes>", bytes.len());
    }
    let b = &bytes[0..4];
    if b.iter().all(|&c| c.is_ascii_graphic() || c == b' ') {
        // All printable → render as the ASCII it is
        String::from_utf8_lossy(b).to_string()
    } else if b[0] == 0 && b[1..].iter().all(|&c| c.is_ascii_graphic()) {
        // "\x00av1" style — show the printable suffix
        String::from_utf8_lossy(&b[1..]).to_string()
    } else {
        format!("0x{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3])
    }
}

const CONNECT_BASE_DELAY_MS: u64 = 100;
const CONNECT_MAX_DELAY_MS: u64 = 1500;
const MAX_RECONNECT_SLEEP_MS: u64 = 5_000;
const USB_BOUNCE_FAST_RETRY_WINDOW_MS: u64 = 5_000;
const STABLE_DISCONNECT_RECONNECT_DELAY_MS: u64 = 500;

fn reconnect_delay_ms(
    serial: &str,
    attempt: u32,
    device_not_found: bool,
    recent_stream_drop: bool,
) -> u64 {
    let raw_delay = if device_not_found {
        if recent_stream_drop {
            // USB mode switching briefly removes the device from ADB. Poll
            // quickly right after a stable stream drops so we do not add
            // seconds of artificial latency before ADB re-enumerates it.
            let base = std::cmp::min(900, 120 * attempt.min(8) as u64);
            base + reconnect_jitter_ms(serial, 180)
        } else {
            let base = std::cmp::min(15_000, 2_000 * attempt.min(6) as u64);
            base + reconnect_jitter_ms(serial, 2_500)
        }
    } else {
        std::cmp::min(
            CONNECT_MAX_DELAY_MS,
            CONNECT_BASE_DELAY_MS * (1u64 << attempt.min(3)),
        ) + reconnect_jitter_ms(serial, 250)
    };
    raw_delay.min(MAX_RECONNECT_SLEEP_MS)
}

pub struct StreamTokenEntry {
    token: CancellationToken,
    session_id: u64,
    host: String,
    port: u16,
    clients: HashSet<String>,
}

pub type StreamTokens = Arc<Mutex<std::collections::HashMap<String, StreamTokenEntry>>>;
static NEXT_STREAM_SESSION_ID: AtomicU64 = AtomicU64::new(1);

pub struct ControlEntry {
    pub stream: std::net::TcpStream,
    pub video_width: u32,
    pub video_height: u32,
    /// When we last asked this device for a fresh keyframe — see
    /// [`request_keyframe`]. `None` means "never asked", which must not be
    /// throttled: the first client to subscribe right after a stream starts
    /// relies on it. Guards against reset storms when a client keeps falling
    /// behind.
    pub last_keyframe_request: Option<std::time::Instant>,
}

pub type ControlSockets = Arc<std::sync::Mutex<std::collections::HashMap<String, ControlEntry>>>;

pub fn new_tokens() -> StreamTokens {
    Arc::new(Mutex::new(std::collections::HashMap::new()))
}

pub fn new_control_sockets() -> ControlSockets {
    Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn remove_control_socket(control_sockets: &ControlSockets, serial: &str, reason: &str) {
    // A standalone scrcpy window (the ▶ button) owns the panel while it is
    // open: it turned the display off itself, and it turns it back on when it
    // exits. Restoring here would light the panel while that window mirrors.
    let standalone_open = has_standalone_scrcpy(serial);
    if let Ok(mut sockets) = control_sockets.lock() {
        if let Some(mut entry) = sockets.remove(serial) {
            if standalone_open {
                println!(
                    "[SCRCPY-CTRL] removed control socket serial={} reason={} \
                     (left dark: standalone scrcpy owns the panel)",
                    serial, reason
                );
                return;
            }
            // Mirroring blanked the device panel (scrcpy `--turn-screen-off`
            // semantics). Ask for the screen back BEFORE dropping the control
            // socket: the server stops reading it the moment it sees EOF, and
            // the fallback cleanup path can be short-circuited when the server
            // shell is torn down right after.
            let _ = super::scrcpy_control::inject_display_power(&mut entry.stream, false);
            println!(
                "[SCRCPY-CTRL] removed control socket serial={} reason={} (screen restored)",
                serial, reason
            );
        }
    }
}

/// Devices with a standalone scrcpy window open.
///
/// Display power is a per-device (HWC) state, not a per-session one, so two
/// scrcpy sessions on the same device fight over it. In particular a standalone
/// scrcpy turns the panel back ON when it exits — which silently cancels the
/// preview stream's `--turn-screen-off`, since the preview only sends that
/// message once, when it connects.
static STANDALONE_SESSIONS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

fn standalone_sessions() -> &'static std::sync::Mutex<HashSet<String>> {
    STANDALONE_SESSIONS.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// Record that a standalone scrcpy window opened (`true`) or closed (`false`).
pub fn mark_standalone_scrcpy(serial: &str, open: bool) {
    let Ok(mut sessions) = standalone_sessions().lock() else {
        return;
    };
    if open {
        sessions.insert(serial.to_string());
    } else {
        sessions.remove(serial);
    }
}

pub fn has_standalone_scrcpy(serial: &str) -> bool {
    standalone_sessions()
        .lock()
        .map(|sessions| sessions.contains(serial))
        .unwrap_or(false)
}

/// Put a mirrored device's panel back to what the preview expects (dark).
///
/// Needed after anything else has set a different display power on the device —
/// most importantly when a standalone scrcpy window closes. A no-op when no
/// preview stream is running for that device.
pub fn reassert_display_off(control_sockets: &ControlSockets, serial: &str) {
    let Ok(mut sockets) = control_sockets.lock() else {
        return;
    };
    let Some(entry) = sockets.get_mut(serial) else {
        return;
    };
    match super::scrcpy_control::inject_display_power(&mut entry.stream, true) {
        Ok(()) => println!("[SCRCPY-CTRL] display off re-asserted serial={}", serial),
        Err(e) => println!(
            "[SCRCPY-CTRL] display off re-assert failed serial={}: {}",
            serial, e
        ),
    }
}

/// Turn every mirrored device's screen back on.
///
/// Called from the app exit handler: mirroring blanks the panel (scrcpy
/// `--turn-screen-off` semantics), and stream teardown never runs on process
/// exit, so without this the phones would stay dark until physically touched.
pub fn restore_all_displays(control_sockets: &ControlSockets) {
    let mut entries: Vec<(String, TcpStream)> = match control_sockets.lock() {
        Ok(mut map) => map.drain().map(|(serial, entry)| (serial, entry.stream)).collect(),
        Err(_) => return,
    };
    if entries.is_empty() {
        return;
    }
    for (serial, stream) in entries.iter_mut() {
        // A standalone scrcpy window still owns the panel: it is mirroring with
        // the display off and will restore it when it exits on its own.
        if has_standalone_scrcpy(serial) {
            println!(
                "[SCRCPY-CTRL] left serial={} dark on exit: standalone scrcpy owns the panel",
                serial
            );
            continue;
        }
        match super::scrcpy_control::inject_display_power(stream, false) {
            Ok(()) => println!("[SCRCPY-CTRL] screen restored on exit serial={}", serial),
            Err(e) => println!("[SCRCPY-CTRL] screen restore failed serial={}: {}", serial, e),
        }
    }
    // Keep the sockets open just long enough for the devices to read the
    // message before the process goes away.
    std::thread::sleep(std::time::Duration::from_millis(250));
    drop(entries);
}

/// Ask a device for a fresh codec config + keyframe, rate limited per device.
///
/// Video only becomes decodable again from a keyframe onward, and these
/// encoders emit IDRs on picture change only (`i-frame-interval` is ignored),
/// so partners of this call are: a preview client that just subscribed, and
/// the frame fan-out dropping packets it could not deliver. Both leave a
/// decoder waiting for a keyframe that would otherwise never come.
///
/// Returns true when the request was actually sent.
pub fn request_keyframe(control_sockets: &ControlSockets, serial: &str) -> bool {
    const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
    let Ok(mut sockets) = control_sockets.lock() else {
        return false;
    };
    let Some(entry) = sockets.get_mut(serial) else {
        return false;
    };
    if entry
        .last_keyframe_request
        .is_some_and(|last| last.elapsed() < MIN_INTERVAL)
    {
        return false;
    }
    entry.last_keyframe_request = Some(std::time::Instant::now());
    match super::scrcpy_control::inject_reset_video(&mut entry.stream) {
        Ok(()) => {
            println!("[SCRCPY-CTRL] keyframe requested serial={}", serial);
            true
        }
        Err(e) => {
            println!("[SCRCPY-CTRL] keyframe request failed serial={}: {}", serial, e);
            false
        }
    }
}

fn emit_stream_status(
    app: &AppHandle,
    serial: &str,
    host: &str,
    port: u16,
    session_id: u64,
    status: &str,
    error: Option<&str>,
) {
    let payload = match error {
        Some(error) => serde_json::json!({
            "serial": serial,
            "serverHost": host,
            "serverPort": port,
            "sessionId": session_id,
            "status": status,
            "error": error,
        }),
        None => serde_json::json!({
            "serial": serial,
            "serverHost": host,
            "serverPort": port,
            "sessionId": session_id,
            "status": status,
        }),
    };
    let _ = app.emit("stream-status", payload);
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct StreamOptions {
    pub max_size: u32,
    pub max_fps: u32,
    pub bit_rate: u32,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            max_size: 720,
            max_fps: 30,
            bit_rate: 4_000_000,
        }
    }
}

fn reconnect_jitter_ms(serial: &str, modulo: u64) -> u64 {
    if modulo == 0 {
        0
    } else {
        fxhash::hash64(serial) % modulo
    }
}

/// Start a scrcpy-based video stream.
///
/// This connects to the scrcpy server and forwards raw H.264 packets to the local WebSocket hub.
///
/// Notes:
/// - Control is intentionally disabled (Phase 1).
/// - The frontend decodes with WebCodecs and paints directly into per-device canvases.
pub async fn start_stream_loop(
    tokens: StreamTokens,
    control_sockets: ControlSockets,
    adb_semaphore: Arc<Semaphore>,
    serial: String,
    host: String,
    port: u16,
    opts: StreamOptions,
    client_id: String,
    app: AppHandle,
) {
    println!(
        "[STREAM] start_stream_loop serial={} server={}:{} client={} opts={{max_size={}, max_fps={}, bit_rate={}}}",
        serial, host, port, client_id, opts.max_size, opts.max_fps, opts.bit_rate
    );
    let token = CancellationToken::new();
    let session_id = NEXT_STREAM_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut map = tokens.lock().await;
        if let Some(existing) = map.get_mut(&serial) {
            if existing.host == host && existing.port == port && !existing.token.is_cancelled() {
                let inserted = existing.clients.insert(client_id.clone());
                println!(
                    "[STREAM] reuse existing stream serial={} server={}:{} client={} inserted={} clients={}",
                    serial,
                    host,
                    port,
                    client_id,
                    inserted,
                    existing.clients.len()
                );
                return;
            }
            if let Some(old) = map.remove(&serial) {
                println!(
                    "[STREAM] replacing stream serial={} old={}:{} new={}:{}",
                    serial, old.host, old.port, host, port
                );
                old.token.cancel();
            }
        }

        let mut clients = HashSet::new();
        clients.insert(client_id);
        map.insert(
            serial.clone(),
            StreamTokenEntry {
                token: token.clone(),
                session_id,
                host: host.clone(),
                port,
                clients,
            },
        );
    }

    // Backoff for connection-attempt failures. Large batches can make many
    // devices re-enumerate at once, so device-not-found retries use a longer
    // staggered delay to avoid a reconnect storm against the ADB server.
    let mut attempt: u32 = 0;
    let mut first_run = true;
    let mut disconnect_count: u32 = 0;
    let mut fast_retry_until: Option<std::time::Instant> = None;

    loop {
        if token.is_cancelled() {
            break;
        }

        // Cleanup from previous iteration
        remove_control_socket(&control_sockets, &serial, "stream iteration restart");

        let status_label = if first_run {
            "starting"
        } else {
            "reconnecting"
        };
        emit_stream_status(&app, &serial, &host, port, session_id, status_label, None);
        if !first_run {
            println!(
                "[STREAM] reconnecting serial={} attempt={}",
                serial, attempt
            );
        }
        first_run = false;

        // Connect to scrcpy server. This is ADB-heavy, so share a global
        // limiter with group input commands to avoid starving tap/swipe.
        let adb_permit = tokio::select! {
            _ = token.cancelled() => { break; }
            permit = Arc::clone(&adb_semaphore).acquire_owned() => {
                match permit {
                    Ok(permit) => permit,
                    Err(_) => break,
                }
            }
        };

        let serial_clone = serial.clone();
        let host_clone = host.clone();
        let opts_clone = opts.clone();
        let conn_result = tokio::task::spawn_blocking(move || {
            super::scrcpy_client::start_scrcpy_and_connect(
                &serial_clone,
                &host_clone,
                port,
                &opts_clone,
            )
        })
        .await;
        drop(adb_permit);

        let scrcpy_conn = match conn_result {
            Ok(Ok(c)) => {
                attempt = 0;
                fast_retry_until = None;
                c
            }
            Ok(Err(e)) => {
                println!("[STREAM] connect failed serial={}: {}", serial, e);
                let device_not_found = e.contains("device") && e.contains("not found");
                emit_stream_status(
                    &app,
                    &serial,
                    &host,
                    port,
                    session_id,
                    "reconnecting",
                    Some(&e),
                );
                attempt += 1;
                let recent_stream_drop = fast_retry_until
                    .map(|deadline| std::time::Instant::now() <= deadline)
                    .unwrap_or(false);
                let delay =
                    reconnect_delay_ms(&serial, attempt, device_not_found, recent_stream_drop);
                println!(
                    "[STREAM] reconnect sleep serial={} attempt={} delay={}ms mode={}",
                    serial,
                    attempt,
                    delay,
                    if device_not_found && recent_stream_drop {
                        "usb-bounce-fast"
                    } else if device_not_found {
                        "device-not-found"
                    } else {
                        "normal"
                    }
                );
                tokio::select! {
                    _ = token.cancelled() => { break; }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
                }
                continue;
            }
            Err(e) => {
                println!("[STREAM] connect task panicked serial={}: {}", serial, e);
                attempt += 1;
                let delay = reconnect_delay_ms(&serial, attempt, false, false);
                tokio::select! {
                    _ = token.cancelled() => { break; }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
                }
                continue;
            }
        };

        emit_stream_status(&app, &serial, &host, port, session_id, "connected", None);
        let connected_at = std::time::Instant::now();
        let stdout = scrcpy_conn.stream;
        let local_port = scrcpy_conn.local_port;
        let server_shell = scrcpy_conn.server_shell;

        if let Some(ctrl) = scrcpy_conn.control {
            control_sockets.lock().unwrap().insert(
                serial.clone(),
                ControlEntry {
                    stream: ctrl,
                    video_width: 0,
                    video_height: 0,
                    last_keyframe_request: None,
                },
            );
            // A tile that mounted while this stream was still starting has its
            // keyframe request queued in the hub — the socket to send it on
            // exists only now.
            app.state::<WsHub>()
                .inner()
                .flush_pending_keyframe(&serial);
        }

        let serial_for_task = serial.clone();
        let host_for_task = host.clone();
        let token_for_task = token.clone();
        let app_for_task = app.clone();
        let hub = app.state::<WsHub>().inner().clone();
        let cs_for_task = Arc::clone(&control_sockets);
        let forward_task = tokio::task::spawn_blocking(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                forward_h264_to_ws(
                    stdout,
                    &serial_for_task,
                    &host_for_task,
                    port,
                    session_id,
                    &token_for_task,
                    &app_for_task,
                    &hub,
                    &cs_for_task,
                )
            }));

            match res {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = app_for_task.emit(
                        "stream-error",
                        serde_json::json!({
                            "serial": &serial_for_task,
                            "serverHost": &host_for_task,
                            "serverPort": port,
                            "sessionId": session_id,
                            "error": e,
                        }),
                    );
                }
                Err(_) => {
                    let _ = app_for_task.emit(
                        "stream-error",
                        serde_json::json!({
                            "serial": &serial_for_task,
                            "serverHost": &host_for_task,
                            "serverPort": port,
                            "sessionId": session_id,
                            "error": "panic in stream forward",
                        }),
                    );
                }
            }

            remove_control_socket(&cs_for_task, &serial_for_task, "stream forward ended");
        });

        let cancelled = tokio::select! {
            _ = token.cancelled() => true,
            _ = forward_task => {
                println!("[STREAM] stream disconnected serial={}", serial);
                remove_control_socket(&control_sockets, &serial, "stream disconnected");
                false
            }
        };

        // Give the server a moment to process the display-power restore that
        // remove_control_socket just sent before we tear its shell down —
        // killing it first would leave the device panel dark.
        std::thread::sleep(std::time::Duration::from_millis(150));
        super::scrcpy_client::shutdown_shell(&server_shell);
        super::scrcpy_client::remove_forward(&host, port, &serial, local_port);
        println!(
            "[SCRCPY] forward listener removed serial={} port={} reason=stream ended",
            serial, local_port
        );
        if cancelled {
            break;
        }

        let alive_ms = connected_at.elapsed().as_millis() as u64;
        if alive_ms >= 2_000 {
            disconnect_count = 0;
            fast_retry_until = Some(
                std::time::Instant::now()
                    + std::time::Duration::from_millis(USB_BOUNCE_FAST_RETRY_WINDOW_MS),
            );
            println!(
                "[STREAM] stable-disconnect reconnect sleep serial={} alive={}ms delay={}ms",
                serial, alive_ms, STABLE_DISCONNECT_RECONNECT_DELAY_MS
            );
            tokio::select! {
                _ = token.cancelled() => { break; }
                _ = tokio::time::sleep(std::time::Duration::from_millis(STABLE_DISCONNECT_RECONNECT_DELAY_MS)) => {}
            }
            continue;
        } else {
            disconnect_count += 1;
            let raw_delay = std::cmp::min(5_000, 500 * disconnect_count.min(8) as u64)
                + reconnect_jitter_ms(&serial, 500);
            let delay = raw_delay.min(MAX_RECONNECT_SLEEP_MS);
            println!(
                "[STREAM] rapid-disconnect backoff serial={} count={} alive={}ms delay={}ms",
                serial, disconnect_count, alive_ms, delay
            );
            tokio::select! {
                _ = token.cancelled() => { break; }
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
            }
        }
    }

    // Final cleanup
    remove_control_socket(&control_sockets, &serial, "stream loop stopped");
    let is_current_session = {
        let mut map = tokens.lock().await;
        match map.get(&serial) {
            Some(entry) if entry.session_id == session_id => {
                map.remove(&serial);
                true
            }
            _ => false,
        }
    };
    if is_current_session {
        let _ = app.emit(
            "stream-status",
            serde_json::json!({
                "serial": serial,
                "serverHost": host,
                "serverPort": port,
                "sessionId": session_id,
                "status": "stopped",
            }),
        );
    }
}

pub async fn stop_stream_loop(
    tokens: StreamTokens,
    control_sockets: ControlSockets,
    serial: &str,
    host: Option<&str>,
    port: Option<u16>,
    client_id: Option<&str>,
    force: bool,
) {
    let mut map = tokens.lock().await;
    let Some(entry) = map.get_mut(serial) else {
        return;
    };
    let source_matches =
        host.is_none_or(|host| entry.host == host) && port.is_none_or(|port| entry.port == port);
    if !source_matches {
        return;
    }

    if force || client_id.is_none() {
        if let Some(entry) = map.remove(serial) {
            remove_control_socket(&control_sockets, serial, "stop stream requested");
            println!(
                "[STREAM] stop stream serial={} reason={} clients={}",
                serial,
                if force { "force" } else { "legacy" },
                entry.clients.len()
            );
            entry.token.cancel();
        }
        return;
    }

    if let Some(client_id) = client_id {
        entry.clients.remove(client_id);
        println!(
            "[STREAM] release stream lease serial={} client={} remaining={}",
            serial,
            client_id,
            entry.clients.len()
        );
        if entry.clients.is_empty() {
            if let Some(entry) = map.remove(serial) {
                remove_control_socket(&control_sockets, serial, "last stream client released");
                entry.token.cancel();
            }
        }
    }
}

/// Forward raw H.264 packets from the scrcpy stream to the WebSocket hub.
///
/// Raw NAL units are sent to the frontend where WebCodecs `VideoDecoder`
/// handles decoding.
fn forward_h264_to_ws<R: Read + Send + 'static>(
    mut input: R,
    serial: &str,
    host: &str,
    port: u16,
    session_id: u64,
    token: &CancellationToken,
    app: &AppHandle,
    hub: &WsHub,
    control_sockets: &ControlSockets,
) -> Result<(), String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 64 * 1024];

    let mut dummy_consumed = false;
    let mut codec_meta_consumed = false;
    let mut video_width: u32 = 0;
    let mut video_height: u32 = 0;
    let mut invalid_header_hits: u32 = 0;
    let mut first_data_at: Option<std::time::Instant> = None;
    let loop_start = std::time::Instant::now();
    let mut last_idle_log: Option<std::time::Instant> = None;
    let mut last_config: Option<Vec<u8>> = None;
    let mut first_packet_forwarded = false;
    let mut first_keyframe_forwarded = false;
    let mut packet_seq: u64 = 0;

    println!("[SCRCPY-FWD] entering read loop serial={}", serial);

    while !token.is_cancelled() {
        let n = match input.read(&mut chunk) {
            Ok(0) => {
                remove_control_socket(control_sockets, serial, "stream EOF");
                emit_stream_status(
                    app,
                    serial,
                    host,
                    port,
                    session_id,
                    "disconnected",
                    Some("video stream disconnected"),
                );
                println!(
                    "[SCRCPY-FWD] stream EOF serial={} after {:?}",
                    serial,
                    loop_start.elapsed()
                );
                break;
            }
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                let now = std::time::Instant::now();
                let should_log = last_idle_log
                    .map(|t| now.duration_since(t) >= std::time::Duration::from_secs(5))
                    .unwrap_or(true);
                if should_log && first_data_at.is_none() {
                    last_idle_log = Some(now);
                    println!(
                        "[SCRCPY-FWD] no bytes from server serial={} waited={:?}",
                        serial,
                        loop_start.elapsed()
                    );
                }
                continue;
            }
            Err(e) => {
                remove_control_socket(control_sockets, serial, "stream read error");
                emit_stream_status(
                    app,
                    serial,
                    host,
                    port,
                    session_id,
                    "disconnected",
                    Some("video stream read error"),
                );
                return Err(format!("read scrcpy stream failed: {e}"));
            }
        };

        if first_data_at.is_none() {
            first_data_at = Some(std::time::Instant::now());
            println!("[SCRCPY-FWD] first bytes serial={} n={}", serial, n);
        }
        buf.extend_from_slice(&chunk[..n]);

        // No decodable picture yet: these encoders emit nothing at all until the
        // device picture changes, so a freshly started stream (and any tile that
        // subscribed while it was starting) would sit blank until the first tap.
        // Keep asking once the server has had time to start its encoder — the
        // callee rate limits the retries, and the loop stops at the first
        // keyframe.
        if !first_keyframe_forwarded && loop_start.elapsed() > std::time::Duration::from_millis(1200)
        {
            request_keyframe(control_sockets, serial);
        }

        if !dummy_consumed && !buf.is_empty() {
            if buf[0] == 0 {
                buf.drain(..1);
            }
            dummy_consumed = true;
        }
        if dummy_consumed && !codec_meta_consumed && buf.len() >= 12 {
            let fourcc = parse_codec_fourcc(&buf[0..4]);
            video_width = u32::from_be_bytes(buf[4..8].try_into().unwrap());
            video_height = u32::from_be_bytes(buf[8..12].try_into().unwrap());
            println!(
                "[SCRCPY-FWD] codec meta serial={} codec={} w={} h={}",
                serial, fourcc, video_width, video_height
            );
            if let Ok(mut sockets) = control_sockets.lock() {
                if let Some(entry) = sockets.get_mut(serial) {
                    entry.video_width = video_width;
                    entry.video_height = video_height;
                    println!(
                        "[SCRCPY-FWD] updated control socket video size serial={} {}x{}",
                        serial, video_width, video_height
                    );
                }
            }
            buf.drain(..12);
            codec_meta_consumed = true;
        }

        if !codec_meta_consumed {
            continue;
        }

        while buf.len() >= 12 {
            let pts_raw = u64::from_be_bytes(buf[0..8].try_into().unwrap());
            let packet_size = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
            if packet_size == 0 {
                buf.drain(..12);
                continue;
            }
            if packet_size > 10 * 1024 * 1024 {
                invalid_header_hits = invalid_header_hits.saturating_add(1);
                if invalid_header_hits % 2000 == 1 {
                    let head_len = std::cmp::min(16, buf.len());
                    let mut hex = String::new();
                    for b in &buf[..head_len] {
                        use std::fmt::Write as _;
                        let _ = write!(&mut hex, "{:02x}", b);
                    }
                    println!(
                        "[SCRCPY-FWD] invalid header serial={} buf_len={} head={}...",
                        serial,
                        buf.len(),
                        hex
                    );
                }
                buf.drain(..1);
                continue;
            }

            invalid_header_hits = 0;
            if buf.len() < 12 + packet_size {
                break;
            }

            let is_config = (pts_raw >> 63) & 1 == 1;
            let is_key = (pts_raw >> 62) & 1 == 1;
            let pts = pts_raw & 0x3FFF_FFFF_FFFF_FFFF;

            let nal_data = &buf[12..12 + packet_size];

            if is_config {
                // Config packet — cache and forward as type 0
                last_config = Some(nal_data.to_vec());
                let seq = packet_seq;
                packet_seq = packet_seq.wrapping_add(1);
                let packed = WsHub::pack_h264_frame(
                    serial,
                    0,
                    seq,
                    pts,
                    video_width,
                    video_height,
                    nal_data,
                );
                hub.broadcast(serial, packed);
            } else {
                let packet_type = if is_key { 1u8 } else { 2 };
                if is_key {
                    first_keyframe_forwarded = true;
                }
                // For keyframes, prepend the last config (SPS/PPS) so the
                // decoder can be (re-)configured even if it missed the
                // initial config packet due to late WS subscription.
                if is_key {
                    if let Some(ref cfg) = last_config {
                        let seq = packet_seq;
                        packet_seq = packet_seq.wrapping_add(1);
                        let packed = WsHub::pack_h264_frame(
                            serial,
                            0,
                            seq,
                            pts,
                            video_width,
                            video_height,
                            cfg,
                        );
                        hub.broadcast(serial, packed);
                    }
                }
                let seq = packet_seq;
                packet_seq = packet_seq.wrapping_add(1);
                let packed = WsHub::pack_h264_frame(
                    serial,
                    packet_type,
                    seq,
                    pts,
                    video_width,
                    video_height,
                    nal_data,
                );
                hub.broadcast(serial, packed);
                if !first_packet_forwarded {
                    first_packet_forwarded = true;
                    let _ = app.emit(
                        "stream-status",
                        serde_json::json!({
                            "serial": serial,
                            "serverHost": host,
                            "serverPort": port,
                            "sessionId": session_id,
                            "status": "receiving",
                        }),
                    );
                }
            }

            buf.drain(..12 + packet_size);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codec_fourcc_h264() {
        // scrcpy 3.x codec_id for H.264 is the ASCII FourCC "h264".
        let bytes = [0x68u8, 0x32, 0x36, 0x34];
        assert_eq!(parse_codec_fourcc(&bytes), "h264");
    }

    #[test]
    fn parse_codec_fourcc_h265() {
        let bytes = [0x68u8, 0x32, 0x36, 0x35];
        assert_eq!(parse_codec_fourcc(&bytes), "h265");
    }

    #[test]
    fn parse_codec_fourcc_av1() {
        // AV1 uses a leading NUL: "\x00av1" (0x00617631).
        let bytes = [0x00u8, 0x61, 0x76, 0x31];
        assert_eq!(parse_codec_fourcc(&bytes), "av1");
    }

    #[test]
    fn parse_codec_fourcc_h264_is_not_in_1_to_3() {
        // Regression guard: the old code used `(1..=3).contains(&codec_id)`
        // which never matched because the FourCC is a large integer, so
        // codec meta was never drained.
        let codec_id = u32::from_be_bytes([0x68, 0x32, 0x36, 0x34]);
        assert!(
            !(1..=3).contains(&codec_id),
            "h264 FourCC = {codec_id} must not match the old 1..=3 check"
        );
    }

    #[test]
    fn parse_codec_fourcc_unknown_renders_hex() {
        let bytes = [0x01u8, 0x02, 0x03, 0x04];
        assert_eq!(parse_codec_fourcc(&bytes), "0x01020304");
    }

    #[test]
    fn parse_codec_fourcc_short_bytes() {
        assert_eq!(parse_codec_fourcc(&[0x68, 0x32]), "<2 bytes>");
    }

    #[test]
    fn reconnect_delay_uses_fast_retry_after_stable_stream_drop() {
        let delay = reconnect_delay_ms("device-a", 1, true, true);

        assert!(
            delay < 300,
            "first USB-bounce retry should not wait multiple seconds, got {delay}ms"
        );
    }

    #[test]
    fn reconnect_delay_keeps_longer_stagger_for_cold_device_not_found() {
        let delay = reconnect_delay_ms("device-a", 1, true, false);

        assert!(
            delay >= 2_000,
            "cold device-not-found retry should stay staggered, got {delay}ms"
        );
    }

    #[test]
    fn reconnect_delay_is_capped() {
        let delay = reconnect_delay_ms("device-a", 6, true, false);

        assert_eq!(delay, MAX_RECONNECT_SLEEP_MS);
    }

    /// Register a fake streaming device: a plain TCP pair stands in for scrcpy's
    /// control socket. Returns the peer end — what the device would receive.
    ///
    /// Each test uses its own serial (the standalone-window registry and the
    /// control-socket map are per-process) and its own socket map.
    fn fake_control_socket(sockets: &ControlSockets, serial: &str) -> std::net::TcpStream {
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        sockets.lock().unwrap().insert(
            serial.to_string(),
            ControlEntry {
                stream: client,
                video_width: 0,
                video_height: 0,
                last_keyframe_request: None,
            },
        );
        peer
    }

    /// `Some([10, n])` if the device received a display-power message within
    /// 200ms, `None` if the app left the panel alone.
    fn try_read_display_power(peer: &mut std::net::TcpStream) -> Option<[u8; 2]> {
        use std::io::Read;

        let mut msg = [0u8; 2];
        match peer.read_exact(&mut msg) {
            Ok(()) => Some(msg),
            Err(_) => None,
        }
    }

    /// Two fake devices (plain TCP pairs) stand in for control sockets: after
    /// `restore_all_displays` every peer must have received the display-power
    /// NORMAL message, and the registry must be emptied so no later teardown
    /// tries to reuse a socket that is already gone.
    #[test]
    fn restore_all_displays_sends_normal_power_to_every_socket() {
        let sockets = new_control_sockets();
        let mut peers: Vec<_> = ["device-a", "device-b"]
            .iter()
            .map(|serial| fake_control_socket(&sockets, serial))
            .collect();

        restore_all_displays(&sockets);

        assert!(sockets.lock().unwrap().is_empty());
        for peer in peers.iter_mut() {
            assert_eq!(
                try_read_display_power(peer),
                Some([10, 1]),
                "expected SET_DISPLAY_POWER / NORMAL"
            );
        }
    }

    /// The reported bug: a standalone scrcpy window turns the panel back on as
    /// it exits, cancelling the preview stream's one-shot `--turn-screen-off`.
    /// The stream must put it back.
    #[test]
    fn reassert_display_off_blanks_a_live_preview() {
        let sockets = new_control_sockets();
        let mut peer = fake_control_socket(&sockets, "reassert-live");

        reassert_display_off(&sockets, "reassert-live");

        assert_eq!(
            try_read_display_power(&mut peer),
            Some([10, 0]),
            "expected SET_DISPLAY_POWER / OFF"
        );
        // The stream keeps running — only the panel state was re-sent.
        assert!(sockets.lock().unwrap().contains_key("reassert-live"));
    }

    #[test]
    fn reassert_display_off_is_a_noop_without_a_preview_stream() {
        let sockets = new_control_sockets();

        // Nothing is mirroring this device, so the panel is the user's business.
        reassert_display_off(&sockets, "reassert-idle");

        assert!(sockets.lock().unwrap().is_empty());
    }

    /// Normal teardown: nothing else is mirroring, so the panel must come back.
    #[test]
    fn remove_control_socket_restores_the_panel() {
        let sockets = new_control_sockets();
        let mut peer = fake_control_socket(&sockets, "teardown-plain");

        remove_control_socket(&sockets, "teardown-plain", "test");

        assert!(sockets.lock().unwrap().is_empty());
        assert_eq!(
            try_read_display_power(&mut peer),
            Some([10, 1]),
            "expected SET_DISPLAY_POWER / NORMAL"
        );
    }

    /// Preview teardown while a standalone window is open must leave the panel
    /// dark: the window turned it off and will turn it back on when it closes.
    #[test]
    fn remove_control_socket_leaves_the_panel_dark_for_a_standalone_window() {
        let sockets = new_control_sockets();
        let mut peer = fake_control_socket(&sockets, "teardown-standalone");
        mark_standalone_scrcpy("teardown-standalone", true);

        remove_control_socket(&sockets, "teardown-standalone", "test");

        assert!(sockets.lock().unwrap().is_empty());
        assert_eq!(
            try_read_display_power(&mut peer),
            None,
            "the standalone window owns the panel, so nothing may be sent"
        );
        mark_standalone_scrcpy("teardown-standalone", false);
        assert!(!has_standalone_scrcpy("teardown-standalone"));
    }

    /// Same rule on app exit: only devices whose panel the app is holding dark.
    #[test]
    fn restore_all_displays_skips_devices_with_a_standalone_window() {
        let sockets = new_control_sockets();
        let mut plain = fake_control_socket(&sockets, "exit-plain");
        let mut standalone = fake_control_socket(&sockets, "exit-standalone");
        mark_standalone_scrcpy("exit-standalone", true);

        restore_all_displays(&sockets);

        assert!(sockets.lock().unwrap().is_empty());
        assert_eq!(try_read_display_power(&mut plain), Some([10, 1]));
        assert_eq!(
            try_read_display_power(&mut standalone),
            None,
            "the standalone window restores its own panel when it exits"
        );
        mark_standalone_scrcpy("exit-standalone", false);
    }
}
