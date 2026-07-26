use anyhow::{anyhow, Context, Result};
use sm_core::{
    diagnostics::DIAGNOSTICS_PORT,
    discovery::{self, DiscoveredPeer, PeerAnnouncement, PeerRole},
};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::pipeline::{self, PipelineHandle, SendArgs};

const AUTO_HOST: &str = "auto";

pub struct Announcer {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

pub struct SenderSupervisor {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl SenderSupervisor {
    pub fn start(args: SendArgs) -> Self {
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut active_hosts = String::new();
            let mut active_pipeline: Option<PipelineHandle> = None;
            let mut detached_for_no_receivers = false;

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
                }

                match resolve_sender_args(args.clone()) {
                    Ok(resolved) if resolved.host != active_hosts => {
                        detached_for_no_receivers = false;
                        if let Some(handle) = active_pipeline.take() {
                            if let Err(error) = handle.stop() {
                                eprintln!("sender restart stop failed: {error:#}");
                            }
                        }
                        match pipeline::build_sender_pipeline(&resolved) {
                            Ok(description) => {
                                eprintln!("sender targets updated: {}", resolved.host);
                                active_hosts = resolved.host;
                                active_pipeline = Some(pipeline::spawn_pipeline(description));
                            }
                            Err(error) => eprintln!("sender pipeline build failed: {error:#}"),
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if let Some(handle) = active_pipeline.take() {
                            if let Err(error) = handle.stop() {
                                eprintln!("sender stop after receiver loss failed: {error:#}");
                            }
                            active_hosts.clear();
                        }
                        if args.enable_virtual_display && !detached_for_no_receivers {
                            crate::monitors::remove_bundled_virtual_display();
                            detached_for_no_receivers = true;
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

pub fn resolve_sender_args(mut args: SendArgs) -> Result<SendArgs> {
    if !is_auto_host(&args.host) {
        if args.enable_virtual_display {
            crate::monitors::ensure_bundled_virtual_display_installed();
            if !crate::monitors::wait_for_bundled_virtual_display(Duration::from_secs(15)) {
                crate::logging::append("bundled VDD did not appear before sender start");
            }
            crate::monitors::request_extended_desktop();
            if !crate::monitors::wait_for_bundled_virtual_capture(Duration::from_secs(10)) {
                crate::logging::append("bundled VDD was not capture-ready before sender start");
            }
        }
        return Ok(args);
    }

    let receivers = discover_receivers_with_pin(Duration::from_secs(5), &args.pin)?;
    if receivers.is_empty() {
        if args.enable_virtual_display {
            crate::monitors::remove_bundled_virtual_display();
        }
        return Err(anyhow!(
            "no receivers discovered with matching PIN; start receiver mode on another device or set the same four-digit PIN"
        ));
    }

    if args.enable_virtual_display {
        crate::monitors::ensure_bundled_virtual_display_installed();
        if !crate::monitors::wait_for_bundled_virtual_display(Duration::from_secs(15)) {
            crate::logging::append("bundled VDD did not appear before sender start");
        }
        crate::monitors::request_extended_desktop();
        if !crate::monitors::wait_for_bundled_virtual_capture(Duration::from_secs(10)) {
            crate::logging::append("bundled VDD was not capture-ready before sender start");
        }
    }

    let target_display = receivers
        .iter()
        .find_map(|peer| peer.announcement.display.clone());

    if args.sync_virtual_display_resolution {
        if let Err(error) =
            crate::monitors::sync_preferred_virtual_display_mode(target_display.as_ref())
        {
            crate::logging::append(format!("virtual display resolution sync failed: {error:#}"));
        }
    }

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
    Ok(args)
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
