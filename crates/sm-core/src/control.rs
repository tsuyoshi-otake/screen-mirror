use crate::discovery::pin_hash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const CONTROL_PORT: u16 = 47778;
pub const CONTROL_PROTOCOL: &str = "screen-mirror.control";
pub const CONTROL_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlEvent {
    pub protocol: String,
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_hash: Option<String>,
    pub action: TouchAction,
    pub x: f32,
    pub y: f32,
    pub pointer_id: i32,
    pub timestamp_ms: u64,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
}

impl ControlEvent {
    pub fn touch(action: TouchAction, x: f32, y: f32, pointer_id: i32, timestamp_ms: u64) -> Self {
        Self::touch_with_pin(action, x, y, pointer_id, timestamp_ms, None)
            .expect("touch event without PIN must be valid")
    }

    pub fn touch_with_pin(
        action: TouchAction,
        x: f32,
        y: f32,
        pointer_id: i32,
        timestamp_ms: u64,
        pin: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            protocol: CONTROL_PROTOCOL.to_string(),
            version: CONTROL_VERSION,
            pin_hash: pin.map(pin_hash).transpose()?,
            action,
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
            pointer_id,
            timestamp_ms,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to encode control event")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let event: Self =
            serde_json::from_slice(bytes).context("failed to decode control event")?;
        anyhow::ensure!(
            event.protocol == CONTROL_PROTOCOL,
            "unexpected control protocol"
        );
        anyhow::ensure!(
            event.version == CONTROL_VERSION,
            "unsupported control version"
        );
        Ok(event)
    }
}
