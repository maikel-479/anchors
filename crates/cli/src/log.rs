use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Off = 0,
    Error = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Off as u8);
static FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn file_lock() -> &'static Mutex<Option<std::fs::File>> {
    FILE.get_or_init(|| Mutex::new(None))
}

pub fn init() {
    let level = std::env::var("CANC_LOG")
        .ok()
        .and_then(|v| parse_level(&v))
        .unwrap_or(Level::Off);
    LEVEL.store(level as u8, Ordering::Relaxed);

    if level != Level::Off {
        if let Ok(path) = std::env::var("CANC_LOG_FILE") {
            if !path.is_empty() {
                if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
                    if let Ok(mut guard) = file_lock().lock() {
                        *guard = Some(f);
                    }
                }
            }
        }
        info(&format!("log initialized level={}", level_name(level)));
    }
}

pub fn set_level(level: Level) {
    let prev = LEVEL.load(Ordering::Relaxed);
    LEVEL.store(level as u8, Ordering::Relaxed);
    if prev == Level::Off as u8 && level != Level::Off {
        // ensure file lock exists
        let _ = file_lock();
    }
}

pub fn enabled(level: Level) -> bool {
    LEVEL.load(Ordering::Relaxed) >= level as u8
}

fn parse_level(s: &str) -> Option<Level> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "0" | "none" => Some(Level::Off),
        "error" | "err" | "1" => Some(Level::Error),
        "info" | "2" => Some(Level::Info),
        "debug" | "3" => Some(Level::Debug),
        "trace" | "4" => Some(Level::Trace),
        _ => None,
    }
}

pub fn parse_level_public(s: &str) -> Option<Level> {
    parse_level(s)
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Off => "off",
        Level::Error => "error",
        Level::Info => "info",
        Level::Debug => "debug",
        Level::Trace => "trace",
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn emit(level: Level, msg: &str) {
    if !enabled(level) {
        return;
    }
    let line = format!("[canc {} {}ms {}] {msg}", level_name(level), now_ms(), std::process::id());
    eprintln!("{line}");
    if let Ok(mut guard) = file_lock().lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

pub fn error(msg: &str) { emit(Level::Error, msg); }
pub fn info(msg: &str) { emit(Level::Info, msg); }
pub fn debug(msg: &str) { emit(Level::Debug, msg); }
pub fn trace(msg: &str) { emit(Level::Trace, msg); }
