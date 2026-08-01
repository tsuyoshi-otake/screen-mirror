use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::pipeline::ReceiverStreamStats;

pub struct RenderWindowGuard {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl RenderWindowGuard {
    pub fn start(stats: Option<Arc<Mutex<ReceiverStreamStats>>>) -> Self {
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            #[cfg(windows)]
            run_window_guard(&stop_rx, stats);
            #[cfg(not(windows))]
            {
                let _ = stats;
                while stop_rx.try_recv().is_err() {
                    thread::sleep(Duration::from_millis(250));
                }
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
fn run_window_guard(stop_rx: &mpsc::Receiver<()>, stats: Option<Arc<Mutex<ReceiverStreamStats>>>) {
    let mut overlay = None;
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        let renderer = apply_receiver_window_chrome();
        if let Some(stats) = stats.as_ref() {
            overlay = sync_stats_overlay(renderer, overlay, Arc::clone(stats));
            pump_overlay_messages();
        } else if let Some(window) = overlay.take() {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(window.window);
            }
        }

        if renderer.is_none() {
            if let Some(window) = overlay.take() {
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(window.window);
                }
            }
        }

        thread::sleep(Duration::from_millis(250));
    }

    if let Some(window) = overlay {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(window.window);
        }
    }
}

#[cfg(windows)]
fn apply_receiver_window_chrome() -> Option<windows_sys::Win32::Foundation::HWND> {
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
        found: HWND,
    }

    unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = &mut *(lparam as *mut Context);
        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id != context.process_id || IsWindowVisible(hwnd) == 0 {
            return TRUE;
        }

        let mut buffer = [0_u16; 256];
        let length = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        let window_title = String::from_utf16_lossy(&buffer[..length.max(0) as usize]);
        let normalized = window_title.to_ascii_lowercase();
        if !normalized.contains("direct3d11")
            && !normalized.contains("renderer")
            && !normalized.contains("screen-mirror receiver")
        {
            return TRUE;
        }

        context.found = hwnd;
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
    let mut context = Context {
        process_id: std::process::id(),
        title: "screen-mirror Receiver"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect(),
        icon,
        found: std::ptr::null_mut(),
    };

    unsafe {
        EnumWindows(Some(enum_window), &mut context as *mut Context as isize);
    }
    (!context.found.is_null()).then_some(context.found)
}

#[cfg(windows)]
struct StatsOverlay {
    parent: windows_sys::Win32::Foundation::HWND,
    window: windows_sys::Win32::Foundation::HWND,
}

#[cfg(windows)]
fn sync_stats_overlay(
    renderer: Option<windows_sys::Win32::Foundation::HWND>,
    overlay: Option<StatsOverlay>,
    stats: Arc<Mutex<ReceiverStreamStats>>,
) -> Option<StatsOverlay> {
    let Some(renderer) = renderer else {
        if let Some(overlay) = overlay {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(overlay.window);
            }
        }
        return None;
    };

    let overlay = match overlay {
        Some(overlay) if overlay.parent == renderer => overlay,
        Some(overlay) => {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(overlay.window);
            }
            create_stats_overlay(renderer, stats)?
        }
        None => create_stats_overlay(renderer, stats)?,
    };

    unsafe {
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetClientRect, SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOSENDCHANGING,
            SWP_SHOWWINDOW,
        };

        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 360,
            bottom: 102,
        };
        if GetClientRect(renderer, &mut rect) != 0 {
            let width = (rect.right - rect.left).clamp(280, 420);
            let height = (rect.bottom - rect.top).clamp(86, 120);
            SetWindowPos(
                overlay.window,
                HWND_TOP,
                14,
                14,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOSENDCHANGING | SWP_SHOWWINDOW,
            );
        }
        InvalidateRect(overlay.window, std::ptr::null(), 1);
    }
    Some(overlay)
}

#[cfg(windows)]
fn create_stats_overlay(
    parent: windows_sys::Win32::Foundation::HWND,
    stats: Arc<Mutex<ReceiverStreamStats>>,
) -> Option<StatsOverlay> {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::HINSTANCE;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, RegisterClassW, CS_HREDRAW, CS_VREDRAW, WNDCLASSW, WS_CHILD,
        WS_EX_NOACTIVATE, WS_EX_TRANSPARENT, WS_VISIBLE,
    };

    static CLASS_NAME: &[u16] = &[
        b's' as u16,
        b'c' as u16,
        b'r' as u16,
        b'e' as u16,
        b'e' as u16,
        b'n' as u16,
        b'M' as u16,
        b'i' as u16,
        b'r' as u16,
        b'r' as u16,
        b'o' as u16,
        b'r' as u16,
        b'O' as u16,
        b'v' as u16,
        b'e' as u16,
        b'r' as u16,
        b'l' as u16,
        b'a' as u16,
        b'y' as u16,
        0,
    ];
    let instance: HINSTANCE = unsafe { GetModuleHandleW(null_mut()) };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(stats_overlay_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null_mut(),
        lpszClassName: CLASS_NAME.as_ptr(),
    };
    unsafe {
        RegisterClassW(&class);
    }

    let context = Box::new(StatsOverlayContext { stats });
    let raw_context = Box::into_raw(context);
    let title = [b's' as u16, b't' as u16, b'a' as u16, b't' as u16, 0];
    let window = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            CLASS_NAME.as_ptr(),
            title.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            14,
            14,
            360,
            102,
            parent,
            null_mut(),
            instance,
            raw_context as *const c_void,
        )
    };
    if window.is_null() {
        unsafe {
            drop(Box::from_raw(raw_context));
        }
        return None;
    }
    Some(StatsOverlay { parent, window })
}

#[cfg(windows)]
struct StatsOverlayContext {
    stats: Arc<Mutex<ReceiverStreamStats>>,
}

#[cfg(windows)]
unsafe extern "system" fn stats_overlay_proc(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, SetBkMode,
        SetTextColor, DT_LEFT, DT_NOPREFIX, DT_TOP, TRANSPARENT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetWindowLongPtrW, SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA,
        HTTRANSPARENT, MA_NOACTIVATE, WM_CREATE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCDESTROY,
        WM_NCHITTEST, WM_PAINT,
    };

    if message == WM_CREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        return 0;
    }

    let context = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut StatsOverlayContext;
    match message {
        WM_PAINT => {
            let mut paint = std::mem::zeroed();
            let hdc = BeginPaint(window, &mut paint);
            let background = CreateSolidBrush(0x00141414);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(window, &mut rect);
            FillRect(hdc, &rect, background);
            DeleteObject(background);

            if !context.is_null() {
                if let Ok(stats) = (*context).stats.lock() {
                    let text = format_overlay_text(&stats);
                    let wide = text.encode_utf16().collect::<Vec<_>>();
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(hdc, 0x00E8E8E8);
                    let mut text_rect = RECT {
                        left: 10,
                        top: 7,
                        right: rect.right - 10,
                        bottom: rect.bottom - 7,
                    };
                    DrawTextW(
                        hdc,
                        wide.as_ptr(),
                        wide.len() as i32,
                        &mut text_rect,
                        DT_LEFT | DT_TOP | DT_NOPREFIX,
                    );
                }
            }
            EndPaint(window, &paint);
            0
        }
        WM_ERASEBKGND => 1,
        WM_NCHITTEST => HTTRANSPARENT as isize,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_NCDESTROY => {
            if !context.is_null() {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                drop(Box::from_raw(context));
            }
            DefWindowProcW(window, message, wparam, lparam)
        }
        _ => {
            if message == WM_CREATE {
                0
            } else {
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
    }
}

#[cfg(windows)]
fn format_overlay_text(stats: &ReceiverStreamStats) -> String {
    format!(
        "screen-mirror\n decoded {:>5.1} fps  displayed {:>5.1} fps\n RTP +{}  loss {}  late {}  dup {}\n jitter {:>3} ms  window {} ms",
        stats.decoded_fps,
        stats.displayed_fps,
        stats.received_packets,
        stats.lost_packets,
        stats.late_packets,
        stats.duplicate_packets,
        stats.jitter_ms,
        stats.window_ms,
    )
}

#[cfg(windows)]
fn pump_overlay_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let mut message = MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
    };
    unsafe {
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn format_overlay_text(_stats: &ReceiverStreamStats) -> String {
    String::new()
}
