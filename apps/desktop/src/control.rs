use anyhow::{Context, Result};
use sm_core::control::{ControlEvent, StreamFeedback, TouchAction, CONTROL_PORT};
use sm_core::discovery::pin_hash;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FEEDBACK_STALE_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
struct FeedbackEntry {
    feedback: StreamFeedback,
    received_at: Instant,
}

/// Latest receiver reports, indexed by the receiver's LAN address.
///
/// Feedback deliberately shares the established control socket with touch events. Keeping the
/// store separate from the receiver thread means sender pipelines can read it without adding
/// locks to the packet path or changing the existing ControlEvent schema.
#[derive(Clone, Default)]
pub struct FeedbackStore {
    entries: Arc<Mutex<HashMap<Ipv4Addr, FeedbackEntry>>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FeedbackHealth {
    pub receiver_count: usize,
    pub loss_ratio: f64,
    pub late_ratio: f64,
    pub jitter_ms: f64,
}

impl FeedbackStore {
    pub fn health(&self) -> FeedbackHealth {
        let now = Instant::now();
        let Ok(entries) = self.entries.lock() else {
            return FeedbackHealth::default();
        };

        let mut health = FeedbackHealth::default();
        for entry in entries.values() {
            if now.duration_since(entry.received_at) > FEEDBACK_STALE_AFTER {
                continue;
            }

            let total_packets = entry
                .feedback
                .received_packets
                .saturating_add(entry.feedback.lost_packets)
                .max(1) as f64;
            health.receiver_count += 1;
            health.loss_ratio = health
                .loss_ratio
                .max(entry.feedback.lost_packets as f64 / total_packets);
            health.late_ratio = health
                .late_ratio
                .max(entry.feedback.late_packets as f64 / total_packets);
            health.jitter_ms = health.jitter_ms.max(f64::from(entry.feedback.jitter_ms));
        }
        health
    }

    fn update(&self, source: Ipv4Addr, feedback: StreamFeedback) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                source,
                FeedbackEntry {
                    feedback,
                    received_at: Instant::now(),
                },
            );
        }
    }
}

pub struct ControlServer {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
    feedback: FeedbackStore,
}

impl ControlServer {
    pub fn start(pin: &str) -> Result<Self> {
        let expected_pin_hash = pin_hash(pin)?;
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, CONTROL_PORT))
            .context("failed to bind touch control UDP port")?;
        socket
            .set_nonblocking(true)
            .context("failed to set touch control socket nonblocking")?;
        let feedback = FeedbackStore::default();
        let feedback_for_thread = feedback.clone();
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut buffer = [0_u8; 2048];
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                match socket.recv_from(&mut buffer) {
                    Ok((len, source)) => {
                        if let Ok(report) = StreamFeedback::decode(&buffer[..len]) {
                            if report.pin_hash != expected_pin_hash {
                                continue;
                            }
                            if let std::net::SocketAddr::V4(source) = source {
                                feedback_for_thread.update(*source.ip(), report);
                            }
                            continue;
                        }

                        if let Ok(event) = ControlEvent::decode(&buffer[..len]) {
                            if event.pin_hash.as_deref() != Some(expected_pin_hash.as_str()) {
                                continue;
                            }
                            inject_touch(event);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        eprintln!("touch control receive failed: {error}");
                        thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        });

        Ok(Self {
            stop,
            thread: Some(thread),
            feedback,
        })
    }

    pub fn feedback(&self) -> FeedbackStore {
        self.feedback.clone()
    }

    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

#[cfg(windows)]
fn inject_touch(event: ControlEvent) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MOVE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1) as f32;
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1) as f32;
    let x = ((event.x.clamp(0.0, 1.0) * width) * 65535.0 / width).round() as i32;
    let y = ((event.y.clamp(0.0, 1.0) * height) * 65535.0 / height).round() as i32;
    let mut flags = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE;

    match event.action {
        TouchAction::Down => flags |= MOUSEEVENTF_LEFTDOWN,
        TouchAction::Move => {}
        TouchAction::Up | TouchAction::Cancel => flags |= MOUSEEVENTF_LEFTUP,
    }

    unsafe {
        mouse_event(flags, x, y, 0, 0);
    }
}

#[cfg(not(windows))]
fn inject_touch(_event: ControlEvent) {}

#[cfg(test)]
mod tests {
    use super::{FeedbackEntry, FeedbackStore, FEEDBACK_STALE_AFTER};
    use sm_core::control::StreamFeedback;
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};

    #[test]
    fn health_uses_the_worst_active_receiver() {
        let store = FeedbackStore::default();
        store.update(
            Ipv4Addr::new(192, 168, 1, 10),
            StreamFeedback::with_pin("1234", 1, 1_000, 98, 2, 4, 0, 30, 30, 8)
                .expect("valid feedback"),
        );
        store.update(
            Ipv4Addr::new(192, 168, 1, 11),
            StreamFeedback::with_pin("1234", 1, 1_000, 90, 10, 1, 0, 30, 30, 20)
                .expect("valid feedback"),
        );

        let health = store.health();
        assert_eq!(health.receiver_count, 2);
        assert!((health.loss_ratio - 0.1).abs() < 0.001);
        assert!((health.late_ratio - 0.04).abs() < 0.001);
        assert_eq!(health.jitter_ms, 20.0);
    }

    #[test]
    fn stale_feedback_is_not_used_for_adaptation() {
        let store = FeedbackStore::default();
        let feedback = StreamFeedback::with_pin("1234", 1, 1_000, 100, 20, 20, 0, 30, 30, 10)
            .expect("valid feedback");
        store.entries.lock().expect("feedback store lock").insert(
            Ipv4Addr::LOCALHOST,
            FeedbackEntry {
                feedback,
                received_at: Instant::now() - FEEDBACK_STALE_AFTER - Duration::from_millis(10),
            },
        );
        assert_eq!(store.health(), Default::default());
    }
}
