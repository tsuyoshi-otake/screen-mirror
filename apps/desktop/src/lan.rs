use anyhow::{anyhow, Context, Result};
use sm_core::{
    diagnostics::DIAGNOSTICS_PORT,
    discovery::{self, DiscoveredPeer, DisplayInfo, PeerAnnouncement, PeerRole},
};
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
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

struct ResolvedSender {
    args: SendArgs,
    target_display: Option<DisplayInfo>,
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
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut active_hosts = String::new();
            let mut active_pipeline: Option<PipelineHandle> = None;
            let mut detached_for_no_receivers = false;
            let mut last_receiver_seen: Option<Instant> = None;
            let mut preparation = SenderPreparationState::default();

            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                if active_pipeline
                    .as_ref()
                    .map(|handle| handle.is_finished())
                    .unwrap_or(false)
                {
                    if let Some(handle) = active_pipeline.take() {
                        if let Err(error) = handle.finish() {
                            eprintln!("sender pipeline stopped: {error:#}");
                        }
                    }
                    active_hosts.clear();
                    preparation.reset();
                }

                match discover_sender_args(args.clone()) {
                    Ok(resolved) if resolved.args.host != active_hosts => {
                        last_receiver_seen = Some(Instant::now());
                        if let Some(handle) = active_pipeline.take() {
                            if let Err(error) = handle.stop() {
                                eprintln!("sender restart stop failed: {error:#}");
                            }
                        }
                        if preparation.should_prepare(&resolved.args.host)
                            && prepare_sender_environment(&resolved)
                        {
                            detached_for_no_receivers = false;
                        }
                        match pipeline::build_sender_pipeline(&resolved.args) {
                            Ok(description) => {
                                eprintln!("sender targets updated: {}", resolved.args.host);
                                active_hosts = resolved.args.host;
                                active_pipeline = Some(pipeline::spawn_pipeline(description));
                                detached_for_no_receivers = false;
                            }
                            Err(error) => eprintln!("sender pipeline build failed: {error:#}"),
                        }
                    }
                    Ok(_) => {
                        detached_for_no_receivers = false;
                        last_receiver_seen = Some(Instant::now());
                    }
                    Err(error) => {
                        if !detached_for_no_receivers
                            && last_receiver_seen
                                .is_some_and(|last_seen| last_seen.elapsed() < RECEIVER_LOSS_GRACE)
                        {
                            eprintln!(
                                "receiver rediscovery missed; keeping sender pipeline for {}s: {error:#}",
                                RECEIVER_LOSS_GRACE.as_secs()
                            );
                            thread::sleep(Duration::from_secs(1));
                            continue;
                        }
                        if let Some(handle) = active_pipeline.take() {
                            if let Err(error) = handle.stop() {
                                eprintln!("sender stop after receiver loss failed: {error:#}");
                            }
                            active_hosts.clear();
                        }
                        if args.enable_virtual_display && !detached_for_no_receivers {
                            crate::monitors::remove_bundled_virtual_display();
                            detached_for_no_receivers = true;
                            last_receiver_seen = None;
                            preparation.reset();
                        }
                        eprintln!("waiting for receivers: {error:#}");
                    }
                }

                for _ in 0..50 {
                    if stop_rx.try_recv().is_ok() {
                        if let Some(handle) = active_pipeline.take() {
                            let _ = handle.stop();
                        }
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }

            if let Some(handle) = active_pipeline {
                let _ = handle.stop();
            }
        });

        Self {
            stop,
            thread: Some(thread),
        }
    }

    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SenderSupervisor {
    fn drop(&mut self) {
        let _ = self.stop.send(());
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
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            if let Err(error) = discovery::broadcast(&socket, &announcement) {
                eprintln!("discovery broadcast failed: {error:#}");
            }
            thread::sleep(Duration::from_secs(1));
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
    let resolved = discover_sender_args(args)?;
    let _ = prepare_sender_environment(&resolved);
    Ok(resolved.args)
}

fn discover_sender_args(mut args: SendArgs) -> Result<ResolvedSender> {
    if !is_auto_host(&args.host) {
        return Ok(ResolvedSender {
            args,
            target_display: None,
        });
    }

    let receivers = discover_receivers_with_pin(Duration::from_secs(5), &args.pin)?;
    if receivers.is_empty() {
        return Err(anyhow!(
            "no receivers discovered with matching PIN; start receiver mode on another device or set the same four-digit PIN"
        ));
    }

    let target_display = receivers
        .iter()
        .find_map(|peer| peer.announcement.display.clone());

    if args.width.is_none() && args.height.is_none() {
        if let Some(display) = &target_display {
            args.width = Some(display.width);
            args.height = Some(display.height);
        }
    }

    let selected: Vec<String> = receivers
        .into_iter()
        .take(args.max_receivers as usize)
        .map(|peer| format!("{}:{}", peer.address, peer.announcement.stream_port))
        .collect();

    args.host = selected.join(",");
    Ok(ResolvedSender {
        args,
        target_display,
    })
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
    use super::SenderPreparationState;

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
}
