use crate::discovery::pin_hash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const CONTROL_PORT: u16 = 47778;
pub const CONTROL_PROTOCOL: &str = "screen-mirror.control";
pub const STREAM_FEEDBACK_PROTOCOL: &str = "screen-mirror.feedback";
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

/// Receiver-side stream health measured over a recent reporting window.
///
/// This message shares [`CONTROL_PORT`] with touch control, but has its own protocol identifier so
/// a listener can distinguish it without changing the established `ControlEvent` JSON schema.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StreamFeedback {
    pub protocol: String,
    pub version: u16,
    pub pin_hash: String,
    pub timestamp_ms: u64,
    pub window_ms: u64,
    pub received_packets: u64,
    pub lost_packets: u64,
    pub late_packets: u64,
    pub duplicate_packets: u64,
    pub decoded_frames: u64,
    pub displayed_frames: u64,
    pub jitter_ms: u32,
}

impl StreamFeedback {
    #[allow(clippy::too_many_arguments)]
    pub fn with_pin(
        pin: &str,
        timestamp_ms: u64,
        window_ms: u64,
        received_packets: u64,
        lost_packets: u64,
        late_packets: u64,
        duplicate_packets: u64,
        decoded_frames: u64,
        displayed_frames: u64,
        jitter_ms: u32,
    ) -> Result<Self> {
        Ok(Self {
            protocol: STREAM_FEEDBACK_PROTOCOL.to_string(),
            version: CONTROL_VERSION,
            pin_hash: pin_hash(pin)?,
            timestamp_ms,
            window_ms,
            received_packets,
            lost_packets,
            late_packets,
            duplicate_packets,
            decoded_frames,
            displayed_frames,
            jitter_ms,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to encode stream feedback")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let feedback: Self =
            serde_json::from_slice(bytes).context("failed to decode stream feedback")?;
        anyhow::ensure!(
            feedback.protocol == STREAM_FEEDBACK_PROTOCOL,
            "unexpected stream feedback protocol"
        );
        anyhow::ensure!(
            feedback.version == CONTROL_VERSION,
            "unsupported stream feedback version"
        );
        Ok(feedback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_feedback_with_pin_round_trips_as_its_own_protocol() {
        let feedback = StreamFeedback::with_pin("1234", 1_000, 500, 100, 4, 2, 1, 58, 56, 7)
            .expect("valid PIN");

        let encoded = feedback.encode().expect("encode feedback");
        let json: serde_json::Value =
            serde_json::from_slice(&encoded).expect("valid feedback JSON");
        assert_eq!(json["protocol"], STREAM_FEEDBACK_PROTOCOL);
        assert_eq!(json["version"], CONTROL_VERSION);
        assert_eq!(json["pin_hash"], pin_hash("1234").expect("valid PIN"));

        let decoded = StreamFeedback::decode(&encoded).expect("decode feedback");
        assert_eq!(decoded.protocol, STREAM_FEEDBACK_PROTOCOL);
        assert_eq!(decoded.timestamp_ms, 1_000);
        assert_eq!(decoded.window_ms, 500);
        assert_eq!(decoded.received_packets, 100);
        assert_eq!(decoded.lost_packets, 4);
        assert_eq!(decoded.late_packets, 2);
        assert_eq!(decoded.duplicate_packets, 1);
        assert_eq!(decoded.decoded_frames, 58);
        assert_eq!(decoded.displayed_frames, 56);
        assert_eq!(decoded.jitter_ms, 7);
    }

    #[test]
    fn stream_feedback_decode_rejects_another_protocol_or_version() {
        let feedback =
            StreamFeedback::with_pin("1234", 1, 1, 1, 0, 0, 0, 1, 1, 0).expect("valid PIN");

        let mut wrong_protocol = feedback.clone();
        wrong_protocol.protocol = CONTROL_PROTOCOL.to_string();
        assert!(
            StreamFeedback::decode(&wrong_protocol.encode().expect("encode feedback")).is_err()
        );

        let mut wrong_version = feedback;
        wrong_version.version = CONTROL_VERSION + 1;
        assert!(StreamFeedback::decode(&wrong_version.encode().expect("encode feedback")).is_err());
    }

    #[test]
    fn existing_control_event_json_remains_decodable() {
        let json = br#"{"protocol":"screen-mirror.control","version":1,"action":"down","x":0.25,"y":0.5,"pointer_id":7,"timestamp_ms":42}"#;
        let event = ControlEvent::decode(json).expect("legacy control event remains valid");

        assert!(event.pin_hash.is_none());
        assert!(matches!(event.action, TouchAction::Down));
        assert_eq!(event.pointer_id, 7);
    }
}
