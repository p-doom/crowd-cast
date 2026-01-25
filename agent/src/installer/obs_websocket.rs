//! OBS WebSocket configuration helpers

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

use crate::config::Config;
use super::obs_detector::OBSInstallation;

pub struct WebSocketConfigResult {
    pub updated: bool,
}

pub fn ensure_obs_websocket_config(
    obs: &OBSInstallation,
    config: &mut Config,
) -> Result<WebSocketConfigResult> {
    let config_dir = obs
        .data_dir
        .join("plugin_config")
        .join("obs-websocket");
    let config_path = config_dir.join("config.json");

    std::fs::create_dir_all(&config_dir).with_context(|| {
        format!(
            "Failed to create OBS WebSocket config directory: {:?}",
            config_dir
        )
    })?;

    let mut json = read_config_json(&config_path)?;
    let existing_password = json
        .get("server_password")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let desired_password = config
        .obs
        .password
        .clone()
        .or(existing_password)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut updated = false;
    updated |= set_json_bool(&mut json, "server_enabled", true);
    updated |= set_json_bool(&mut json, "auth_required", true);
    updated |= set_json_u16(&mut json, "server_port", config.obs.port);
    updated |= set_json_string(&mut json, "server_password", &desired_password);

    if config.obs.password.as_deref() != Some(desired_password.as_str()) {
        config.obs.password = Some(desired_password);
        config.save().context("Failed to update agent config with WebSocket password")?;
    }

    if updated {
        write_config_json(&config_path, &json)?;
    }

    Ok(WebSocketConfigResult { updated })
}

fn read_config_json(path: &PathBuf) -> Result<Value> {
    if path.exists() {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read OBS WebSocket config: {:?}", path))?;
        let json = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse OBS WebSocket config: {:?}", path))?;
        Ok(json)
    } else {
        Ok(Value::Object(serde_json::Map::new()))
    }
}

fn write_config_json(path: &PathBuf, json: &Value) -> Result<()> {
    let contents = serde_json::to_string_pretty(json)
        .context("Failed to serialize OBS WebSocket config")?;
    std::fs::write(path, contents)
        .with_context(|| format!("Failed to write OBS WebSocket config: {:?}", path))?;
    Ok(())
}

fn set_json_bool(json: &mut Value, key: &str, value: bool) -> bool {
    match json.get(key).and_then(|v| v.as_bool()) {
        Some(current) if current == value => false,
        _ => {
            json[key] = Value::Bool(value);
            true
        }
    }
}

fn set_json_u16(json: &mut Value, key: &str, value: u16) -> bool {
    match json.get(key).and_then(|v| v.as_u64()) {
        Some(current) if current == value as u64 => false,
        _ => {
            json[key] = Value::Number(value.into());
            true
        }
    }
}

fn set_json_string(json: &mut Value, key: &str, value: &str) -> bool {
    match json.get(key).and_then(|v| v.as_str()) {
        Some(current) if current == value => false,
        _ => {
            json[key] = Value::String(value.to_string());
            true
        }
    }
}
