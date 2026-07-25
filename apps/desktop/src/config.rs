use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::pipeline::{CaptureApi, Decoder, Encoder, RecvArgs, SendArgs, Sink};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub startup_mode: StartupMode,
    pub autostart: bool,
    pub send: SendConfig,
    pub recv: RecvConfig,
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartupMode {
    Idle,
    Sender,
    Receiver,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SendConfig {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_max_receivers")]
    pub max_receivers: u32,
    pub monitor_index: i32,
    pub fps: u32,
    pub bitrate: u32,
    pub mtu: u32,
    pub encoder: ConfigEncoder,
    pub capture_api: ConfigCaptureApi,
    pub show_cursor: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn default_max_receivers() -> u32 {
    3
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecvConfig {
    pub port: u16,
    pub jitter_ms: u32,
    pub decoder: ConfigDecoder,
    pub sink: ConfigSink,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigEncoder {
    Auto,
    Nvidia,
    MediaFoundation,
    QuickSync,
    X264,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigDecoder {
    Auto,
    D3d11,
    Avdec,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigSink {
    Auto,
    D3d11,
    AutoVideo,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigCaptureApi {
    Dxgi,
    Wgc,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            startup_mode: StartupMode::Idle,
            autostart: false,
            send: SendConfig::default(),
            recv: RecvConfig::default(),
        }
    }
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            host: "auto".to_string(),
            port: 5004,
            max_receivers: 3,
            monitor_index: -1,
            fps: 60,
            bitrate: 12_000,
            mtu: 1200,
            encoder: ConfigEncoder::Auto,
            capture_api: ConfigCaptureApi::Dxgi,
            show_cursor: true,
            width: None,
            height: None,
        }
    }
}

impl Default for RecvConfig {
    fn default() -> Self {
        Self {
            port: 5004,
            jitter_ms: 20,
            decoder: ConfigDecoder::Auto,
            sink: ConfigSink::Auto,
        }
    }
}

impl AppConfig {
    pub fn load_or_create() -> Result<(Self, PathBuf)> {
        let path = config_path()?;
        if !path.exists() {
            let config = Self::default();
            config.save_to(&path)?;
            return Ok((config, path));
        }

        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let config = toml::from_str(&text)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        Ok((config, path))
    }

    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(path, text).with_context(|| format!("failed to write config: {}", path.display()))
    }
}

impl From<SendConfig> for SendArgs {
    fn from(config: SendConfig) -> Self {
        Self {
            host: config.host,
            port: config.port,
            max_receivers: config.max_receivers,
            monitor_index: config.monitor_index,
            fps: config.fps,
            bitrate: config.bitrate,
            mtu: config.mtu,
            encoder: config.encoder.into(),
            capture_api: config.capture_api.into(),
            no_cursor: !config.show_cursor,
            width: config.width,
            height: config.height,
        }
    }
}

impl From<RecvConfig> for RecvArgs {
    fn from(config: RecvConfig) -> Self {
        Self {
            port: config.port,
            jitter_ms: config.jitter_ms,
            decoder: config.decoder.into(),
            sink: config.sink.into(),
        }
    }
}

impl From<ConfigEncoder> for Encoder {
    fn from(value: ConfigEncoder) -> Self {
        match value {
            ConfigEncoder::Auto => Self::Auto,
            ConfigEncoder::Nvidia => Self::Nvidia,
            ConfigEncoder::MediaFoundation => Self::MediaFoundation,
            ConfigEncoder::QuickSync => Self::QuickSync,
            ConfigEncoder::X264 => Self::X264,
        }
    }
}

impl From<ConfigDecoder> for Decoder {
    fn from(value: ConfigDecoder) -> Self {
        match value {
            ConfigDecoder::Auto => Self::Auto,
            ConfigDecoder::D3d11 => Self::D3d11,
            ConfigDecoder::Avdec => Self::Avdec,
        }
    }
}

impl From<ConfigSink> for Sink {
    fn from(value: ConfigSink) -> Self {
        match value {
            ConfigSink::Auto => Self::Auto,
            ConfigSink::D3d11 => Self::D3d11,
            ConfigSink::AutoVideo => Self::AutoVideo,
        }
    }
}

impl From<ConfigCaptureApi> for CaptureApi {
    fn from(value: ConfigCaptureApi) -> Self {
        match value {
            ConfigCaptureApi::Dxgi => Self::Dxgi,
            ConfigCaptureApi::Wgc => Self::Wgc,
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("failed to resolve config directory")?;
    Ok(base.join("screen-mirror").join("config.toml"))
}
