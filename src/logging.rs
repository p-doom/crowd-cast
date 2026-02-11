use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const LOG_FILE_BASENAME: &str = "crowd-cast.log";
const LOG_DIR_ENV: &str = "CROWD_CAST_LOG_PATH";
const LOG_RETENTION_DAYS: u64 = 7;

/// Bundle identifier matching Info.plist CFBundleIdentifier.
/// Used as the subsystem for macOS unified logging (os_log).
#[cfg(target_os = "macos")]
const OSLOG_SUBSYSTEM: &str = "dev.crowd-cast.agent";

/// Get the log directory path
pub fn get_log_dir() -> Result<PathBuf> {
    resolve_log_dir()
}

pub fn init_logging() -> Result<WorkerGuard> {
    let log_dir = resolve_log_dir()?;
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log directory: {:?}", log_dir))?;

    prune_old_logs(
        &log_dir,
        Duration::from_secs(60 * 60 * 24 * LOG_RETENTION_DAYS),
    );

    let file_appender = tracing_appender::rolling::daily(&log_dir, LOG_FILE_BASENAME);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);

    #[cfg(target_os = "macos")]
    {
        // On macOS, add unified logging (os_log) layer alongside file logging.
        // Logs will appear in Console.app and `log stream --predicate 'subsystem == "dev.crowd-cast.agent"'`
        let oslog_layer = tracing_oslog::OsLogger::new(OSLOG_SUBSYSTEM, "default");

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(oslog_layer)
            .init();
    }

    #[cfg(not(target_os = "macos"))]
    {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .init();
    }

    Ok(guard)
}

fn resolve_log_dir() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var(LOG_DIR_ENV) {
        return Ok(PathBuf::from(override_path));
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("Failed to determine home directory for log path")?;
        return Ok(home.join("Library").join("Logs").join("crowd-cast"));
    }

    let proj_dirs = ProjectDirs::from("dev", "crowd-cast", "agent")
        .context("Failed to determine project directories for log path")?;

    #[cfg(target_os = "windows")]
    {
        return Ok(proj_dirs.data_local_dir().join("Logs"));
    }

    #[cfg(target_os = "linux")]
    {
        let base = proj_dirs
            .state_dir()
            .unwrap_or_else(|| proj_dirs.data_local_dir());
        return Ok(base.join("logs"));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        return Ok(proj_dirs.data_local_dir().join("logs"));
    }
}

fn prune_old_logs(log_dir: &PathBuf, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };

    let cutoff = SystemTime::now().checked_sub(max_age);
    let Some(cutoff) = cutoff else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path.file_name().and_then(|name| name.to_str());
        let Some(file_name) = file_name else {
            continue;
        };

        if !file_name.starts_with(LOG_FILE_BASENAME) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        let Ok(modified) = metadata.modified() else {
            continue;
        };

        if modified < cutoff {
            let _ = std::fs::remove_file(&path);
        }
    }
}
