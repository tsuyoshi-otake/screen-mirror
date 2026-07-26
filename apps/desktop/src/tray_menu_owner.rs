use anyhow::{bail, Context, Result};
use std::ptr;
use tray_icon::menu::{ContextMenu, Menu};
use windows_sys::Win32::{
    Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{AttachThreadInput, GetCurrentThreadId},
    },
    UI::WindowsAndMessaging::{
        BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow,
        GetWindowThreadProcessId, PostMessageW, RegisterClassW, SetForegroundWindow, ShowWindow,
        SW_HIDE, SW_SHOW, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    },
};

const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
const CLASS_NAME: &[u16] = &[
    b'S' as u16,
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
    b'T' as u16,
    b'r' as u16,
    b'a' as u16,
    b'y' as u16,
    b'M' as u16,
    b'e' as u16,
    b'n' as u16,
    b'u' as u16,
    0,
];

pub struct TrayMenuOwner {
    hwnd: HWND,
}

impl TrayMenuOwner {
    pub fn new() -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(ptr::null());
            if instance.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("failed to resolve tray menu module handle");
            }

            let class = WNDCLASSW {
                lpfnWndProc: Some(owner_window_proc),
                hInstance: instance,
                lpszClassName: CLASS_NAME.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(std::io::Error::last_os_error())
                    .context("failed to register tray menu owner window class");
            }

            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS_NAME.as_ptr(),
                ptr::null(),
                WS_POPUP,
                -32_000,
                -32_000,
                1,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null(),
            );
            if hwnd.is_null() {
                bail!(
                    "failed to create tray menu owner window: {}",
                    std::io::Error::last_os_error()
                );
            }

            Ok(Self { hwnd })
        }
    }

    pub fn show(&self, menu: &Menu) {
        unsafe {
            let previous = GetForegroundWindow();
            let current_thread = GetCurrentThreadId();
            let foreground_thread = if previous.is_null() {
                0
            } else {
                GetWindowThreadProcessId(previous, ptr::null_mut())
            };
            let attached = foreground_thread != 0
                && foreground_thread != current_thread
                && AttachThreadInput(current_thread, foreground_thread, 1) != 0;

            ShowWindow(self.hwnd, SW_SHOW);
            let foreground_set = SetForegroundWindow(self.hwnd) != 0;
            BringWindowToTop(self.hwnd);
            if attached {
                AttachThreadInput(current_thread, foreground_thread, 0);
            }

            crate::logging::append(format!(
                "tray menu show: owner={} previous={} foreground_set={} attached={}",
                self.hwnd as usize, previous as usize, foreground_set, attached
            ));

            let selected = menu.show_context_menu_for_hwnd(self.hwnd as isize, None);
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            ShowWindow(self.hwnd, SW_HIDE);
            crate::logging::append(format!("tray menu closed: selected={selected}"));
        }
    }
}

impl Drop for TrayMenuOwner {
    fn drop(&mut self) {
        if !self.hwnd.is_null() {
            unsafe {
                DestroyWindow(self.hwnd);
            }
        }
    }
}

unsafe extern "system" fn owner_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
