use anyhow::{anyhow, Context, Result};
use sm_core::discovery::{self, DiscoveredPeer, PeerAnnouncement, PeerRole};
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

            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                match resolve_sender_args(args.clone()) {
                    Ok(resolved) if resolved.host != active_hosts => {
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
                        if active_pipeline.is_none() {
                            eprintln!("waiting for receivers: {error:#}");
                        }
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
    pub fn receiver(stream_port: u16) -> Result<Self> {
        let socket = discovery::bind_ephemeral_broadcast_socket()?;
        let announcement = PeerAnnouncement::new(
            instance_id(),
            device_name(),
            PeerRole::Receiver,
            stream_port,
        );
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
        return Ok(args);
    }

    let receivers = discover_receivers(Duration::from_secs(5))?;
    if receivers.is_empty() {
        return Err(anyhow!(
            "no receivers discovered; start receiver mode on another device or set host explicitly"
        ));
    }

    let selected: Vec<String> = receivers
        .into_iter()
        .take(args.max_receivers as usize)
        .map(|peer| format!("{}:{}", peer.address, peer.announcement.stream_port))
        .collect();

    args.host = selected.join(",");
    Ok(args)
}

pub fn discover_receivers(timeout: Duration) -> Result<Vec<DiscoveredPeer>> {
    discovery::discover_receivers(timeout).context("receiver discovery failed")
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
