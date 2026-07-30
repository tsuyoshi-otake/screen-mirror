use anyhow::{Context, Result};
use get_if_addrs::{get_if_addrs, IfAddr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DISCOVERY_PORT: u16 = 47777;
pub const DISCOVERY_PROBE_PORT: u16 = 47776;
pub const PROTOCOL: &str = "screen-mirror.discovery";
pub const PROBE_PROTOCOL: &str = "screen-mirror.discovery-probe";
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
    /// Largest frame the peer's hardware decoder accepts, when it knows one. A stream above this
    /// fails to negotiate before the decoder sees a frame, so a sender scales down to fit instead.
    /// Peers that predate this field leave it empty, which means "no known limit".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_decode_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_decode_height: Option<u32>,
}

impl DisplayInfo {
    pub fn new(width: u32, height: u32, refresh_hz: Option<u32>) -> Self {
        Self {
            width,
            height,
            refresh_hz,
            max_decode_width: None,
            max_decode_height: None,
        }
    }

    /// A limit is only usable as a pair, so a peer that reports one side and not the other is
    /// treated as having reported nothing.
    pub fn with_decode_limits(mut self, limits: Option<(u32, u32)>) -> Self {
        (self.max_decode_width, self.max_decode_height) = match limits {
            Some((width, height)) => (Some(width), Some(height)),
            None => (None, None),
        };
        self
    }

    pub fn decode_limits(&self) -> Option<(u32, u32)> {
        self.max_decode_width.zip(self.max_decode_height)
    }

    /// Frame size a `source`-sized capture has to be scaled to before this peer can decode it,
    /// or `None` when the capture already fits or the peer announced no limit.
    pub fn fitted_frame_size(&self, source: (u32, u32)) -> Option<(u32, u32)> {
        fit_within(source, self.decode_limits()?)
    }
}

/// Scales `source` down to fit `limit` while keeping its aspect ratio, or `None` when it already
/// fits and needs no scaling at all.
///
/// Both sides are rounded down to an even number: H.264 chroma subsampling needs even dimensions
/// and the D3D11 encoders reject odd ones. Rounding down also keeps the result inside the limit.
pub fn fit_within(source: (u32, u32), limit: (u32, u32)) -> Option<(u32, u32)> {
    let (width, height) = source;
    let (max_width, max_height) = limit;
    if [width, height, max_width, max_height].contains(&0) {
        return None;
    }
    if width <= max_width && height <= max_height {
        return None;
    }

    // Scaling by the wider-constrained side keeps the whole frame inside the limit; comparing the
    // two candidate heights avoids the rounding a floating-point ratio would introduce.
    let height_at_max_width = u64::from(max_width) * u64::from(height) / u64::from(width);
    let fitted = if height_at_max_width <= u64::from(max_height) {
        (u64::from(max_width), height_at_max_width)
    } else {
        (
            u64::from(max_height) * u64::from(width) / u64::from(height),
            u64::from(max_height),
        )
    };
    let even = |value: u64| (value as u32) & !1;
    let fitted = (even(fitted.0).max(2), even(fitted.1).max(2));
    (fitted != source).then_some(fitted)
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiscoveryProbe {
    pub protocol: String,
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wanted_role: Option<PeerRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_hash: Option<String>,
    pub timestamp_ms: u64,
}

impl DiscoveryProbe {
    pub fn new(wanted_role: Option<PeerRole>, pin: Option<&str>) -> Result<Self> {
        Ok(Self {
            protocol: PROBE_PROTOCOL.to_string(),
            version: PROTOCOL_VERSION,
            wanted_role,
            pin_hash: pin.map(pin_hash).transpose()?,
            timestamp_ms: now_ms(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to encode discovery probe")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let probe: Self =
            serde_json::from_slice(bytes).context("failed to decode discovery probe")?;
        anyhow::ensure!(
            probe.protocol == PROBE_PROTOCOL,
            "unexpected probe protocol"
        );
        anyhow::ensure!(
            probe.version == PROTOCOL_VERSION,
            "unsupported probe version"
        );
        Ok(probe)
    }

    pub fn accepts(&self, announcement: &PeerAnnouncement) -> bool {
        !self
            .wanted_role
            .is_some_and(|wanted| announcement.role != wanted)
            && !self
                .pin_hash
                .as_deref()
                .is_some_and(|wanted| announcement.pin_hash.as_deref() != Some(wanted))
    }
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

pub fn bind_probe_responder_socket() -> Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        DISCOVERY_PROBE_PORT,
    ))
    .context("failed to bind discovery probe UDP socket")?;
    socket
        .set_nonblocking(true)
        .context("failed to set discovery probe socket nonblocking")?;
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
    let broadcast_socket = bind_discovery_socket().ok();
    let probe_socket = bind_ephemeral_broadcast_socket()?;
    let probe = DiscoveryProbe::new(role, pin)?;
    let probe_bytes = probe.encode()?;
    let probe_targets = unicast_probe_addresses()?;
    let broadcast_targets = broadcast_addresses()?;
    let deadline = Instant::now() + timeout;
    let mut next_probe = Instant::now();
    let mut peers = Vec::new();
    let mut buffer = [0_u8; 2048];

    while Instant::now() < deadline {
        if Instant::now() >= next_probe {
            send_discovery_probes(
                &probe_socket,
                &probe_bytes,
                &probe_targets,
                &broadcast_targets,
            );
            next_probe = Instant::now() + Duration::from_secs(1);
        }

        if let Some(socket) = broadcast_socket.as_ref() {
            receive_announcements(
                socket,
                &mut buffer,
                role,
                wanted_pin_hash.as_deref(),
                &mut peers,
            )?;
        }
        receive_announcements(
            &probe_socket,
            &mut buffer,
            role,
            wanted_pin_hash.as_deref(),
            &mut peers,
        )?;
        std::thread::sleep(Duration::from_millis(25));
    }

    Ok(peers)
}

fn send_discovery_probes(
    socket: &UdpSocket,
    bytes: &[u8],
    unicast_targets: &[Ipv4Addr],
    broadcast_targets: &[Ipv4Addr],
) {
    for address in unicast_targets.iter().chain(broadcast_targets) {
        let _ = socket.send_to(bytes, SocketAddrV4::new(*address, DISCOVERY_PROBE_PORT));
    }
}

fn receive_announcements(
    socket: &UdpSocket,
    buffer: &mut [u8],
    role: Option<PeerRole>,
    wanted_pin_hash: Option<&str>,
    peers: &mut Vec<DiscoveredPeer>,
) -> Result<()> {
    loop {
        match socket.recv_from(buffer) {
            Ok((len, source)) => {
                let Ok(announcement) = PeerAnnouncement::decode(&buffer[..len]) else {
                    continue;
                };
                if role.is_some_and(|wanted| announcement.role != wanted) {
                    continue;
                }
                if wanted_pin_hash
                    .is_some_and(|wanted| announcement.pin_hash.as_deref() != Some(wanted))
                {
                    continue;
                }
                let std::net::SocketAddr::V4(source) = source else {
                    continue;
                };
                if peers
                    .iter()
                    .any(|peer| peer.announcement.instance_id == announcement.instance_id)
                {
                    continue;
                }
                peers.push(DiscoveredPeer {
                    announcement,
                    address: *source.ip(),
                });
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(())
            }
            Err(error) => return Err(error).context("failed to receive discovery packet"),
        }
    }
}

pub fn unicast_probe_addresses() -> Result<Vec<Ipv4Addr>> {
    let mut addresses = Vec::new();
    for interface in get_if_addrs().context("failed to enumerate network interfaces")? {
        let IfAddr::V4(ipv4) = interface.addr else {
            continue;
        };
        let ip = ipv4.ip;
        if ip.is_loopback() || ip.is_link_local() || !is_lan_address(ip) {
            continue;
        }
        append_probe_subnet(&mut addresses, ip, ipv4.netmask);
        if addresses.len() >= 2048 {
            break;
        }
    }
    addresses.truncate(2048);
    Ok(addresses)
}

fn append_probe_subnet(addresses: &mut Vec<Ipv4Addr>, own_ip: Ipv4Addr, netmask: Ipv4Addr) {
    let own = u32::from(own_ip);
    let mask = u32::from(netmask);
    let scan_mask = if mask.count_ones() < 24 {
        u32::from(Ipv4Addr::new(255, 255, 255, 0))
    } else {
        mask
    };
    let network = own & scan_mask;
    let broadcast = own | !scan_mask;
    if broadcast <= network + 1 {
        return;
    }
    for candidate in (network + 1)..broadcast {
        let candidate = Ipv4Addr::from(candidate);
        if candidate != own_ip && !addresses.contains(&candidate) {
            addresses.push(candidate);
        }
    }
}

fn is_lan_address(ip: Ipv4Addr) -> bool {
    ip.is_private() || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
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

#[cfg(test)]
mod tests {
    use super::{
        append_probe_subnet, fit_within, DiscoveryProbe, DisplayInfo, PeerAnnouncement, PeerRole,
        DISCOVERY_PROBE_PORT,
    };
    use std::net::Ipv4Addr;

    #[test]
    fn discovery_probe_round_trips_and_filters_role_and_pin() {
        let probe = DiscoveryProbe::new(Some(PeerRole::Receiver), Some("9700")).unwrap();
        let decoded = DiscoveryProbe::decode(&probe.encode().unwrap()).unwrap();
        let matching = PeerAnnouncement::new("receiver-1", "receiver", PeerRole::Receiver, 5004)
            .with_pin("9700")
            .unwrap();
        let wrong_role = PeerAnnouncement::new("sender-1", "sender", PeerRole::Sender, 5004)
            .with_pin("9700")
            .unwrap();
        let wrong_pin = PeerAnnouncement::new("receiver-2", "receiver", PeerRole::Receiver, 5004)
            .with_pin("0000")
            .unwrap();

        assert!(decoded.accepts(&matching));
        assert!(!decoded.accepts(&wrong_role));
        assert!(!decoded.accepts(&wrong_pin));
        assert_eq!(DISCOVERY_PROBE_PORT, 47776);
    }

    #[test]
    fn frames_are_scaled_to_fit_a_decoder_limit_without_changing_their_aspect() {
        // An ultrawide desktop into the 1920x1088 an Intel HD 4000 decodes.
        assert_eq!(fit_within((2560, 1080), (1920, 1088)), Some((1920, 810)));
        // 4K fits by width alone, and the height it lands on needs no further scaling.
        assert_eq!(fit_within((3840, 2160), (1920, 1088)), Some((1920, 1080)));
        // A portrait capture is bounded by height instead, and both sides stay even.
        assert_eq!(fit_within((1080, 2400), (1920, 1088)), Some((488, 1088)));
        // Anything that already fits is left alone, so the capture keeps its native size.
        assert_eq!(fit_within((1366, 768), (1920, 1088)), None);
        assert_eq!(fit_within((1920, 1088), (1920, 1088)), None);
        // A source or limit we know nothing about is not a reason to resize.
        assert_eq!(fit_within((0, 0), (1920, 1088)), None);
        assert_eq!(fit_within((2560, 1080), (0, 1088)), None);
    }

    #[test]
    fn a_display_without_announced_decode_limits_never_resizes_the_capture() {
        let unknown = DisplayInfo::new(1366, 768, Some(60));
        let limited = DisplayInfo::new(1920, 1080, Some(60)).with_decode_limits(Some((1920, 1088)));

        assert_eq!(unknown.decode_limits(), None);
        assert_eq!(unknown.fitted_frame_size((2560, 1080)), None);
        assert_eq!(limited.fitted_frame_size((2560, 1080)), Some((1920, 810)));
        assert_eq!(limited.fitted_frame_size((1366, 768)), None);
    }

    #[test]
    fn decode_limits_survive_the_wire_and_older_peers_decode_without_them() {
        let announced = PeerAnnouncement::new("receiver-1", "receiver", PeerRole::Receiver, 5004)
            .with_display(
                DisplayInfo::new(1920, 1080, Some(60)).with_decode_limits(Some((1920, 1088))),
            );
        let decoded = PeerAnnouncement::decode(&announced.encode().unwrap()).unwrap();
        assert_eq!(decoded.display.unwrap().decode_limits(), Some((1920, 1088)));

        let older = PeerAnnouncement::new("receiver-2", "receiver", PeerRole::Receiver, 5004)
            .with_display(DisplayInfo::new(1920, 1080, Some(60)));
        let encoded = String::from_utf8(older.encode().unwrap()).unwrap();
        assert!(!encoded.contains("max_decode"));
        let decoded = PeerAnnouncement::decode(encoded.as_bytes()).unwrap();
        assert_eq!(decoded.display.unwrap().decode_limits(), None);
    }

    #[test]
    fn probe_scan_is_bounded_to_the_local_slash_24() {
        let mut addresses = Vec::new();
        append_probe_subnet(
            &mut addresses,
            Ipv4Addr::new(10, 255, 10, 144),
            Ipv4Addr::new(255, 255, 0, 0),
        );

        assert_eq!(addresses.len(), 253);
        assert!(addresses.contains(&Ipv4Addr::new(10, 255, 10, 90)));
        assert!(!addresses.contains(&Ipv4Addr::new(10, 255, 10, 144)));
        assert!(!addresses.contains(&Ipv4Addr::new(10, 255, 11, 1)));
    }
}
