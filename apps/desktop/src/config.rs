use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sm_core::discovery::DEFAULT_PIN;
use std::fs;
use std::path::PathBuf;

use crate::pipeline::{CaptureApi, Decoder, Encoder, NvidiaTuning, RecvArgs, SendArgs, Sink};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub startup_mode: StartupMode,
    pub autostart: bool,
    #[serde(default)]
    pub security: SecurityConfig,
    pub send: SendConfig,
    pub recv: RecvConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SecurityConfig {
    #[serde(default = "default_pin")]
    pub pin: String,
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
    #[serde(default = "default_prefer_virtual_display")]
    pub prefer_virtual_display: bool,
    #[serde(default = "default_enable_virtual_display")]
    pub enable_virtual_display: bool,
    #[serde(default = "default_sync_virtual_display_resolution")]
    pub sync_virtual_display_resolution: bool,
    pub monitor_index: i32,
    pub fps: u32,
    pub bitrate: u32,
    pub mtu: u32,
    #[serde(default = "default_udp_buffer_size")]
    pub udp_buffer_size: u32,
    #[serde(default = "default_qos_dscp")]
    pub qos_dscp: i32,
    #[serde(default)]
    pub allow_software_encoder: bool,
    #[serde(default)]
    pub nvidia_tuning: ConfigNvidiaTuning,
    pub encoder: ConfigEncoder,
    pub capture_api: ConfigCaptureApi,
    pub show_cursor: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn default_max_receivers() -> u32 {
    3
}

fn default_prefer_virtual_display() -> bool {
    true
}

fn default_enable_virtual_display() -> bool {
    true
}

fn default_sync_virtual_display_resolution() -> bool {
    true
}

fn default_mtu() -> u32 {
    1200
}

fn default_udp_buffer_size() -> u32 {
    4 * 1024 * 1024
}

fn default_qos_dscp() -> i32 {
    -1
}

fn default_pin() -> String {
    DEFAULT_PIN.to_string()
}

fn default_jitter_faststart_packets() -> u32 {
    2
}

fn default_jitter_max_dropout_ms() -> u32 {
    200
}

fn default_jitter_max_misorder_ms() -> u32 {
    50
}

fn default_receiver_fullscreen() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecvConfig {
    pub port: u16,
    pub jitter_ms: u32,
    #[serde(default = "default_udp_buffer_size")]
    pub udp_buffer_size: u32,
    #[serde(default = "default_mtu")]
    pub mtu: u32,
    #[serde(default = "default_jitter_faststart_packets")]
    pub jitter_faststart_packets: u32,
    #[serde(default = "default_jitter_max_dropout_ms")]
    pub jitter_max_dropout_ms: u32,
    #[serde(default = "default_jitter_max_misorder_ms")]
    pub jitter_max_misorder_ms: u32,
    #[serde(default = "default_receiver_fullscreen")]
    pub fullscreen: bool,
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

#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigNvidiaTuning {
    #[default]
    Auto,
    Gtx,
    Rtx,
    LowLatency,
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
            autostart: true,
            security: SecurityConfig::default(),
            send: SendConfig::default(),
            recv: RecvConfig::default(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self { pin: default_pin() }
    }
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            host: "auto".to_string(),
            port: 5004,
            max_receivers: 3,
            prefer_virtual_display: true,
            enable_virtual_display: true,
            sync_virtual_display_resolution: true,
            monitor_index: -1,
            fps: 60,
            bitrate: 12_000,
            mtu: default_mtu(),
            udp_buffer_size: default_udp_buffer_size(),
            qos_dscp: default_qos_dscp(),
            allow_software_encoder: false,
            nvidia_tuning: ConfigNvidiaTuning::Auto,
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
            jitter_ms: 15,
            udp_buffer_size: default_udp_buffer_size(),
            mtu: default_mtu(),
            jitter_faststart_packets: default_jitter_faststart_packets(),
            jitter_max_dropout_ms: default_jitter_max_dropout_ms(),
            jitter_max_misorder_ms: default_jitter_max_misorder_ms(),
            fullscreen: true,
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
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        sm_core::discovery::normalize_pin(&config.security.pin)
            .with_context(|| format!("invalid PIN in config: {}", path.display()))?;
        Ok((config, path))
    }

    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }
        let text = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(path, text).with_context(|| format!("failed to write config: {}", path.display()))
    }

    pub fn send_args(&self) -> SendArgs {
        let mut args: SendArgs = self.send.clone().into();
        args.pin = self.security.pin.clone();
        args
    }

    pub fn recv_args(&self) -> RecvArgs {
        let mut args: RecvArgs = self.recv.clone().into();
        args.pin = self.security.pin.clone();
        args
    }
}

impl From<SendConfig> for SendArgs {
    fn from(config: SendConfig) -> Self {
        Self {
            host: config.host,
            port: config.port,
            max_receivers: config.max_receivers,
            pin: DEFAULT_PIN.to_string(),
            prefer_virtual_display: config.prefer_virtual_display,
            enable_virtual_display: config.enable_virtual_display,
            sync_virtual_display_resolution: config.sync_virtual_display_resolution,
            monitor_index: config.monitor_index,
            fps: config.fps,
            bitrate: config.bitrate,
            mtu: config.mtu,
            udp_buffer_size: config.udp_buffer_size,
            qos_dscp: config.qos_dscp,
            allow_software_encoder: config.allow_software_encoder,
            nvidia_tuning: config.nvidia_tuning.into(),
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
            pin: DEFAULT_PIN.to_string(),
            jitter_ms: config.jitter_ms,
            udp_buffer_size: config.udp_buffer_size,
            mtu: config.mtu,
            jitter_faststart_packets: config.jitter_faststart_packets,
            jitter_max_dropout_ms: config.jitter_max_dropout_ms,
            jitter_max_misorder_ms: config.jitter_max_misorder_ms,
            fullscreen: config.fullscreen,
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

impl From<ConfigNvidiaTuning> for NvidiaTuning {
    fn from(value: ConfigNvidiaTuning) -> Self {
        match value {
            ConfigNvidiaTuning::Auto => Self::Auto,
            ConfigNvidiaTuning::Gtx => Self::Gtx,
            ConfigNvidiaTuning::Rtx => Self::Rtx,
            ConfigNvidiaTuning::LowLatency => Self::LowLatency,
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
