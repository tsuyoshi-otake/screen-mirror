use std::process::Command;

pub fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    hide_window(&mut command);
    command
}

pub fn hide_window(command: &mut Command) {
    hide_window_platform(command);
}

#[cfg(windows)]
fn hide_window_platform(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window_platform(_command: &mut Command) {}
