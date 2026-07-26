use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct RenderWindowGuard {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl RenderWindowGuard {
    pub fn start() -> Self {
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            while stop_rx.try_recv().is_err() {
                apply_receiver_window_chrome();
                thread::sleep(Duration::from_millis(250));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for RenderWindowGuard {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
fn apply_receiver_window_chrome() {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, LoadImageW,
        SendMessageW, SetWindowTextW, ICON_BIG, ICON_SMALL, ICON_SMALL2, IMAGE_ICON,
        LR_LOADFROMFILE, WM_SETICON,
    };

    struct Context {
        process_id: u32,
        title: Vec<u16>,
        icon: isize,
    }

    unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = &*(lparam as *const Context);
        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id != context.process_id || IsWindowVisible(hwnd) == 0 {
            return TRUE;
        }

        let mut buffer = [0_u16; 256];
        let length = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        let window_title = String::from_utf16_lossy(&buffer[..length.max(0) as usize]);
        let normalized = window_title.to_ascii_lowercase();
        if !normalized.contains("direct3d11") && !normalized.contains("renderer") {
            return TRUE;
        }

        SetWindowTextW(hwnd, context.title.as_ptr());
        if context.icon != 0 {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, context.icon);
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL2 as usize, context.icon);
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, context.icon);
        }
        TRUE
    }

    let icon_path = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("screen-mirror.ico")));
    let icon = icon_path
        .filter(|path| path.exists())
        .map(|path| {
            let wide = path
                .as_os_str()
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            unsafe {
                LoadImageW(
                    std::ptr::null_mut(),
                    wide.as_ptr(),
                    IMAGE_ICON,
                    32,
                    32,
                    LR_LOADFROMFILE,
                ) as isize
            }
        })
        .unwrap_or(0);
    let context = Context {
        process_id: std::process::id(),
        title: "screen-mirror Receiver"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect(),
        icon,
    };

    unsafe {
        EnumWindows(Some(enum_window), &context as *const Context as isize);
    }
}

#[cfg(not(windows))]
fn apply_receiver_window_chrome() {}
