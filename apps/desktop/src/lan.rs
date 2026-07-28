use anyhow::{anyhow, Context, Result};
use sm_core::{
    diagnostics::DIAGNOSTICS_PORT,
    discovery::{self, DiscoveredPeer, DisplayInfo, PeerAnnouncement, PeerRole},
};
use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::pipeline::{self, PipelineHandle, SendArgs};

const AUTO_HOST: &str = "auto";
const RECEIVER_LOSS_GRACE: Duration = Duration::from_secs(75);

pub struct Announcer {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

pub struct SenderSupervisor {
    command: Sender<SenderSupervisorCommand>,
    thread: Option<JoinHandle<()>>,
}

enum SenderSupervisorCommand {
    SetAudioEnabled(bool),
    Stop,
}

struct ResolvedSender {
    args: SendArgs,
    target_display: Option<DisplayInfo>,
    receivers: Vec<ReceiverTarget>,
}

struct ReceiverTarget {
    host: String,
    display: Option<DisplayInfo>,
}

#[derive(Default)]
struct SenderPreparationState {
    hosts: String,
}

impl SenderPreparationState {
    fn should_prepare(&mut self, hosts: &str) -> bool {
        if self.hosts == hosts {
            return false;
        }
        self.hosts = hosts.to_string();
        true
    }

    fn reset(&mut self) {
        self.hosts.clear();
    }
}

impl SenderSupervisor {
    pub fn start(args: SendArgs) -> Self {
        let (command, command_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            log_sender("sender supervisor started; waiting for matching receivers");
            let mut args = args;
            let mut active_hosts = String::new();
            let mut active_video_pipelines: Vec<PipelineHandle> = Vec::new();
            let mut active_audio_pipeline: Option<PipelineHandle> = None;
            let mut detached_for_no_receivers = false;
            let mut last_receiver_seen: Option<Instant> = None;
            let mut preparation = SenderPreparationState::default();

            'supervisor: loop {
                while let Ok(message) = command_rx.try_recv() {
                    if apply_sender_supervisor_command(
                        message,
                        &mut args,
                        &active_hosts,
                        &mut active_audio_pipeline,
                    ) {
                        break 'supervisor;
                    }
                }

                // One receiver's pipeline dying takes the whole set down, so the next discovery
                // pass rebuilds every receiver against a fresh display assignment.
                if active_video_pipelines
                    .iter()
                    .any(|handle| handle.is_finished())
                {
                    stop_video_pipelines(
                        &mut active_video_pipelines,
                        "sender video pipeline stopped",
                    );
                    if let Some(handle) = active_audio_pipeline.take() {
                        if let Err(error) = handle.stop() {
                            log_sender(format!(
                                "sender audio cleanup after video stop failed: {error:#}"
                            ));
                        }
                    }
                    active_hosts.clear();
                    preparation.reset();
                }

                if active_audio_pipeline
                    .as_ref()
                    .map(|handle| handle.is_finished())
                    .unwrap_or(false)
                {
                    if let Some(handle) = active_audio_pipeline.take() {
                        if let Err(error) = handle.finish() {
                            log_sender(format!(
                                "sender audio pipeline stopped; video remains active: {error:#}"
                            ));
                        }
                    }
                }

                match discover_sender_args(args.clone()) {
                    Ok(resolved) if resolved.args.host != active_hosts => {
                        last_receiver_seen = Some(Instant::now());
                        if let Some(handle) = active_audio_pipeline.take() {
                            if let Err(error) = handle.stop() {
                                log_sender(format!("sender audio target update failed: {error:#}"));
                            }
                        }
                        stop_video_pipelines(
                            &mut active_video_pipelines,
                            "sender video target update failed",
                        );
                        active_hosts.clear();
                        if preparation.should_prepare(&resolved.args.host)
                            && prepare_sender_environment(&resolved)
                        {
                            detached_for_no_receivers = false;
                        }
                        let pipelines = spawn_receiver_video_pipelines(&resolved);
                        if pipelines.is_empty() {
                            log_sender("no sender video pipeline could be started for the current receivers");
                        } else {
                            log_sender(format!(
                                "sender targets updated: {} ({} display stream(s))",
                                resolved.args.host,
                                pipelines.len()
                            ));
                            active_hosts = resolved.args.host;
                            active_video_pipelines = pipelines;
                            apply_sender_audio_state(
                                &args,
                                &active_hosts,
                                &mut active_audio_pipeline,
                            );
                            detached_for_no_receivers = false;
                        }
                    }
                    Ok(_) => {
                        detached_for_no_receivers = false;
                        last_receiver_seen = Some(Instant::now());
                        apply_sender_audio_state(&args, &active_hosts, &mut active_audio_pipeline);
                    }
                    Err(error) => {
                        if !detached_for_no_receivers
                            && last_receiver_seen
                                .is_some_and(|last_seen| last_seen.elapsed() < RECEIVER_LOSS_GRACE)
                        {
                            log_sender(format!(
                                "receiver rediscovery missed; keeping sender pipeline for {}s: {error:#}",
                                RECEIVER_LOSS_GRACE.as_secs()
                            ));
                            thread::sleep(Duration::from_secs(1));
                            continue;
                        }
                        if let Some(handle) = active_audio_pipeline.take() {
                            if let Err(error) = handle.stop() {
                                log_sender(format!(
                                    "sender audio stop after receiver loss failed: {error:#}"
                                ));
                            }
                        }
                        stop_video_pipelines(
                            &mut active_video_pipelines,
                            "sender video stop after receiver loss failed",
                        );
                        active_hosts.clear();
                        if args.enable_virtual_display && !detached_for_no_receivers {
                            crate::monitors::remove_bundled_virtual_display();
                            detached_for_no_receivers = true;
                            last_receiver_seen = None;
                            preparation.reset();
                        }
                        log_sender(format!("waiting for receivers: {error:#}"));
                    }
                }

                for _ in 0..50 {
                    match command_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(message) => {
                            if apply_sender_supervisor_command(
                                message,
                                &mut args,
                                &active_hosts,
                                &mut active_audio_pipeline,
                            ) {
                                break 'supervisor;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break 'supervisor,
                    }
                }
            }

            if let Some(handle) = active_audio_pipeline {
                let _ = handle.stop();
            }
            for handle in active_video_pipelines {
                let _ = handle.stop();
            }
        });

        Self {
            command,
            thread: Some(thread),
        }
    }

    pub fn set_audio_enabled(&self, enabled: bool) -> Result<()> {
        self.command
            .send(SenderSupervisorCommand::SetAudioEnabled(enabled))
            .map_err(|_| anyhow!("sender supervisor is not running"))
    }

    pub fn stop(mut self) {
        let _ = self.command.send(SenderSupervisorCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn apply_sender_supervisor_command(
    message: SenderSupervisorCommand,
    args: &mut SendArgs,
    active_hosts: &str,
    active_audio_pipeline: &mut Option<PipelineHandle>,
) -> bool {
    match message {
        SenderSupervisorCommand::SetAudioEnabled(enabled) => {
            args.audio_enabled = enabled;
            apply_sender_audio_state(args, active_hosts, active_audio_pipeline);
            false
        }
        SenderSupervisorCommand::Stop => true,
    }
}

fn stop_video_pipelines(pipelines: &mut Vec<PipelineHandle>, context: &str) {
    for handle in pipelines.drain(..) {
        if let Err(error) = handle.stop() {
            log_sender(format!("{context}: {error:#}"));
        }
    }
}

fn apply_sender_audio_state(
    args: &SendArgs,
    active_hosts: &str,
    active_audio_pipeline: &mut Option<PipelineHandle>,
) {
    if !args.audio_enabled {
        if let Some(handle) = active_audio_pipeline.take() {
            if let Err(error) = handle.stop() {
                log_sender(format!("sender audio stop failed: {error:#}"));
            } else {
                log_sender("sender audio stopped without restarting video");
            }
        }
        return;
    }
    if active_hosts.is_empty() || active_audio_pipeline.is_some() {
        return;
    }

    let mut audio_args = args.clone();
    audio_args.host = active_hosts.to_string();
    match pipeline::build_sender_audio_pipeline(&audio_args) {
        Ok(description) => {
            *active_audio_pipeline = Some(pipeline::spawn_pipeline(description));
            log_sender("sender audio started without restarting video");
        }
        Err(error) => log_sender(format!("sender audio pipeline build failed: {error:#}")),
    }
}

fn log_sender(message: impl AsRef<str>) {
    let message = message.as_ref();
    eprintln!("{message}");
    crate::logging::append(message);
}

impl Drop for SenderSupervisor {
    fn drop(&mut self) {
        let _ = self.command.send(SenderSupervisorCommand::Stop);
    }
}

impl Announcer {
    pub fn sender(stream_port: u16, audio_port: Option<u16>, pin: &str) -> Result<Self> {
        Self::start(PeerRole::Sender, stream_port, audio_port, pin)
    }

    pub fn receiver(stream_port: u16, audio_port: Option<u16>, pin: &str) -> Result<Self> {
        Self::start(PeerRole::Receiver, stream_port, audio_port, pin)
    }

    fn start(role: PeerRole, stream_port: u16, audio_port: Option<u16>, pin: &str) -> Result<Self> {
        let socket = discovery::bind_ephemeral_broadcast_socket()?;
        let mut announcement =
            PeerAnnouncement::new(instance_id(), device_name(), role, stream_port)
                .with_pin(pin)?
                .with_audio_port(audio_port)
                .with_diagnostics_port(DIAGNOSTICS_PORT);
        if let Some(display) = crate::monitors::primary_display_info() {
            announcement = announcement.with_display(display);
        }
        let announcement_bytes = announcement.encode()?;
        let probe_socket = match discovery::bind_probe_responder_socket() {
            Ok(socket) => Some(socket),
            Err(error) => {
                crate::logging::append(format!(
                    "discovery unicast responder unavailable: {error:#}"
                ));
                None
            }
        };
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut next_broadcast = Instant::now();
            let mut buffer = [0_u8; 2048];
            let mut logged_unicast_response = false;
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                if Instant::now() >= next_broadcast {
                    if let Err(error) = discovery::broadcast(&socket, &announcement) {
                        crate::logging::append(format!("discovery broadcast failed: {error:#}"));
                        eprintln!("discovery broadcast failed: {error:#}");
                    }
                    next_broadcast = Instant::now() + Duration::from_secs(1);
                }

                if let Some(probe_socket) = probe_socket.as_ref() {
                    loop {
                        match probe_socket.recv_from(&mut buffer) {
                            Ok((length, source)) => {
                                let Ok(probe) =
                                    discovery::DiscoveryProbe::decode(&buffer[..length])
                                else {
                                    continue;
                                };
                                if probe.accepts(&announcement) {
                                    let _ = probe_socket.send_to(&announcement_bytes, source);
                                    if !logged_unicast_response {
                                        crate::logging::append(format!(
                                            "discovery unicast response sent to {source}"
                                        ));
                                        logged_unicast_response = true;
                                    }
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(error) => {
                                crate::logging::append(format!(
                                    "discovery unicast responder failed: {error}"
                                ));
                                break;
                            }
                        }
                    }
                }

                thread::sleep(Duration::from_millis(50));
            }
        });

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

pub fn resolve_sender_args(args: SendArgs) -> Result<SendArgs> {
    let mut resolved = discover_sender_args(args)?;
    (resolved.args.width, resolved.args.height) = effective_video_size(
        resolved.args.width,
        resolved.args.height,
        resolved.target_display.as_ref(),
    );
    let _ = prepare_sender_environment(&resolved);
    Ok(resolved.args)
}

fn discover_sender_args(mut args: SendArgs) -> Result<ResolvedSender> {
    if !is_auto_host(&args.host) {
        let receivers = args
            .host
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(|host| ReceiverTarget {
                host: host.to_string(),
                display: None,
            })
            .collect();
        return Ok(ResolvedSender {
            args,
            target_display: None,
            receivers,
        });
    }

    let receivers = stable_unique_receivers(discover_receivers_with_pin(
        Duration::from_secs(5),
        &args.pin,
    )?);
    if receivers.is_empty() {
        return Err(anyhow!(
            "no receivers discovered with matching PIN; start receiver mode on another device or set the same four-digit PIN"
        ));
    }

    let target_display = receivers
        .iter()
        .find_map(|peer| peer.announcement.display.clone());

    let selected: Vec<ReceiverTarget> = receivers
        .into_iter()
        .take(args.max_receivers as usize)
        .map(|peer| ReceiverTarget {
            host: format!("{}:{}", peer.address, peer.announcement.stream_port),
            display: peer.announcement.display.clone(),
        })
        .collect();

    args.host = selected
        .iter()
        .map(|receiver| receiver.host.as_str())
        .collect::<Vec<_>>()
        .join(",");
    Ok(ResolvedSender {
        args,
        target_display,
        receivers: selected,
    })
}

/// Discovery arrival order changes from pass to pass.  Keep receiver-to-display assignment stable
/// and collapse stale duplicate announcements that point at the same RTP endpoint; otherwise the
/// sender rebuilds its pipelines and can alternate two desktop streams on one Android UDP socket.
fn stable_unique_receivers(mut receivers: Vec<DiscoveredPeer>) -> Vec<DiscoveredPeer> {
    receivers.sort_by(|left, right| {
        left.announcement
            .instance_id
            .cmp(&right.announcement.instance_id)
            .then_with(|| left.address.cmp(&right.address))
            .then_with(|| {
                left.announcement
                    .stream_port
                    .cmp(&right.announcement.stream_port)
            })
    });

    let mut endpoints = HashSet::new();
    receivers.retain(|peer| endpoints.insert((peer.address, peer.announcement.stream_port)));
    receivers
}

/// Preserve the capture source's native mode unless the user explicitly requested output caps.
///
/// A receiver's display dimensions are only a target for VDD mode sync. Using them as encoder
/// caps stretches the source when Windows cannot apply that mode (for example, a 1366x768 VDD
/// paired with a portrait phone).
fn effective_video_size(
    width: Option<u32>,
    height: Option<u32>,
    _receiver_display: Option<&DisplayInfo>,
) -> (Option<u32>, Option<u32>) {
    (width, height)
}

/// Gives every receiver its own virtual display, so a second phone joining gets a new desktop
/// instead of a copy of the first one's.
fn assign_receiver_displays(
    resolved: &ResolvedSender,
) -> Vec<Option<crate::monitors::DisplayMonitor>> {
    let receivers = &resolved.receivers;
    if !resolved.args.enable_virtual_display || receivers.is_empty() {
        return receivers.iter().map(|_| None).collect();
    }

    let targets = crate::monitors::ensure_bundled_virtual_display_count(receivers.len());
    if targets.is_empty() {
        log_sender("no bundled virtual display is capture-ready; receivers fall back to the default capture target");
        return receivers.iter().map(|_| None).collect();
    }
    if targets.len() < receivers.len() {
        log_sender(format!(
            "only {} virtual display(s) for {} receiver(s); the remaining receivers share the last display",
            targets.len(),
            receivers.len()
        ));
    }

    receivers
        .iter()
        .enumerate()
        .map(|(index, receiver)| {
            let target = targets[index.min(targets.len() - 1)].clone();
            if resolved.args.sync_virtual_display_resolution {
                if let Err(error) =
                    crate::monitors::sync_virtual_display_mode(&target, receiver.display.as_ref())
                {
                    log_sender(format!(
                        "virtual display sync for {} failed: {error:#}",
                        receiver.host
                    ));
                }
            }
            log_sender(format!(
                "receiver {} captures {}",
                receiver.host, target.adapter_name
            ));
            Some(target)
        })
        .collect()
}

/// One video pipeline per receiver; a build failure only drops that receiver.
fn spawn_receiver_video_pipelines(resolved: &ResolvedSender) -> Vec<PipelineHandle> {
    let assignments = assign_receiver_displays(resolved);
    let mut pipelines = Vec::new();
    for (receiver, target) in resolved.receivers.iter().zip(assignments) {
        let mut receiver_args = resolved.args.clone();
        receiver_args.host = receiver.host.clone();
        (receiver_args.width, receiver_args.height) = effective_video_size(
            receiver_args.width,
            receiver_args.height,
            receiver.display.as_ref(),
        );
        match pipeline::build_sender_video_pipeline_for(&receiver_args, target.as_ref()) {
            Ok(description) => pipelines.push(pipeline::spawn_pipeline(description)),
            Err(error) => log_sender(format!(
                "sender video pipeline build failed for {}: {error:#}",
                receiver.host
            )),
        }
    }
    pipelines
}

fn prepare_sender_environment(resolved: &ResolvedSender) -> bool {
    let display_ready = !resolved.args.enable_virtual_display
        || crate::monitors::ensure_bundled_virtual_display_ready();
    if !display_ready {
        crate::logging::append("bundled VDD was not capture-ready before sender start");
    }

    if resolved.args.sync_virtual_display_resolution {
        if let Err(error) =
            crate::monitors::sync_preferred_virtual_display_mode(resolved.target_display.as_ref())
        {
            crate::logging::append(format!("virtual display resolution sync failed: {error:#}"));
        }
    }

    display_ready
}

pub fn discover_receivers_with_pin(timeout: Duration, pin: &str) -> Result<Vec<DiscoveredPeer>> {
    discovery::discover_receivers_with_pin(timeout, pin).context("receiver discovery failed")
}

pub fn discover_senders_with_pin(timeout: Duration, pin: &str) -> Result<Vec<DiscoveredPeer>> {
    discovery::discover_with_pin(timeout, Some(PeerRole::Sender), Some(pin))
        .context("sender discovery failed")
}

fn is_auto_host(host: &str) -> bool {
    host.trim().is_empty() || host.trim().eq_ignore_ascii_case(AUTO_HOST)
}

pub fn wants_auto_host(host: &str) -> bool {
    is_auto_host(host)
}

fn instance_id() -> String {
    let name = device_name();
    let process = std::process::id();
    format!("{name}-{process}")
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "screen-mirror-desktop".to_string())
}

#[cfg(test)]
mod tests {
    use super::{effective_video_size, stable_unique_receivers, SenderPreparationState};
    use sm_core::discovery::{DiscoveredPeer, DisplayInfo, PeerAnnouncement, PeerRole};
    use std::net::Ipv4Addr;

    #[test]
    fn sender_environment_is_prepared_once_per_receiver_set() {
        let mut state = SenderPreparationState::default();

        assert!(state.should_prepare("10.0.0.2:5004"));
        assert!(!state.should_prepare("10.0.0.2:5004"));
        assert!(state.should_prepare("10.0.0.3:5004"));
        assert!(!state.should_prepare("10.0.0.3:5004"));

        state.reset();
        assert!(state.should_prepare("10.0.0.3:5004"));
    }

    #[test]
    fn receiver_order_and_endpoint_set_are_stable_across_discovery_passes() {
        let receiver = |instance: &str, address: [u8; 4], port: u16| DiscoveredPeer {
            announcement: PeerAnnouncement::new(instance, instance, PeerRole::Receiver, port),
            address: Ipv4Addr::from(address),
        };
        let first = vec![
            receiver("phone-b", [10, 0, 0, 3], 5004),
            receiver("phone-a-stale", [10, 0, 0, 2], 5004),
            receiver("phone-a", [10, 0, 0, 2], 5004),
        ];
        let mut second = first.clone();
        second.reverse();

        let first = stable_unique_receivers(first);
        let second = stable_unique_receivers(second);
        let identities = |peers: &[DiscoveredPeer]| {
            peers
                .iter()
                .map(|peer| {
                    (
                        peer.announcement.instance_id.clone(),
                        peer.address,
                        peer.announcement.stream_port,
                    )
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(identities(&first), identities(&second));
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].announcement.instance_id, "phone-a");
        assert_eq!(first[1].announcement.instance_id, "phone-b");
    }

    #[test]
    fn receiver_display_size_never_overrides_capture_native_size() {
        let portrait = DisplayInfo {
            width: 720,
            height: 1604,
            refresh_hz: Some(60),
        };

        assert_eq!(
            effective_video_size(None, None, Some(&portrait)),
            (None, None)
        );
        assert_eq!(
            effective_video_size(Some(1920), Some(1080), Some(&portrait)),
            (Some(1920), Some(1080))
        );
    }
}
