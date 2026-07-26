use crate::discovery::pin_hash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DIAGNOSTICS_PORT: u16 = 47779;
pub const DIAGNOSTICS_PROTOCOL: &str = "screen-mirror.diagnostics";
pub const DIAGNOSTICS_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiagnosticsRequest {
    pub protocol: String,
    pub version: u16,
    pub request_id: String,
    pub pin_hash: String,
    pub timestamp_ms: u64,
}

impl DiagnosticsRequest {
    pub fn new(pin: &str) -> Result<Self> {
        Ok(Self {
            protocol: DIAGNOSTICS_PROTOCOL.to_string(),
            version: DIAGNOSTICS_VERSION,
            request_id: request_id(),
            pin_hash: pin_hash(pin)?,
            timestamp_ms: now_ms(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to encode diagnostics request")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let request: Self =
            serde_json::from_slice(bytes).context("failed to decode diagnostics request")?;
        anyhow::ensure!(
            request.protocol == DIAGNOSTICS_PROTOCOL,
            "unexpected diagnostics protocol"
        );
        anyhow::ensure!(
            request.version == DIAGNOSTICS_VERSION,
            "unsupported diagnostics version"
        );
        Ok(request)
    }
}

fn request_id() -> String {
    format!("diag-{}-{}", std::process::id(), now_ms())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
