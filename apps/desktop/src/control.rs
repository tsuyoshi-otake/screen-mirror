use anyhow::{Context, Result};
use sm_core::control::{ControlEvent, TouchAction, CONTROL_PORT};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct ControlServer {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    pub fn start() -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, CONTROL_PORT))
            .context("failed to bind touch control UDP port")?;
        socket
            .set_nonblocking(true)
            .context("failed to set touch control socket nonblocking")?;
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut buffer = [0_u8; 2048];
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                match socket.recv_from(&mut buffer) {
                    Ok((len, _source)) => {
                        if let Ok(event) = ControlEvent::decode(&buffer[..len]) {
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
        })
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
