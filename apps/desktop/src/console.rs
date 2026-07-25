pub fn attach_for_cli() {
    attach_parent_console();
}

pub fn line(message: impl AsRef<str>) {
    use std::io::Write;

    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{}", message.as_ref()).is_ok() {
        return;
    }

    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{}", message.as_ref());
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}
