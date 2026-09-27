/* libmicyou — micyou-gui frontend (original MicYou Vue UI). */

//! Shell-side file logger.
//!
//! Mirrors the stock GUI's logging contract: a plain-text log at
//! `<app_log_dir>/micyou.log` that `get_log_path` / `get_log_content` /
//! `export_log` / `open_log_dir` operate on. Deliberately minimal (append-only,
//! no rotation) — the daemon keeps its own structured log
//! (`~/.config/micyou/logs/daemon.log`).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};
use tauri::{AppHandle, Manager};

/// Name of the shell log file inside the platform app-log directory.
pub const LOG_FILE: &str = "micyou.log";

struct FileLogger {
    file: Mutex<Option<File>>,
}

impl FileLogger {
    fn new(path: &std::path::Path) -> Self {
        let file = path
            .parent()
            .map(fs::create_dir_all)
            .transpose()
            .ok()
            .flatten()
            .and_then(|()| {
                OpenOptions::new().create(true).append(true).open(path).ok()
            })
            .or_else(|| {
                eprintln!("[shelllog] cannot open {}: logging disabled", path.display());
                None
            });
        let logger = Self {
            file: Mutex::new(file),
        };
        logger.session_header();
        logger
    }

    fn session_header(&self) {
        self.write_line(&format!(
            "=== micyou-gui {} session start (pid {}) ===",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ));
    }

    fn write_line(&self, line: &str) {
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        let Some(file) = guard.as_mut() else {
            return;
        };
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "-".to_string());
        let line = format!(
            "{timestamp} {:>5} [{}] {}",
            record.level(),
            record.target(),
            record.args()
        );
        self.write_line(&line);
        // Debug runs also mirror to the terminal (`cargo run` diagnostics).
        if cfg!(debug_assertions) {
            eprintln!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// Absolute path of the shell log file.
pub fn log_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(LOG_FILE))
}

/// Install the global logger. Call once during setup, before any logging.
pub fn init(app: &AppHandle) {
    let Ok(path) = log_file_path(app) else {
        eprintln!("[shelllog] app log dir unavailable; logging disabled");
        return;
    };
    let logger = FileLogger::new(&path);
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}
