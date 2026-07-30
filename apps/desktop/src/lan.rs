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
/// Consecutive failed sender starts before the tray stops claiming the session is fine.
const PREPARE_FAILURES_BEFORE_ALERT: u32 = 3;

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
    receiver_key: String,
}

impl SenderPreparationState {
    fn needs_prepare(&self, receiver_key: &str) -> bool {
        self.receiver_key != receiver_key
    }

    fn mark_prepared(&mut self, receiver_key: &str) {
        self.receiver_key = receiver_key.to_string();
    }

    fn reset(&mut self) {
        self.receiver_key.clear();
    }
}

impl SenderSupervisor {
    pub fn start(args: SendArgs, status: Sender<String>) -> Self {
        let (command, command_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            log_sender("sender supervisor started; waiting for matching receivers");
            let mut args = args;
            let mut failed_starts = 0_u32;
            let mut active_hosts = String::new();
            let mut active_receiver_key = String::new();
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
                    active_receiver_key.clear();
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
                    Ok(resolved) => {
                        last_receiver_seen = Some(Instant::now());
                        let receiver_key = receiver_set_key(&resolved.receivers);
                        if receiver_key != active_receiver_key {
                            if let Some(handle) = active_audio_pipeline.take() {
                                if let Err(error) = handle.stop() {
                                    log_sender(format!(
                                        "sender audio target update failed: {error:#}"
                                    ));
                                }
                            }
                            stop_video_pipelines(
                                &mut active_video_pipelines,
                                "sender video target update failed",
                            );
                            active_hosts.clear();
                            active_receiver_key.clear();

                            let start_result = (|| {
                                if preparation.needs_prepare(&receiver_key) {
                                    prepare_sender_environment(&resolved)?;
                                    preparation.mark_prepared(&receiver_key);
                                }
                                spawn_receiver_video_pipelines(&resolved)
                            })();

                            match start_result {
                                Ok(pipelines) => {
                                    failed_starts = 0;
                                    log_sender(format!(
                                        "sender targets updated: {} ({} display stream(s))",
                                        resolved.args.host,
                                        pipelines.len()
                                    ));
                                    active_hosts = resolved.args.host;
                                    active_receiver_key = receiver_key;
                                    active_video_pipelines = pipelines;
                                    apply_sender_audio_state(
                                        &args,
                                        &active_hosts,
                                        &mut active_audio_pipeline,
                                    );
                                    detached_for_no_receivers = false;
                                }
                                Err(error) => {
                                    // Preparation is committed only for a complete receiver route.
                                    // Any count, mode or pipeline build failure must retry it.
                                    preparation.reset();
                                    failed_starts += 1;
                                    log_sender(format!(
                                        "sender environment not ready; transmission deferred and will retry ({failed_starts}): {error:#}"
                                    ));
                                    // Retrying forever behind a "sending" tray label looks
                                    // identical to a working session, so escalate once the
                                    // environment has clearly not recovered on its own.
                                    if failed_starts == PREPARE_FAILURES_BEFORE_ALERT {
                                        crate::monitors::log_monitor_inventory(
                                            "sender environment still not ready",
                                        );
                                        let _ = status
                                            .send(format!("Error: sender cannot start: {error:#}"));
                                    }
                                }
                            }
                        } else {
                            detached_for_no_receivers = false;
                            apply_sender_audio_state(
                                &args,
                                &active_hosts,
                                &mut active_audio_pipeline,
                            );
                        }
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
                        active_receiver_key.clear();
                        if args.enable_virtual_display && !detached_for_no_receivers {
                            crate::monitors::remove_bundled_virtual_display();
                            detached_for_no_receivers = true;
                            last_receiver_seen = None;
                            preparation.reset();
                        }
                        log_sender(format!("waiting for receivers: {error:#}"));
                    }
                }

                crate::updater::set_stream_active(!active_video_pipelines.is_empty());

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

            crate::updater::set_stream_active(false);
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

/// Resolves the single-pipeline (CLI and manual-host) path, returning the capture target the
/// caller must use. Handing the assignment back keeps that path from re-running the whole
/// virtual-display setup a second time inside the pipeline builder.
pub fn resolve_sender_args(
    args: SendArgs,
) -> Result<(SendArgs, Option<crate::monitors::DisplayMonitor>)> {
    let mut resolved = discover_sender_args(args)?;
    (resolved.args.width, resolved.args.height) = effective_video_size(
        resolved.args.width,
        resolved.args.height,
        resolved.target_display.as_ref(),
    );
    prepare_sender_environment(&resolved)?;
    // The direct CLI/manual-host path builds its pipeline outside SenderSupervisor, so perform
    // the same exact-count and per-receiver mode checks before returning control to the caller.
    let target = assign_receiver_displays(&resolved)?.into_iter().next();
    Ok((resolved.args, target.flatten()))
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

    // The bundled driver tops out at MAX_BUNDLED_VIRTUAL_DISPLAYS monitors, and every receiver
    // needs its own. Taking more receivers than that would fail the exact-count check forever.
    let receiver_limit = if args.enable_virtual_display {
        (args.max_receivers as usize).min(crate::monitors::MAX_BUNDLED_VIRTUAL_DISPLAYS)
    } else {
        args.max_receivers as usize
    };
    let selected: Vec<ReceiverTarget> = receivers
        .into_iter()
        .take(receiver_limit)
        .map(|peer| ReceiverTarget {
            host: format!("{}:{}", peer.address, peer.announcement.stream_port),
            display: peer.announcement.display.clone(),
        })
        .collect();
    let target_display = selected
        .iter()
        .find_map(|receiver| receiver.display.clone());

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

fn receiver_set_key(receivers: &[ReceiverTarget]) -> String {
    receivers
        .iter()
        .map(|receiver| match receiver.display.as_ref() {
            Some(display) => format!(
                "{}@{}x{}@{}",
                receiver.host,
                display.width,
                display.height,
                display.refresh_hz.unwrap_or_default()
            ),
            None => format!("{}@unknown", receiver.host),
        })
        .collect::<Vec<_>>()
        .join("|")
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
) -> Result<Vec<Option<crate::monitors::DisplayMonitor>>> {
    let receivers = &resolved.receivers;
    if !resolved.args.enable_virtual_display || receivers.is_empty() {
        return Ok(receivers.iter().map(|_| None).collect());
    }

    let targets = crate::monitors::ensure_bundled_virtual_display_count(receivers.len());
    ensure_virtual_target_count(receivers.len(), targets.len())?;

    let mut assignments = Vec::with_capacity(receivers.len());
    for (receiver, target) in receivers.iter().zip(targets) {
        if resolved.args.sync_virtual_display_resolution {
            crate::monitors::sync_virtual_display_mode(&target, receiver.display.as_ref());
        }
        log_sender(format!(
            "receiver {} captures {}",
            receiver.host, target.adapter_name
        ));
        assignments.push(Some(target));
    }
    Ok(assignments)
}

fn ensure_virtual_target_count(required: usize, available: usize) -> Result<()> {
    anyhow::ensure!(
        available == required,
        "bundled virtual displays are not capture-ready: required {required}, available {available}; refusing physical-display fallback"
    );
    Ok(())
}

/// Validate every receiver route before starting any pipeline.
fn spawn_receiver_video_pipelines(resolved: &ResolvedSender) -> Result<Vec<PipelineHandle>> {
    let assignments = assign_receiver_displays(resolved)?;
    let mut descriptions = Vec::with_capacity(resolved.receivers.len());
    for (receiver, target) in resolved.receivers.iter().zip(assignments) {
        let mut receiver_args = resolved.args.clone();
        receiver_args.host = receiver.host.clone();
        (receiver_args.width, receiver_args.height) = effective_video_size(
            receiver_args.width,
            receiver_args.height,
            receiver.display.as_ref(),
        );
        let description =
            pipeline::build_sender_video_pipeline_for(&receiver_args, target.as_ref())
                .with_context(|| {
                    format!(
                        "failed to build sender video pipeline for {}",
                        receiver.host
                    )
                })?;
        descriptions.push(description);
    }
    anyhow::ensure!(
        !descriptions.is_empty(),
        "no receiver video pipelines were prepared"
    );
    Ok(descriptions
        .into_iter()
        .map(pipeline::spawn_pipeline)
        .collect())
}

fn prepare_sender_environment(resolved: &ResolvedSender) -> Result<()> {
    if resolved.args.enable_virtual_display
        && !crate::monitors::ensure_bundled_virtual_display_ready()
    {
        return Err(anyhow!(
            "bundled VDD was not capture-ready before sender start"
        ));
    }

    Ok(())
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
    use super::{
        effective_video_size, ensure_virtual_target_count, receiver_set_key,
        stable_unique_receivers, ReceiverTarget, SenderPreparationState,
    };
    use sm_core::discovery::{DiscoveredPeer, DisplayInfo, PeerAnnouncement, PeerRole};
    use std::net::Ipv4Addr;

    #[test]
    fn sender_environment_is_prepared_once_per_receiver_set() {
        let mut state = SenderPreparationState::default();

        assert!(state.needs_prepare("10.0.0.2:5004@1366x768"));
        // A failed attempt is not committed, so the same receiver remains retryable.
        assert!(state.needs_prepare("10.0.0.2:5004@1366x768"));

        state.mark_prepared("10.0.0.2:5004@1366x768");
        assert!(!state.needs_prepare("10.0.0.2:5004@1366x768"));
        assert!(state.needs_prepare("10.0.0.2:5004@1920x1080"));

        state.mark_prepared("10.0.0.2:5004@1920x1080");
        assert!(!state.needs_prepare("10.0.0.2:5004@1920x1080"));

        state.reset();
        assert!(state.needs_prepare("10.0.0.2:5004@1920x1080"));
    }

    #[test]
    fn virtual_display_count_must_match_receivers_before_sending() {
        assert!(ensure_virtual_target_count(1, 1).is_ok());
        assert!(ensure_virtual_target_count(1, 0).is_err());
        assert!(ensure_virtual_target_count(2, 1).is_err());
        assert!(ensure_virtual_target_count(1, 2).is_err());
    }

    #[test]
    fn receiver_key_changes_when_the_announced_display_mode_changes() {
        let mut receivers = vec![ReceiverTarget {
            host: "10.0.0.2:5004".to_string(),
            display: Some(DisplayInfo {
                width: 1366,
                height: 768,
                refresh_hz: Some(60),
            }),
        }];
        let initial = receiver_set_key(&receivers);
        receivers[0].display = Some(DisplayInfo {
            width: 1920,
            height: 1080,
            refresh_hz: Some(60),
        });

        assert_ne!(initial, receiver_set_key(&receivers));
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
