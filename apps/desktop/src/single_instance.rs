use anyhow::{anyhow, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub struct InstanceGuard {
    lock_path: PathBuf,
    _lock_file: File,
    #[cfg(windows)]
    mutex: windows_sys::Win32::Foundation::HANDLE,
}

pub fn acquire_tray_instance() -> Result<InstanceGuard> {
    let lock_path = lock_path()?;
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create lock directory: {}", parent.display()))?;
    }

    if lock_path.exists() {
        let stale = fs::read_to_string(&lock_path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .is_none_or(|pid| !process_is_running(pid));
        if stale {
            let _ = fs::remove_file(&lock_path);
        }
    }

    let mut lock_file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::process::exit(0);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to create lock file: {}", lock_path.display()));
        }
    };
    writeln!(lock_file, "{}", std::process::id()).context("failed to write lock file")?;

    #[cfg(windows)]
    let mutex = acquire_named_mutex()?;

    Ok(InstanceGuard {
        lock_path,
        _lock_file: lock_file,
        #[cfg(windows)]
        mutex,
    })
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.mutex);
        }
    }
}

fn lock_path() -> Result<PathBuf> {
    let base = dirs::data_dir().context("failed to resolve shared app data directory")?;
    Ok(base.join("ScreenMirror").join("screen-mirror.lock"))
}

#[cfg(windows)]
fn acquire_named_mutex() -> Result<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name = "Global\\ScreenMirrorTrayInstance";
    let mut wide = name.encode_utf16().collect::<Vec<_>>();
    wide.push(0);

    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(anyhow!("failed to create single-instance mutex"));
    }

    let last_error = unsafe { GetLastError() };
    if last_error == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        std::process::exit(0);
    }

    Ok(handle)
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut exit_code = 0;
    let running = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0
        && exit_code == STILL_ACTIVE as u32;
    unsafe {
        CloseHandle(handle);
    }
    running
}

#[cfg(not(windows))]
fn process_is_running(_pid: u32) -> bool {
    false
}

#[cfg(all(test, windows))]
mod tests {
    use super::process_is_running;

    #[test]
    fn distinguishes_running_and_exited_processes() {
        assert!(process_is_running(std::process::id()));

        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("failed to start test child process");
        let pid = child.id();
        assert!(process_is_running(pid));
        child.wait().expect("failed to wait for test child process");
        assert!(!process_is_running(pid));
    }
}
