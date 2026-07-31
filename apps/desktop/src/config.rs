use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sm_core::discovery::DEFAULT_PIN;
use std::fs;
use std::path::PathBuf;

use crate::pipeline::{
    CaptureApi, Decoder, Encoder, NvidiaTuning, RecvArgs, Sampling, SendArgs, Sink,
};

const CURRENT_CONFIG_VERSION: u32 = 4;
const BALANCED_RESOURCE_CONFIG_VERSION: u32 = 2;
const LOW_LATENCY_AUDIO_CONFIG_VERSION: u32 = 3;
const STABLE_LOW_LATENCY_AUDIO_CONFIG_VERSION: u32 = 4;
const LEGACY_DEFAULT_FPS: u32 = 60;
const LEGACY_DEFAULT_BITRATE: u32 = 12_000;
const LEGACY_DEFAULT_UDP_BUFFER_SIZE: u32 = 4 * 1024 * 1024;
const LEGACY_DEFAULT_AUDIO_FRAME_MS: &str = "5";
const LEGACY_DEFAULT_AUDIO_JITTER_MS: [u32; 2] = [5, 15];
const AGGRESSIVE_DEFAULT_AUDIO_FRAME_MS: &str = "2.5";
const AGGRESSIVE_DEFAULT_AUDIO_JITTER_MS: u32 = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default)]
    pub config_version: u32,
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
    #[serde(default)]
    pub audio_enabled: bool,
    #[serde(default = "default_audio_port")]
    pub audio_port: u16,
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate: u32,
    #[serde(default = "default_audio_frame_ms")]
    pub audio_frame_ms: String,
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
    /// GPU to encode on: "auto", a DXGI adapter index, or part of the adapter name.
    #[serde(default = "default_gpu")]
    pub gpu: String,
    pub capture_api: ConfigCaptureApi,
    #[serde(default = "default_zero_copy")]
    pub zero_copy: bool,
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
    1024 * 1024
}

fn default_zero_copy() -> bool {
    true
}

fn default_qos_dscp() -> i32 {
    -1
}

fn default_gpu() -> String {
    crate::gpu::AUTO.to_string()
}

fn default_pin() -> String {
    DEFAULT_PIN.to_string()
}

fn default_audio_port() -> u16 {
    5005
}

fn default_audio_bitrate() -> u32 {
    96_000
}

fn default_audio_frame_ms() -> String {
    "5".to_string()
}

fn default_audio_jitter_ms() -> u32 {
    10
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
    #[serde(default)]
    pub audio_enabled: bool,
    #[serde(default = "default_audio_port")]
    pub audio_port: u16,
    #[serde(default = "default_audio_jitter_ms")]
    pub audio_jitter_ms: u32,
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
    /// Texture filter the sink scales with: auto, linear, or point.
    #[serde(default)]
    pub sampling: ConfigSampling,
    /// GPU to decode and render on: "auto", a DXGI adapter index, or part of the adapter name.
    #[serde(default = "default_gpu")]
    pub gpu: String,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigEncoder {
    Auto,
    Nvidia,
    Amf,
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
    Software,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigSink {
    Auto,
    D3d11,
    AutoVideo,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigSampling {
    #[default]
    Auto,
    Linear,
    Point,
}

impl ConfigSampling {
    /// The value as it is spelled in config.toml, which is also what the tray menu ids are built
    /// from so a click and a hand-edited file cannot disagree about what a mode is called.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Linear => "linear",
            Self::Point => "point",
        }
    }
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
            config_version: CURRENT_CONFIG_VERSION,
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
            audio_enabled: false,
            audio_port: default_audio_port(),
            audio_bitrate: default_audio_bitrate(),
            audio_frame_ms: default_audio_frame_ms(),
            max_receivers: 3,
            prefer_virtual_display: true,
            enable_virtual_display: true,
            sync_virtual_display_resolution: true,
            monitor_index: -1,
            fps: 30,
            bitrate: 8_000,
            mtu: default_mtu(),
            udp_buffer_size: default_udp_buffer_size(),
            qos_dscp: default_qos_dscp(),
            allow_software_encoder: false,
            nvidia_tuning: ConfigNvidiaTuning::Auto,
            encoder: ConfigEncoder::Auto,
            gpu: default_gpu(),
            capture_api: ConfigCaptureApi::Dxgi,
            zero_copy: true,
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
            audio_enabled: false,
            audio_port: default_audio_port(),
            audio_jitter_ms: default_audio_jitter_ms(),
            jitter_ms: 15,
            udp_buffer_size: default_udp_buffer_size(),
            mtu: default_mtu(),
            jitter_faststart_packets: default_jitter_faststart_packets(),
            jitter_max_dropout_ms: default_jitter_max_dropout_ms(),
            jitter_max_misorder_ms: default_jitter_max_misorder_ms(),
            fullscreen: true,
            decoder: ConfigDecoder::Auto,
            sink: ConfigSink::Auto,
            sampling: ConfigSampling::Auto,
            gpu: default_gpu(),
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
        let mut config: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        sm_core::discovery::normalize_pin(&config.security.pin)
            .with_context(|| format!("invalid PIN in config: {}", path.display()))?;
        if migrate_legacy_defaults(&mut config) {
            config.save_to(&path)?;
            crate::logging::append(format!(
                "config migrated to version {} with balanced resource and low-latency audio defaults",
                CURRENT_CONFIG_VERSION
            ));
        }
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
            audio_enabled: config.audio_enabled,
            audio_port: config.audio_port,
            audio_bitrate: config.audio_bitrate,
            audio_frame_ms: config.audio_frame_ms,
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
            gpu: config.gpu,
            capture_api: config.capture_api.into(),
            zero_copy: config.zero_copy,
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
            audio_enabled: config.audio_enabled,
            audio_port: config.audio_port,
            audio_jitter_ms: config.audio_jitter_ms,
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
            sampling: config.sampling.into(),
            gpu: config.gpu,
        }
    }
}

impl From<ConfigEncoder> for Encoder {
    fn from(value: ConfigEncoder) -> Self {
        match value {
            ConfigEncoder::Auto => Self::Auto,
            ConfigEncoder::Nvidia => Self::Nvidia,
            ConfigEncoder::Amf => Self::Amf,
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
            ConfigDecoder::Software => Self::Software,
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

impl From<ConfigSampling> for Sampling {
    fn from(value: ConfigSampling) -> Self {
        match value {
            ConfigSampling::Auto => Self::Auto,
            ConfigSampling::Linear => Self::Linear,
            ConfigSampling::Point => Self::Point,
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

fn migrate_legacy_defaults(config: &mut AppConfig) -> bool {
    if config.config_version >= CURRENT_CONFIG_VERSION {
        return false;
    }

    if config.config_version < BALANCED_RESOURCE_CONFIG_VERSION {
        if config.send.fps == LEGACY_DEFAULT_FPS {
            config.send.fps = 30;
        }
        if config.send.bitrate == LEGACY_DEFAULT_BITRATE {
            config.send.bitrate = 8_000;
        }
        if config.send.udp_buffer_size == LEGACY_DEFAULT_UDP_BUFFER_SIZE {
            config.send.udp_buffer_size = default_udp_buffer_size();
        }
        if config.recv.udp_buffer_size == LEGACY_DEFAULT_UDP_BUFFER_SIZE {
            config.recv.udp_buffer_size = default_udp_buffer_size();
        }
    }
    if config.config_version < LOW_LATENCY_AUDIO_CONFIG_VERSION {
        if config.send.audio_frame_ms == LEGACY_DEFAULT_AUDIO_FRAME_MS {
            config.send.audio_frame_ms = default_audio_frame_ms();
        }
        if LEGACY_DEFAULT_AUDIO_JITTER_MS.contains(&config.recv.audio_jitter_ms) {
            config.recv.audio_jitter_ms = default_audio_jitter_ms();
        }
    }
    if config.config_version < STABLE_LOW_LATENCY_AUDIO_CONFIG_VERSION {
        if config.send.audio_frame_ms == AGGRESSIVE_DEFAULT_AUDIO_FRAME_MS {
            config.send.audio_frame_ms = default_audio_frame_ms();
        }
        if config.recv.audio_jitter_ms == AGGRESSIVE_DEFAULT_AUDIO_JITTER_MS {
            config.recv.audio_jitter_ms = default_audio_jitter_ms();
        }
    }
    config.config_version = CURRENT_CONFIG_VERSION;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_defaults_migrate_to_balanced_resource_usage() {
        let mut config = AppConfig {
            config_version: 0,
            ..AppConfig::default()
        };
        config.send.fps = LEGACY_DEFAULT_FPS;
        config.send.bitrate = LEGACY_DEFAULT_BITRATE;
        config.send.udp_buffer_size = LEGACY_DEFAULT_UDP_BUFFER_SIZE;
        config.recv.udp_buffer_size = LEGACY_DEFAULT_UDP_BUFFER_SIZE;
        config.send.audio_frame_ms = LEGACY_DEFAULT_AUDIO_FRAME_MS.to_string();
        config.recv.audio_jitter_ms = 15;

        assert!(migrate_legacy_defaults(&mut config));
        assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.send.fps, 30);
        assert_eq!(config.send.bitrate, 8_000);
        assert_eq!(config.send.udp_buffer_size, 1024 * 1024);
        assert_eq!(config.recv.udp_buffer_size, 1024 * 1024);
        assert_eq!(config.send.audio_frame_ms, "5");
        assert_eq!(config.recv.audio_jitter_ms, 10);
        assert!(!migrate_legacy_defaults(&mut config));
    }

    #[test]
    fn version_two_audio_defaults_migrate_to_stable_low_latency() {
        for legacy_jitter_ms in LEGACY_DEFAULT_AUDIO_JITTER_MS {
            let mut config = AppConfig {
                config_version: BALANCED_RESOURCE_CONFIG_VERSION,
                ..AppConfig::default()
            };
            config.send.audio_frame_ms = LEGACY_DEFAULT_AUDIO_FRAME_MS.to_string();
            config.recv.audio_jitter_ms = legacy_jitter_ms;

            assert!(migrate_legacy_defaults(&mut config));
            assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
            assert_eq!(config.send.audio_frame_ms, "5");
            assert_eq!(config.recv.audio_jitter_ms, 10);
        }
    }

    #[test]
    fn version_three_aggressive_audio_defaults_migrate_to_stable_low_latency() {
        let mut config = AppConfig {
            config_version: LOW_LATENCY_AUDIO_CONFIG_VERSION,
            ..AppConfig::default()
        };
        config.send.audio_frame_ms = AGGRESSIVE_DEFAULT_AUDIO_FRAME_MS.to_string();
        config.recv.audio_jitter_ms = AGGRESSIVE_DEFAULT_AUDIO_JITTER_MS;

        assert!(migrate_legacy_defaults(&mut config));
        assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.send.audio_frame_ms, "5");
        assert_eq!(config.recv.audio_jitter_ms, 10);
    }

    #[test]
    fn explicit_nondefault_values_survive_migration() {
        let mut config = AppConfig {
            config_version: 0,
            ..AppConfig::default()
        };
        config.send.fps = 45;
        config.send.bitrate = 10_000;
        config.send.udp_buffer_size = 2 * 1024 * 1024;
        config.send.audio_frame_ms = "10".to_string();
        config.recv.audio_jitter_ms = 12;

        assert!(migrate_legacy_defaults(&mut config));
        assert_eq!(config.send.fps, 45);
        assert_eq!(config.send.bitrate, 10_000);
        assert_eq!(config.send.udp_buffer_size, 2 * 1024 * 1024);
        assert_eq!(config.send.audio_frame_ms, "10");
        assert_eq!(config.recv.audio_jitter_ms, 12);
    }
}
