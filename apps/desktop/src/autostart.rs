use anyhow::{Context, Result};

const APP_NAME: &str = "ScreenMirror";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
pub fn set_enabled(enabled: bool) -> Result<()> {
    use std::env;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey(RUN_KEY)
        .context("failed to open HKCU Run key")?;

    if enabled {
        let exe = env::current_exe().context("failed to resolve current executable")?;
        let command = format!("\"{}\" tray", exe.display());
        run_key
            .set_value(APP_NAME, &command)
            .context("failed to set autostart registry value")?;
    } else {
        match run_key.delete_value(APP_NAME) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to delete autostart registry value"),
        }
    }

    Ok(())
}

#[cfg(windows)]
pub fn is_enabled() -> Result<bool> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey(RUN_KEY)
        .context("failed to open HKCU Run key")?;
    Ok(run_key.get_value::<String, _>(APP_NAME).is_ok())
}

#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool) -> Result<()> {
    anyhow::bail!("autostart is currently implemented only for Windows")
}

#[cfg(not(windows))]
pub fn is_enabled() -> Result<bool> {
    Ok(false)
}
