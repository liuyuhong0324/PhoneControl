use std::path::PathBuf;
use std::sync::OnceLock;

static ADB_PATH: OnceLock<PathBuf> = OnceLock::new();

fn adb_file() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Resolve the bundled adb, falling back to PATH lookup.
///
/// Order:
/// 1. `ADB_PATH` env override
/// 2. `<exe_dir>/platform-tools/` — bundled layout (resources are installed
///    next to the exe on Windows)
/// 3. `<exe_dir>/../../../platform-tools/` — dev layout
///    (`src-tauri/target/<profile>` → repo root)
/// 4. bare `adb` from PATH — previous behavior, keeps macOS working
fn resolve() -> PathBuf {
    if let Ok(p) = std::env::var("ADB_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
    }

    if let Some(dir) = exe_dir() {
        // Bundled (installed) layout
        let bundled = dir.join("platform-tools").join(adb_file());
        if bundled.is_file() {
            return bundled;
        }

        // Dev layout
        let dev = dir.join("../../..").join("platform-tools").join(adb_file());
        if dev.is_file() {
            return dev;
        }
    }

    PathBuf::from("adb")
}

/// Path to the adb binary to invoke.
pub fn adb_path() -> PathBuf {
    ADB_PATH.get_or_init(resolve).clone()
}
