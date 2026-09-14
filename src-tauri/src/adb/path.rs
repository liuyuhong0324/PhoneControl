use std::path::PathBuf;
use std::sync::OnceLock;

static TOOL_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

fn adb_file() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

fn scrcpy_file() -> &'static str {
    if cfg!(windows) {
        "scrcpy.exe"
    } else {
        "scrcpy"
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Directory holding the bundled tools (adb, scrcpy, scrcpy-server).
///
/// Order:
/// 1. `<exe_dir>/scrcpy/` — bundled layout (resources are installed next to
///    the exe on Windows)
/// 2. `<exe_dir>/../../../scrcpy/` — dev layout
///    (`src-tauri/target/<profile>` → repo root)
/// 3. `None` — fall back to PATH lookup
fn resolve_dir() -> Option<PathBuf> {
    let exe_dir = exe_dir()?;

    // Bundled (installed) layout
    let bundled = exe_dir.join("scrcpy");
    if bundled.join(adb_file()).is_file() {
        return Some(bundled);
    }

    // Dev layout
    let dev = exe_dir.join("../../..").join("scrcpy");
    if dev.join(adb_file()).is_file() {
        return Some(dev);
    }

    None
}

/// Directory of the bundled scrcpy (also holds adb and scrcpy-server), if found.
pub fn tool_dir() -> Option<&'static std::path::Path> {
    TOOL_DIR.get_or_init(resolve_dir).as_deref()
}

/// Path to the adb binary to invoke. Falls back to bare `adb` (PATH lookup).
pub fn adb_path() -> PathBuf {
    if let Ok(p) = std::env::var("ADB_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
    }
    tool_dir()
        .map(|d| d.join(adb_file()))
        .unwrap_or_else(|| PathBuf::from("adb"))
}

/// Path to the scrcpy binary to invoke. Falls back to bare `scrcpy` (PATH lookup).
pub fn scrcpy_path() -> PathBuf {
    if let Ok(p) = std::env::var("SCRCPY_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
    }
    tool_dir()
        .map(|d| d.join(scrcpy_file()))
        .unwrap_or_else(|| PathBuf::from("scrcpy"))
}

/// Path to the bundled scrcpy-server binary, if present.
pub fn scrcpy_server_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SCRCPY_SERVER_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    tool_dir().map(|d| d.join("scrcpy-server")).filter(|p| p.is_file())
}

/// Apply CREATE_NO_WINDOW so console subprocesses (adb, scrcpy) don't flash
/// a window on every spawn in the release build (windows_subsystem = "windows").
#[cfg(windows)]
pub fn hide_window(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
pub fn hide_window(_cmd: &mut std::process::Command) {}

/// Same for tokio commands.
#[cfg(windows)]
pub fn hide_window_tokio(cmd: &mut tokio::process::Command) {
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
pub fn hide_window_tokio(_cmd: &mut tokio::process::Command) {}
