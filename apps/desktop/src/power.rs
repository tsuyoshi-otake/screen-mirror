pub struct SleepGuard {
    active: bool,
}

impl SleepGuard {
    pub fn receiver() -> Self {
        let active = prevent_sleep();
        if active {
            crate::logging::append("receiver sleep guard enabled");
        } else {
            crate::logging::append("receiver sleep guard failed or unsupported");
        }
        Self { active }
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        if self.active {
            allow_sleep();
            crate::logging::append("receiver sleep guard disabled");
        }
    }
}

#[cfg(windows)]
fn prevent_sleep() -> bool {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    let result = unsafe {
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
    };
    result != 0
}

#[cfg(not(windows))]
fn prevent_sleep() -> bool {
    false
}

#[cfg(windows)]
fn allow_sleep() {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};

    unsafe {
        SetThreadExecutionState(ES_CONTINUOUS);
    }
}

#[cfg(not(windows))]
fn allow_sleep() {}
