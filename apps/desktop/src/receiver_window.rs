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
    let mut visual_capture = VisualCaptureState::default();
    let mut renderer_was_present = false;
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

        if let Some(renderer) = renderer {
            renderer_was_present = true;
            capture_visual_if_due(renderer, stats.as_ref(), &mut visual_capture);
        } else if renderer_was_present {
            renderer_was_present = false;
            crate::logging::append("receiver visual capture: renderer window missing");
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
const VISUAL_CAPTURE_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(windows)]
const VISUAL_CAPTURE_LOG_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(windows)]
const VISUAL_CAPTURE_MAX_WIDTH: f64 = 640.0;
#[cfg(windows)]
const VISUAL_CAPTURE_MAX_HEIGHT: f64 = 360.0;

/// State for the low-rate visual probe. The probe deliberately runs beside the renderer window
/// rather than inside the GStreamer streaming thread: a stopped pipeline must still be observable
/// as a frozen or cleared window.
#[cfg(windows)]
#[derive(Default)]
struct VisualCaptureState {
    last_capture: Option<std::time::Instant>,
    last_log: Option<std::time::Instant>,
    last_hash: Option<u64>,
    last_verdict: Option<&'static str>,
    last_flow: Option<&'static str>,
    last_changed: Option<bool>,
    last_anomaly_path: Option<String>,
}

#[cfg(windows)]
fn capture_visual_if_due(
    renderer: windows_sys::Win32::Foundation::HWND,
    stats: Option<&Arc<Mutex<ReceiverStreamStats>>>,
    state: &mut VisualCaptureState,
) {
    let now = std::time::Instant::now();
    if state
        .last_capture
        .is_some_and(|last| now.duration_since(last) < VISUAL_CAPTURE_INTERVAL)
    {
        return;
    }
    state.last_capture = Some(now);

    let stats_available = stats.is_some();
    let snapshot = stats
        .and_then(|stats| stats.lock().ok().map(|stats| stats.clone()))
        .unwrap_or_default();
    let stats_age_ms = snapshot
        .last_update
        .map(|updated| now.duration_since(updated).as_millis().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let stats_fresh = snapshot
        .last_update
        .is_some_and(|updated| now.duration_since(updated) <= Duration::from_millis(1_500));
    let flow = if !stats_available {
        "unknown"
    } else if !stats_fresh {
        "stale"
    } else if snapshot.displayed_fps < 0.5 {
        "none"
    } else {
        "active"
    };

    let image = match capture_receiver_window(renderer) {
        Ok(image) => image,
        Err(error) => {
            let should_log = state
                .last_log
                .is_none_or(|last| now.duration_since(last) >= VISUAL_CAPTURE_LOG_INTERVAL);
            if should_log {
                crate::logging::append(format!(
                    "receiver visual capture: status=error window={:p} error=\"{error:#}\"",
                    renderer
                ));
                state.last_log = Some(now);
            }
            return;
        }
    };

    let metrics = analyze_capture(&image);
    let changed = state.last_hash != Some(metrics.hash);
    state.last_hash = Some(metrics.hash);
    let classification = metrics.classification();
    let no_sink_frames = flow == "stale" || flow == "none";
    let verdict = if no_sink_frames && classification == "likely-white" {
        "blank-white-no-frame"
    } else if no_sink_frames && classification == "likely-black" {
        "blank-black-no-frame"
    } else if no_sink_frames && !changed {
        "stale-frame-no-frame"
    } else if classification == "likely-white" {
        "white-frame-or-renderer"
    } else if classification == "likely-black" {
        "black-frame-or-renderer"
    } else {
        "rendered"
    };

    let capture_dir = receiver_capture_directory();
    let latest_path = capture_dir.join("receiver-window-latest.bmp");
    let save_error = save_capture_bmp(&latest_path, &image).err();

    if verdict != "rendered" && state.last_verdict != Some(verdict) {
        let anomaly_path = capture_dir.join(format!(
            "receiver-window-anomaly-{}.bmp",
            capture_timestamp()
        ));
        if save_capture_bmp(&anomaly_path, &image).is_ok() {
            state.last_anomaly_path = Some(anomaly_path.display().to_string());
        }
    }

    let state_changed = state.last_verdict != Some(verdict)
        || state.last_flow != Some(flow)
        || state.last_changed != Some(changed);
    let periodic_log = state
        .last_log
        .is_none_or(|last| now.duration_since(last) >= VISUAL_CAPTURE_LOG_INTERVAL);
    if state_changed || periodic_log {
        let anomaly_path = if verdict == "rendered" {
            "".to_string()
        } else {
            state.last_anomaly_path.clone().unwrap_or_default()
        };
        let save_status = save_error
            .as_ref()
            .map(|error| format!("error:{error:#}"))
            .unwrap_or_else(|| "ok".to_string());
        crate::logging::append(format!(
            "receiver visual capture: status=ok window={:p} source={} latest=\"{}\" anomaly=\"{}\" size={}x{} save={} classification={} verdict={} flow={} stats-age-ms={} decoded-fps={:.1} sink-input-fps={:.1} sink-buffers={} mean-luma={:.1} stddev-luma={:.1} white-ratio={:.3} black-ratio={:.3} hash={:016x} changed={}",
            renderer,
            image.source,
            latest_path.display(),
            anomaly_path,
            image.width,
            image.height,
            save_status,
            classification,
            verdict,
            flow,
            stats_age_ms,
            snapshot.decoded_fps,
            snapshot.displayed_fps,
            snapshot.displayed_frames,
            metrics.mean_luma,
            metrics.stddev_luma,
            metrics.white_ratio,
            metrics.black_ratio,
            metrics.hash,
            changed,
        ));
        state.last_log = Some(now);
    }
    state.last_verdict = Some(verdict);
    state.last_flow = Some(flow);
    state.last_changed = Some(changed);
}

#[cfg(windows)]
#[derive(Debug)]
struct CapturedImage {
    source: &'static str,
    width: u32,
    height: u32,
    /// 32-bit BGRA pixels in top-down row order.
    pixels: Vec<u8>,
}

#[cfg(windows)]
#[derive(Debug)]
struct CaptureMetrics {
    hash: u64,
    mean_luma: f32,
    stddev_luma: f32,
    white_ratio: f32,
    black_ratio: f32,
}

#[cfg(windows)]
impl CaptureMetrics {
    fn classification(&self) -> &'static str {
        if self.white_ratio >= 0.98 && self.mean_luma >= 245.0 {
            "likely-white"
        } else if self.black_ratio >= 0.98 && self.mean_luma <= 10.0 {
            "likely-black"
        } else {
            "mixed"
        }
    }
}

#[cfg(windows)]
fn analyze_capture(image: &CapturedImage) -> CaptureMetrics {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in &image.pixels {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211_u64);
    }

    let width = image.width as usize;
    let height = image.height as usize;
    let left = (width / 4).min(width.saturating_sub(1));
    let top = (height / 4).min(height.saturating_sub(1));
    let right = (width.saturating_mul(3) / 4).max(left.saturating_add(1));
    let bottom = (height.saturating_mul(3) / 4).max(top.saturating_add(1));
    let mut count = 0_u64;
    let mut white = 0_u64;
    let mut black = 0_u64;
    let mut sum = 0.0_f64;
    let mut sum_squared = 0.0_f64;

    for y in top..bottom.min(height) {
        for x in left..right.min(width) {
            let offset = (y * width + x).saturating_mul(4);
            if offset.saturating_add(2) >= image.pixels.len() {
                continue;
            }
            let blue = f64::from(image.pixels[offset]);
            let green = f64::from(image.pixels[offset + 1]);
            let red = f64::from(image.pixels[offset + 2]);
            let luma = red * 0.2126 + green * 0.7152 + blue * 0.0722;
            count = count.saturating_add(1);
            sum += luma;
            sum_squared += luma * luma;
            if red >= 245.0 && green >= 245.0 && blue >= 245.0 {
                white = white.saturating_add(1);
            }
            if red <= 10.0 && green <= 10.0 && blue <= 10.0 {
                black = black.saturating_add(1);
            }
        }
    }

    let count_as_float = count.max(1) as f64;
    let mean = sum / count_as_float;
    let variance = (sum_squared / count_as_float - mean * mean).max(0.0);
    CaptureMetrics {
        hash,
        mean_luma: mean as f32,
        stddev_luma: variance.sqrt() as f32,
        white_ratio: white as f32 / count_as_float as f32,
        black_ratio: black as f32 / count_as_float as f32,
    }
}

#[cfg(windows)]
fn capture_receiver_window(
    renderer: windows_sys::Win32::Foundation::HWND,
) -> anyhow::Result<CapturedImage> {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, GetDC, ReleaseDC};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetClientRect(renderer, &mut rect) } == 0 {
        return Err(anyhow::anyhow!("GetClientRect failed"));
    }
    let width = rect.right.saturating_sub(rect.left);
    let height = rect.bottom.saturating_sub(rect.top);
    if width <= 0 || height <= 0 {
        return Err(anyhow::anyhow!(
            "receiver renderer has an empty client area"
        ));
    }

    let mut origin = POINT { x: 0, y: 0 };
    let origin_available = unsafe { ClientToScreen(renderer, &mut origin) != 0 };
    let mut last_error = None;
    let null_hwnd = std::ptr::null_mut();
    let screen_dc = unsafe { GetDC(null_hwnd) };
    if !screen_dc.is_null() {
        let result = capture_from_source(
            screen_dc,
            if origin_available { origin.x } else { 0 },
            if origin_available { origin.y } else { 0 },
            width,
            height,
        );
        unsafe { ReleaseDC(null_hwnd, screen_dc) };
        match result {
            Ok(mut image) => {
                image.source = "desktop";
                return Ok(image);
            }
            Err(error) => last_error = Some(error),
        }
    }

    let window_dc = unsafe { GetDC(renderer) };
    if window_dc.is_null() {
        return Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("GetDC failed for both the desktop and receiver")));
    }
    let result = capture_from_source(window_dc, 0, 0, width, height);
    unsafe { ReleaseDC(renderer, window_dc) };
    result
        .map(|mut image| {
            image.source = "window";
            image
        })
        .map_err(|_| {
            last_error.unwrap_or_else(|| anyhow::anyhow!("receiver screen capture failed"))
        })
}

#[cfg(windows)]
fn capture_from_source(
    source_dc: windows_sys::Win32::Graphics::Gdi::HDC,
    source_x: i32,
    source_y: i32,
    source_width: i32,
    source_height: i32,
) -> anyhow::Result<CapturedImage> {
    use std::ffi::c_void;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
        SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
        HALFTONE, HGDIOBJ, SRCCOPY,
    };

    let scale = (VISUAL_CAPTURE_MAX_WIDTH / f64::from(source_width))
        .min(VISUAL_CAPTURE_MAX_HEIGHT / f64::from(source_height))
        .min(1.0);
    let output_width = (f64::from(source_width) * scale).round().max(1.0) as i32;
    let output_height = (f64::from(source_height) * scale).round().max(1.0) as i32;
    let target_dc = unsafe { CreateCompatibleDC(source_dc) };
    if target_dc.is_null() {
        return Err(anyhow::anyhow!("CreateCompatibleDC failed"));
    }
    let bitmap = unsafe { CreateCompatibleBitmap(source_dc, output_width, output_height) };
    if bitmap.is_null() {
        unsafe { DeleteDC(target_dc) };
        return Err(anyhow::anyhow!("CreateCompatibleBitmap failed"));
    }
    let previous = unsafe { SelectObject(target_dc, bitmap as HGDIOBJ) };
    if previous.is_null() {
        unsafe {
            DeleteObject(bitmap as HGDIOBJ);
            DeleteDC(target_dc);
        }
        return Err(anyhow::anyhow!("SelectObject failed"));
    }
    let mut bitmap_selected = true;

    let result = (|| {
        unsafe { SetStretchBltMode(target_dc, HALFTONE) };
        if unsafe {
            StretchBlt(
                target_dc,
                0,
                0,
                output_width,
                output_height,
                source_dc,
                source_x,
                source_y,
                source_width,
                source_height,
                SRCCOPY,
            )
        } == 0
        {
            return Err(anyhow::anyhow!("StretchBlt failed"));
        }

        let pixel_bytes = (output_width as usize)
            .saturating_mul(output_height as usize)
            .saturating_mul(4);
        let mut pixels = vec![0_u8; pixel_bytes];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: output_width,
                biHeight: -output_height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                biSizeImage: pixel_bytes.min(u32::MAX as usize) as u32,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [windows_sys::Win32::Graphics::Gdi::RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        // GetDIBits is undefined while the bitmap is selected into a DC. The blit is complete,
        // so restore the DC before asking GDI for the pixels.
        unsafe { SelectObject(target_dc, previous) };
        bitmap_selected = false;
        let copied = unsafe {
            GetDIBits(
                source_dc,
                bitmap,
                0,
                output_height as u32,
                pixels.as_mut_ptr() as *mut c_void,
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        if copied != output_height {
            return Err(anyhow::anyhow!("GetDIBits copied {copied} lines"));
        }
        Ok(CapturedImage {
            source: "unknown",
            width: output_width as u32,
            height: output_height as u32,
            pixels,
        })
    })();

    unsafe {
        if bitmap_selected {
            SelectObject(target_dc, previous);
        }
        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(target_dc);
    }
    result
}

#[cfg(windows)]
fn receiver_capture_directory() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ScreenMirror")
        .join("Diagnostics")
}

#[cfg(windows)]
fn capture_timestamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(windows)]
fn save_capture_bmp(path: &std::path::Path, image: &CapturedImage) -> anyhow::Result<()> {
    let image_size = u32::try_from(image.pixels.len())
        .map_err(|_| anyhow::anyhow!("capture image is too large for a BMP"))?;
    let file_size = 54_u32
        .checked_add(image_size)
        .ok_or_else(|| anyhow::anyhow!("capture BMP size overflow"))?;
    let mut bytes = Vec::with_capacity(file_size as usize);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&54_u32.to_le_bytes());
    bytes.extend_from_slice(&40_u32.to_le_bytes());
    bytes.extend_from_slice(&(image.width as i32).to_le_bytes());
    bytes.extend_from_slice(&(-(image.height as i32)).to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&32_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&image_size.to_le_bytes());
    bytes.extend_from_slice(&2_835_i32.to_le_bytes());
    bytes.extend_from_slice(&2_835_i32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| anyhow::anyhow!("create {}: {error}", parent.display()))?;
    }
    bytes.extend_from_slice(&image.pixels);
    std::fs::write(path, bytes)
        .map_err(|error| anyhow::anyhow!("write {}: {error}", path.display()))?;
    Ok(())
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

#[cfg(all(test, windows))]
mod tests {
    use super::{analyze_capture, CapturedImage};

    fn solid(width: u32, height: u32, blue: u8, green: u8, red: u8) -> CapturedImage {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[blue, green, red, 255]);
        }
        CapturedImage {
            source: "test",
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn visual_probe_classifies_a_white_surface() {
        let metrics = analyze_capture(&solid(16, 16, 255, 255, 255));

        assert_eq!(metrics.classification(), "likely-white");
        assert!(metrics.white_ratio >= 0.98);
        assert!(metrics.mean_luma >= 245.0);
    }

    #[test]
    fn visual_probe_classifies_a_black_surface() {
        let metrics = analyze_capture(&solid(16, 16, 0, 0, 0));

        assert_eq!(metrics.classification(), "likely-black");
        assert!(metrics.black_ratio >= 0.98);
        assert!(metrics.mean_luma <= 10.0);
    }

    #[test]
    fn visual_probe_does_not_call_a_varied_frame_blank() {
        let mut image = solid(16, 16, 255, 255, 255);
        for (index, pixel) in image.pixels.chunks_exact_mut(4).enumerate() {
            if index % 2 == 0 {
                pixel[..3].copy_from_slice(&[0, 0, 0]);
            }
        }

        let metrics = analyze_capture(&image);

        assert_eq!(metrics.classification(), "mixed");
        assert!(metrics.white_ratio < 0.98);
        assert!(metrics.black_ratio < 0.98);
    }
}
