use anyhow::{Context, Result};
use get_if_addrs::{get_if_addrs, IfAddr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DISCOVERY_PORT: u16 = 47777;
pub const PROTOCOL: &str = "screen-mirror.discovery";
pub const PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_PIN: &str = "0000";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerAnnouncement {
    pub protocol: String,
    pub version: u16,
    pub instance_id: String,
    pub device_name: String,
    pub role: PeerRole,
    pub stream_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayInfo>,
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_hz: Option<u32>,
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeerRole {
    Idle,
    Sender,
    Receiver,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPeer {
    pub announcement: PeerAnnouncement,
    pub address: Ipv4Addr,
}

impl PeerAnnouncement {
    pub fn new(
        instance_id: impl Into<String>,
        device_name: impl Into<String>,
        role: PeerRole,
        stream_port: u16,
    ) -> Self {
        Self {
            protocol: PROTOCOL.to_string(),
            version: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
            device_name: device_name.into(),
            role,
            stream_port,
            audio_port: None,
            diagnostics_port: None,
            pin_hash: None,
            display: None,
            timestamp_ms: now_ms(),
        }
    }

    pub fn with_pin(mut self, pin: &str) -> Result<Self> {
        self.pin_hash = Some(pin_hash(pin)?);
        Ok(self)
    }

    pub fn with_audio_port(mut self, audio_port: Option<u16>) -> Self {
        self.audio_port = audio_port;
        self
    }

    pub fn with_diagnostics_port(mut self, diagnostics_port: u16) -> Self {
        self.diagnostics_port = Some(diagnostics_port);
        self
    }

    pub fn with_display(mut self, display: DisplayInfo) -> Self {
        self.display = Some(display);
        self
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to encode discovery packet")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let packet: Self =
            serde_json::from_slice(bytes).context("failed to decode discovery packet")?;
        anyhow::ensure!(packet.protocol == PROTOCOL, "unexpected discovery protocol");
        anyhow::ensure!(
            packet.version == PROTOCOL_VERSION,
            "unsupported discovery version"
        );
        Ok(packet)
    }
}

pub fn bind_discovery_socket() -> Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))
        .context("failed to bind discovery UDP socket")?;
    socket
        .set_broadcast(true)
        .context("failed to enable UDP broadcast")?;
    socket
        .set_nonblocking(true)
        .context("failed to set discovery socket nonblocking")?;
    Ok(socket)
}

pub fn bind_ephemeral_broadcast_socket() -> Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .context("failed to bind discovery sender socket")?;
    socket
        .set_broadcast(true)
        .context("failed to enable UDP broadcast")?;
    socket
        .set_nonblocking(true)
        .context("failed to set discovery sender nonblocking")?;
    Ok(socket)
}

pub fn broadcast(socket: &UdpSocket, announcement: &PeerAnnouncement) -> Result<()> {
    let bytes = announcement.encode()?;
    for address in broadcast_addresses()? {
        socket
            .send_to(&bytes, SocketAddrV4::new(address, DISCOVERY_PORT))
            .with_context(|| format!("failed to broadcast discovery packet to {address}"))?;
    }
    Ok(())
}

pub fn broadcast_addresses() -> Result<Vec<Ipv4Addr>> {
    let mut addresses = Vec::new();
    for interface in get_if_addrs().context("failed to enumerate network interfaces")? {
        let IfAddr::V4(ipv4) = interface.addr else {
            continue;
        };
        let ip = ipv4.ip;
        if ip.is_loopback() || ip.octets()[0] == 169 {
            continue;
        }
        let ip_u32 = u32::from(ip);
        let mask_u32 = u32::from(ipv4.netmask);
        let broadcast = Ipv4Addr::from(ip_u32 | !mask_u32);
        if !addresses.contains(&broadcast) {
            addresses.push(broadcast);
        }
    }

    if addresses.is_empty() {
        addresses.push(Ipv4Addr::BROADCAST);
    }

    Ok(addresses)
}

pub fn discover_receivers(timeout: Duration) -> Result<Vec<DiscoveredPeer>> {
    discover(timeout, Some(PeerRole::Receiver))
}

pub fn discover_receivers_with_pin(timeout: Duration, pin: &str) -> Result<Vec<DiscoveredPeer>> {
    discover_with_pin(timeout, Some(PeerRole::Receiver), Some(pin))
}

pub fn discover(timeout: Duration, role: Option<PeerRole>) -> Result<Vec<DiscoveredPeer>> {
    discover_with_pin(timeout, role, None)
}

pub fn discover_with_pin(
    timeout: Duration,
    role: Option<PeerRole>,
    pin: Option<&str>,
) -> Result<Vec<DiscoveredPeer>> {
    let wanted_pin_hash = pin.map(pin_hash).transpose()?;
    let socket = bind_discovery_socket()?;
    let deadline = Instant::now() + timeout;
    let mut peers = Vec::new();
    let mut buffer = [0_u8; 2048];

    while Instant::now() < deadline {
        match socket.recv_from(&mut buffer) {
            Ok((len, source)) => {
                let Ok(announcement) = PeerAnnouncement::decode(&buffer[..len]) else {
                    continue;
                };
                if role.is_some_and(|wanted| announcement.role != wanted) {
                    continue;
                }
                if wanted_pin_hash
                    .as_deref()
                    .is_some_and(|wanted| announcement.pin_hash.as_deref() != Some(wanted))
                {
                    continue;
                }
                let std::net::SocketAddr::V4(source) = source else {
                    continue;
                };
                if peers.iter().any(|peer: &DiscoveredPeer| {
                    peer.announcement.instance_id == announcement.instance_id
                }) {
                    continue;
                }
                peers.push(DiscoveredPeer {
                    announcement,
                    address: *source.ip(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error).context("failed to receive discovery packet"),
        }
    }

    Ok(peers)
}

pub fn normalize_pin(pin: &str) -> Result<String> {
    let pin = pin.trim();
    anyhow::ensure!(
        pin.len() == 4 && pin.bytes().all(|byte| byte.is_ascii_digit()),
        "PIN must be exactly four digits"
    );
    Ok(pin.to_string())
}

pub fn pin_hash(pin: &str) -> Result<String> {
    let pin = normalize_pin(pin)?;
    let mut hasher = Sha256::new();
    hasher.update(b"screen-mirror.pin.v1:");
    hasher.update(pin.as_bytes());
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    Ok(text)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
