//! Native control of the bundled MTT Virtual Display Driver.
//!
//! Everything here talks to SetupAPI directly instead of shelling out to PowerShell or devcon:
//! the driver is installed, restarted and removed in-process, and the only thing we still need
//! an external process for is the UAC prompt, which re-launches this same executable.

use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum VddAction {
    /// Install the bundled driver if no device node exists yet.
    Install,
    /// Write the monitor count and restart the driver so it takes effect.
    SetCount,
    /// Remove every bundled virtual display device.
    Remove,
    /// Enable the installed devices.
    Enable,
    /// Disable the installed devices without removing them.
    Disable,
}

/// Runs an action, elevating through a hidden child process when we are not already admin.
pub fn request(action: VddAction, count: u32) -> Result<()> {
    if is_elevated() {
        return apply(action, count);
    }
    elevate(action, count)
}

/// Performs the action in this process. Only meaningful when already elevated.
pub fn apply(action: VddAction, count: u32) -> Result<()> {
    match action {
        VddAction::Install => install(),
        VddAction::SetCount => set_monitor_count(count),
        VddAction::Remove => remove(),
        VddAction::Enable => set_enabled(true),
        VddAction::Disable => set_enabled(false),
    }
}

#[cfg(windows)]
fn set_monitor_count(count: u32) -> Result<()> {
    let changed = write_monitor_count(count.clamp(1, 8))?;
    install()?;
    if changed {
        // The driver reads vdd_settings.xml once at start, so the new count needs a restart.
        restart()?;
    }
    Ok(())
}

/// Rewrites `<monitors><count>` in the settings file the driver reads, returning whether it moved.
#[cfg(windows)]
fn write_monitor_count(count: u32) -> Result<bool> {
    use anyhow::{anyhow, Context};

    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if !path.exists() {
        let template = bundled_settings_template()
            .ok_or_else(|| anyhow!("bundled vdd_settings.xml was not found next to the app"))?;
        std::fs::copy(&template, &path).with_context(|| {
            format!(
                "failed to copy {} to {}",
                template.display(),
                path.display()
            )
        })?;
    }

    let settings =
        std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let updated = replace_monitor_count(&settings, count)?;
    if updated == settings {
        return Ok(false);
    }
    std::fs::write(&path, updated).with_context(|| format!("failed to write {}", path.display()))?;
    crate::logging::append(format!("virtual display count set to {count}"));
    Ok(true)
}

/// Swaps the count in the driver's XML without pulling in a parser; the file is ours to begin with.
fn replace_monitor_count(settings: &str, count: u32) -> Result<String> {
    use anyhow::anyhow;

    let Some(monitors_start) = settings.find("<monitors>") else {
        let anchor = settings
            .find("<vdd_settings>")
            .ok_or_else(|| anyhow!("vdd_settings.xml has no <vdd_settings> element"))?
            + "<vdd_settings>".len();
        let mut updated = settings.to_string();
        updated.insert_str(
            anchor,
            &format!("\n    <monitors>\n        <count>{count}</count>\n    </monitors>"),
        );
        return Ok(updated);
    };
    let monitors_end = settings[monitors_start..]
        .find("</monitors>")
        .map(|offset| monitors_start + offset)
        .ok_or_else(|| anyhow!("vdd_settings.xml has an unterminated <monitors> element"))?;

    let section = &settings[monitors_start..monitors_end];
    let Some(count_start) = section.find("<count>") else {
        let mut updated = settings.to_string();
        updated.insert_str(
            monitors_start + "<monitors>".len(),
            &format!("\n        <count>{count}</count>"),
        );
        return Ok(updated);
    };
    let value_start = monitors_start + count_start + "<count>".len();
    let value_end = settings[value_start..]
        .find("</count>")
        .map(|offset| value_start + offset)
        .ok_or_else(|| anyhow!("vdd_settings.xml has an unterminated <count> element"))?;

    let mut updated = String::with_capacity(settings.len() + 2);
    updated.push_str(&settings[..value_start]);
    updated.push_str(&count.to_string());
    updated.push_str(&settings[value_end..]);
    Ok(updated)
}

#[cfg(windows)]
fn settings_path() -> std::path::PathBuf {
    // Hard-coded in the driver; it never looks anywhere else.
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    std::path::PathBuf::from(format!("{system_drive}\\VirtualDisplayDriver\\vdd_settings.xml"))
}

#[cfg(windows)]
fn bundled_settings_template() -> Option<std::path::PathBuf> {
    bundled_driver_dir().map(|dir| dir.join("vdd_settings.xml"))
}

#[cfg(windows)]
fn bundled_driver_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe()
        .ok()?
        .parent()
        .map(|parent| parent.join("vdd"))?;
    dir.is_dir().then_some(dir)
}

#[cfg(windows)]
mod win {
    use anyhow::{anyhow, Result};
    use std::ffi::c_void;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW, SetupDiSetClassInstallParamsW,
        SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW, DICD_GENERATE_ID,
        DICS_DISABLE, DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIF_REGISTERDEVICE,
        DIF_REMOVE, DIGCF_ALLCLASSES, DIGCF_PRESENT, DI_REMOVEDEVICE_GLOBAL, INSTALLFLAG_FORCE,
        HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS,
        SP_REMOVEDEVICE_PARAMS,
    };
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    pub const HARDWARE_ID: &str = "Root\\MttVDD";

    pub fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn is_elevated() -> bool {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size = 0_u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            );
            windows_sys::Win32::Foundation::CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }

    /// Re-launches this executable through the UAC prompt and waits for it to finish.
    pub fn elevate(arguments: &str) -> Result<()> {
        let exe = std::env::current_exe()?;
        let exe = to_wide(&exe.to_string_lossy());
        let verb = to_wide("runas");
        let parameters = to_wide(arguments);

        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS;
            info.lpVerb = verb.as_ptr();
            info.lpFile = exe.as_ptr();
            info.lpParameters = parameters.as_ptr();
            info.nShow = SW_HIDE;
            if ShellExecuteExW(&mut info) == 0 {
                return Err(anyhow!(
                    "elevation was declined or failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if !info.hProcess.is_null() {
                WaitForSingleObject(info.hProcess, INFINITE);
                windows_sys::Win32::Foundation::CloseHandle(info.hProcess);
            }
        }
        Ok(())
    }

    struct DeviceInfoList(HDEVINFO);

    impl Drop for DeviceInfoList {
        fn drop(&mut self) {
            unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    fn present_devices() -> Result<DeviceInfoList> {
        // SetupAPI reports failure as an invalid handle, which windows-sys models as a bare isize.
        const INVALID_DEVICE_LIST: HDEVINFO = -1;
        let enumerator = to_wide("ROOT");
        let handle = unsafe {
            SetupDiGetClassDevsW(
                std::ptr::null(),
                enumerator.as_ptr(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_ALLCLASSES,
            )
        };
        if handle == INVALID_DEVICE_LIST {
            return Err(anyhow!(
                "failed to enumerate root devices: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(DeviceInfoList(handle))
    }

    fn is_bundled_vdd(list: HDEVINFO, device: &mut SP_DEVINFO_DATA) -> bool {
        let mut buffer = [0_u16; 512];
        let mut required = 0_u32;
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                list,
                device,
                windows_sys::Win32::Devices::DeviceAndDriverInstallation::SPDRP_HARDWAREID,
                std::ptr::null_mut(),
                buffer.as_mut_ptr() as *mut u8,
                std::mem::size_of_val(&buffer) as u32,
                &mut required,
            )
        };
        if ok == 0 {
            return false;
        }
        // REG_MULTI_SZ: every hardware id, back to back, terminated by an empty string.
        buffer
            .split(|value| *value == 0)
            .filter(|id| !id.is_empty())
            .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case(HARDWARE_ID))
    }

    fn for_each_device(mut action: impl FnMut(HDEVINFO, &mut SP_DEVINFO_DATA)) -> Result<usize> {
        let list = present_devices()?;
        let mut index = 0_u32;
        let mut matched = 0;
        loop {
            let mut device: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
            device.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if unsafe { SetupDiEnumDeviceInfo(list.0, index, &mut device) } == 0 {
                break;
            }
            index += 1;
            if !is_bundled_vdd(list.0, &mut device) {
                continue;
            }
            matched += 1;
            action(list.0, &mut device);
        }
        Ok(matched)
    }

    pub fn device_count() -> usize {
        for_each_device(|_, _| {}).unwrap_or(0)
    }

    fn change_state(state: u32) -> Result<usize> {
        let mut changed = 0;
        for_each_device(|list, device| {
            let mut params: SP_PROPCHANGE_PARAMS = unsafe { std::mem::zeroed() };
            params.ClassInstallHeader.cbSize = std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32;
            params.ClassInstallHeader.InstallFunction = DIF_PROPERTYCHANGE;
            params.StateChange = state;
            params.Scope = DICS_FLAG_GLOBAL;
            params.HwProfile = 0;
            let ok = unsafe {
                SetupDiSetClassInstallParamsW(
                    list,
                    device,
                    &params as *const _ as *const SP_CLASSINSTALL_HEADER,
                    std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32,
                ) != 0
                    && SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, list, device) != 0
            };
            if ok {
                changed += 1;
            } else {
                crate::logging::append(format!(
                    "virtual display state change failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
        })?;
        Ok(changed)
    }

    pub fn set_enabled(enabled: bool) -> Result<()> {
        let state = if enabled { DICS_ENABLE } else { DICS_DISABLE };
        let changed = change_state(state)?;
        crate::logging::append(format!(
            "virtual display devices {}: {changed}",
            if enabled { "enabled" } else { "disabled" }
        ));
        Ok(())
    }

    pub fn restart() -> Result<()> {
        let disabled = change_state(DICS_DISABLE)?;
        std::thread::sleep(std::time::Duration::from_millis(750));
        let enabled = change_state(DICS_ENABLE)?;
        crate::logging::append(format!(
            "virtual display driver restarted (disabled {disabled}, enabled {enabled})"
        ));
        Ok(())
    }

    pub fn remove() -> Result<()> {
        let mut removed = 0;
        for_each_device(|list, device| {
            let mut params: SP_REMOVEDEVICE_PARAMS = unsafe { std::mem::zeroed() };
            params.ClassInstallHeader.cbSize = std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32;
            params.ClassInstallHeader.InstallFunction = DIF_REMOVE;
            params.Scope = DI_REMOVEDEVICE_GLOBAL;
            params.HwProfile = 0;
            let ok = unsafe {
                SetupDiSetClassInstallParamsW(
                    list,
                    device,
                    &params as *const _ as *const SP_CLASSINSTALL_HEADER,
                    std::mem::size_of::<SP_REMOVEDEVICE_PARAMS>() as u32,
                ) != 0
                    && SetupDiCallClassInstaller(DIF_REMOVE, list, device) != 0
            };
            if ok {
                removed += 1;
            } else {
                crate::logging::append(format!(
                    "virtual display removal failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
        })?;
        crate::logging::append(format!("virtual display devices removed: {removed}"));
        Ok(())
    }

    /// The same two steps devcon performs: create the root device node, then bind the INF to it.
    pub fn install(inf: &std::path::Path) -> Result<()> {
        let inf_path = to_wide(&inf.to_string_lossy());
        let mut class_guid = unsafe { std::mem::zeroed() };
        let mut class_name = [0_u16; 64];
        let ok = unsafe {
            SetupDiGetINFClassW(
                inf_path.as_ptr(),
                &mut class_guid,
                class_name.as_mut_ptr(),
                class_name.len() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(anyhow!(
                "failed to read the driver class from {}: {}",
                inf.display(),
                std::io::Error::last_os_error()
            ));
        }

        let list = unsafe { SetupDiCreateDeviceInfoList(&class_guid, std::ptr::null_mut()) };
        if list == -1 {
            return Err(anyhow!(
                "failed to create a device list: {}",
                std::io::Error::last_os_error()
            ));
        }
        let list = DeviceInfoList(list);

        let mut device: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
        device.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
        let created = unsafe {
            SetupDiCreateDeviceInfoW(
                list.0,
                class_name.as_ptr(),
                &class_guid,
                std::ptr::null(),
                std::ptr::null_mut(),
                DICD_GENERATE_ID,
                &mut device,
            )
        };
        if created == 0 {
            return Err(anyhow!(
                "failed to create the virtual display device node: {}",
                std::io::Error::last_os_error()
            ));
        }

        // REG_MULTI_SZ needs the extra terminator after the single id.
        let mut hardware_id = to_wide(HARDWARE_ID);
        hardware_id.push(0);
        let bytes = std::mem::size_of_val(hardware_id.as_slice()) as u32;
        let ok = unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                list.0,
                &mut device,
                windows_sys::Win32::Devices::DeviceAndDriverInstallation::SPDRP_HARDWAREID,
                hardware_id.as_ptr() as *const u8,
                bytes,
            )
        };
        if ok == 0 {
            return Err(anyhow!(
                "failed to set the virtual display hardware id: {}",
                std::io::Error::last_os_error()
            ));
        }

        if unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, list.0, &device) } == 0 {
            return Err(anyhow!(
                "failed to register the virtual display device: {}",
                std::io::Error::last_os_error()
            ));
        }

        let id = to_wide(HARDWARE_ID);
        let mut reboot_required = 0;
        let installed = unsafe {
            UpdateDriverForPlugAndPlayDevicesW(
                std::ptr::null_mut(),
                id.as_ptr(),
                inf_path.as_ptr(),
                INSTALLFLAG_FORCE,
                &mut reboot_required,
            )
        };
        if installed == 0 {
            return Err(anyhow!(
                "failed to bind {} to the virtual display device: {}",
                inf.display(),
                std::io::Error::last_os_error()
            ));
        }

        crate::logging::append("virtual display driver installed");
        Ok(())
    }
}

#[cfg(windows)]
fn is_elevated() -> bool {
    win::is_elevated()
}

#[cfg(windows)]
fn elevate(action: VddAction, count: u32) -> Result<()> {
    let action = match action {
        VddAction::Install => "install",
        VddAction::SetCount => "set-count",
        VddAction::Remove => "remove",
        VddAction::Enable => "enable",
        VddAction::Disable => "disable",
    };
    win::elevate(&format!("vdd --action {action} --count {count}"))
}

#[cfg(windows)]
fn install() -> Result<()> {
    use anyhow::anyhow;

    if win::device_count() > 0 {
        return Ok(());
    }
    let inf = bundled_driver_dir()
        .map(|dir| dir.join("MttVDD.inf"))
        .filter(|inf| inf.exists())
        .ok_or_else(|| anyhow!("bundled MttVDD.inf was not found next to the app"))?;
    win::install(&inf)
}

#[cfg(windows)]
fn remove() -> Result<()> {
    win::remove()
}

#[cfg(windows)]
fn restart() -> Result<()> {
    win::restart()
}

#[cfg(windows)]
fn set_enabled(enabled: bool) -> Result<()> {
    win::set_enabled(enabled)
}

#[cfg(not(windows))]
fn set_enabled(_enabled: bool) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn is_elevated() -> bool {
    true
}

#[cfg(not(windows))]
fn elevate(_action: VddAction, _count: u32) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn install() -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn remove() -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn set_monitor_count(_count: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::replace_monitor_count;

    const SETTINGS: &str = "<vdd_settings>\n    <monitors>\n        <count>1</count>\n    </monitors>\n</vdd_settings>";

    #[test]
    fn an_existing_count_is_replaced_in_place() {
        let updated = replace_monitor_count(SETTINGS, 3).unwrap();
        assert!(updated.contains("<count>3</count>"));
        assert!(updated.contains("</vdd_settings>"));
    }

    #[test]
    fn rewriting_the_same_count_is_a_no_op() {
        assert_eq!(replace_monitor_count(SETTINGS, 1).unwrap(), SETTINGS);
    }

    #[test]
    fn a_missing_monitors_section_is_added() {
        let updated = replace_monitor_count("<vdd_settings>\n</vdd_settings>", 2).unwrap();
        assert!(updated.contains("<monitors>"));
        assert!(updated.contains("<count>2</count>"));
    }

    #[test]
    fn a_monitors_section_without_a_count_gains_one() {
        let updated =
            replace_monitor_count("<vdd_settings><monitors></monitors></vdd_settings>", 4).unwrap();
        assert!(updated.contains("<count>4</count>"));
    }

    #[test]
    fn settings_without_a_root_element_are_rejected() {
        assert!(replace_monitor_count("<other/>", 2).is_err());
    }
}
