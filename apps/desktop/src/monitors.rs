#[derive(Clone, Debug)]
pub struct DisplayMonitor {
    pub capture_index: Option<i32>,
    pub monitor_handle: Option<u64>,
    pub adapter_name: String,
    pub adapter_description: String,
    pub monitor_name: Option<String>,
    pub monitor_description: Option<String>,
    pub device_id: String,
    pub attached: bool,
    pub primary: bool,
    pub mirroring: bool,
    pub virtual_candidate: bool,
    pub bundled_virtual_display: bool,
}

#[derive(Clone, Debug)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: Option<u32>,
}

const DISPLAY_MODE_VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const DISPLAY_MODE_VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

fn display_mode_matches(
    mode: &DisplayMode,
    width: u32,
    height: u32,
    refresh_hz: Option<u32>,
) -> bool {
    mode.width == width
        && mode.height == height
        && refresh_hz
            .map(|refresh_hz| mode.refresh_hz == Some(refresh_hz))
            .unwrap_or(true)
}

pub fn print_monitors() {
    let monitors = enumerate_monitors();
    if monitors.is_empty() {
        crate::console::line("No Windows display devices found.");
        return;
    }

    for monitor in monitors {
        let index = monitor
            .capture_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "-".to_string());
        let marker = if monitor.virtual_candidate {
            " virtual-candidate"
        } else {
            ""
        };
        let bundled = if monitor.bundled_virtual_display {
            " bundled-vdd"
        } else {
            ""
        };
        let primary = if monitor.primary { " primary" } else { "" };
        let attached = if monitor.attached {
            " attached"
        } else {
            " detached"
        };
        let mirroring = if monitor.mirroring { " mirroring" } else { "" };

        crate::console::line(format!(
            "[{index}] {} - {}{}{}{}{}{}",
            monitor.adapter_name,
            monitor.adapter_description,
            attached,
            primary,
            mirroring,
            marker,
            bundled
        ));

        if let Some(name) = &monitor.monitor_name {
            crate::console::line(format!("    monitor: {name}"));
        }
        if let Some(description) = &monitor.monitor_description {
            crate::console::line(format!("    name: {description}"));
        }
        if let Some(handle) = monitor.monitor_handle {
            crate::console::line(format!("    hmonitor: {handle}"));
        }
        if !monitor.device_id.is_empty() {
            crate::console::line(format!("    id: {}", monitor.device_id));
        }
    }

    match preferred_virtual_monitor_index() {
        Some(index) => {
            crate::console::line(format!("Preferred virtual display capture index: {index}"))
        }
        None => crate::console::line("Preferred virtual display capture index: not found"),
    }
}

pub fn primary_display_info() -> Option<sm_core::discovery::DisplayInfo> {
    enumerate_monitors()
        .into_iter()
        .find(|monitor| monitor.primary)
        .and_then(|monitor| current_display_mode(&monitor.adapter_name))
        .map(|mode| sm_core::discovery::DisplayInfo {
            width: mode.width,
            height: mode.height,
            refresh_hz: mode.refresh_hz,
        })
}

pub fn request_extended_desktop() {
    #[cfg(windows)]
    {
        let result = crate::process::hidden_command("DisplaySwitch.exe")
            .arg("/extend")
            .status();
        if let Err(error) = result {
            crate::logging::append(format!("failed to request extended desktop: {error}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
}

pub fn preferred_virtual_monitor_index() -> Option<i32> {
    preferred_virtual_monitor().and_then(|monitor| monitor.capture_index)
}

pub fn preferred_virtual_capture_target() -> Option<DisplayMonitor> {
    preferred_virtual_monitor()
}

pub fn resolve_capture_monitor_index(requested: i32, prefer_virtual_display: bool) -> i32 {
    if requested >= 0 || !prefer_virtual_display {
        return requested;
    }

    preferred_virtual_monitor_index().unwrap_or(requested)
}

pub fn detected_nvidia_gpu_name() -> Option<String> {
    enumerate_monitors().into_iter().find_map(|monitor| {
        let combined = format!(
            "{} {} {}",
            monitor.adapter_description, monitor.device_id, monitor.adapter_name
        );
        combined
            .to_ascii_lowercase()
            .contains("nvidia")
            .then_some(combined)
    })
}

pub fn ensure_bundled_virtual_display_installed() {
    if enumerate_monitors()
        .into_iter()
        .any(|monitor| monitor.bundled_virtual_display)
    {
        return;
    }
    if let Err(error) = crate::vdd::request(crate::vdd::VddAction::Install, 1) {
        crate::logging::append(format!("bundled VDD install failed: {error:#}"));
    }
}

pub fn ensure_bundled_virtual_display_ready() -> bool {
    if preferred_bundled_virtual_monitor().is_some() {
        crate::logging::append("bundled VDD already capture-ready; extend request skipped");
        return true;
    }

    ensure_bundled_virtual_display_installed();
    if !wait_for_bundled_virtual_display(std::time::Duration::from_secs(15)) {
        crate::logging::append("bundled VDD did not appear before sender start");
        return false;
    }

    if preferred_bundled_virtual_monitor().is_some() {
        return true;
    }

    request_extended_desktop();
    wait_for_bundled_virtual_capture(std::time::Duration::from_secs(10))
}

pub fn remove_bundled_virtual_display() {
    if !enumerate_monitors()
        .into_iter()
        .any(|monitor| monitor.bundled_virtual_display)
    {
        crate::logging::append("bundled VDD removal skipped: no display endpoint found");
        return;
    }
    if let Err(error) = crate::vdd::request(crate::vdd::VddAction::Remove, 1) {
        crate::logging::append(format!("bundled VDD removal failed: {error:#}"));
    }
}

pub fn wait_for_bundled_virtual_display(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if enumerate_monitors()
            .into_iter()
            .any(|monitor| monitor.bundled_virtual_display)
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    false
}

pub fn wait_for_bundled_virtual_capture(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if preferred_bundled_virtual_monitor().is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    false
}

fn preferred_bundled_virtual_monitor() -> Option<DisplayMonitor> {
    bundled_virtual_capture_targets().into_iter().next()
}

/// Every capturable bundled VDD monitor, in a stable order so receiver N always keeps
/// the same virtual display across rebuilds.
pub fn bundled_virtual_capture_targets() -> Vec<DisplayMonitor> {
    let mut targets: Vec<DisplayMonitor> = enumerate_monitors()
        .into_iter()
        .filter(|monitor| {
            monitor.capture_index.is_some() && monitor.bundled_virtual_display && !monitor.primary
        })
        .collect();
    targets.sort_by(|left, right| left.adapter_name.cmp(&right.adapter_name));
    targets
}

/// Grows (or shrinks) the bundled driver to one virtual monitor per receiver and returns the
/// capture targets that actually materialised.
pub fn ensure_bundled_virtual_display_count(count: usize) -> Vec<DisplayMonitor> {
    let count = count.clamp(1, 8);
    let current_targets = bundled_virtual_capture_targets();
    if current_targets.len() == count {
        return current_targets;
    }

    if let Err(error) = crate::vdd::request(crate::vdd::VddAction::SetCount, count as u32) {
        crate::logging::append(format!("virtual display count change failed: {error:#}"));
        return bundled_virtual_capture_targets();
    }

    // The driver restart tears the monitors down and brings them back. Require the exact
    // requested count twice in succession so a stale pre-restart enumeration cannot start a
    // sender pipeline against monitor handles that are about to disappear. The new endpoints
    // may initially be detached, so extend them once the exact endpoint set is visible.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut stable_samples = 0_u8;
    let mut extended_exact_set = false;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let targets = bundled_virtual_capture_targets();
        if targets.len() == count {
            stable_samples += 1;
            if stable_samples >= 2 {
                return targets;
            }
            continue;
        }
        stable_samples = 0;

        let endpoint_count = enumerate_monitors()
            .into_iter()
            .filter(|monitor| monitor.bundled_virtual_display)
            .count();
        if endpoint_count == count && !extended_exact_set {
            request_extended_desktop();
            extended_exact_set = true;
        } else if endpoint_count != count {
            extended_exact_set = false;
        }
    }

    let targets = bundled_virtual_capture_targets();
    if targets.len() != count {
        crate::logging::append(format!(
            "requested {count} virtual displays but {} are capture-ready",
            targets.len()
        ));
    }
    targets
}

/// Matches one specific virtual display to the receiver that will show it.
pub fn sync_virtual_display_mode(
    monitor: &DisplayMonitor,
    display: Option<&sm_core::discovery::DisplayInfo>,
) -> anyhow::Result<()> {
    let Some(display) = display else {
        return Ok(());
    };
    if display.width < 640 || display.height < 480 {
        return Ok(());
    }
    set_display_mode(
        &monitor.adapter_name,
        display.width,
        display.height,
        display.refresh_hz,
    )
}

fn preferred_virtual_monitor() -> Option<DisplayMonitor> {
    let monitors = enumerate_monitors();
    monitors
        .iter()
        .find(|monitor| {
            monitor.capture_index.is_some() && monitor.bundled_virtual_display && !monitor.primary
        })
        .cloned()
        .or_else(|| {
            monitors.into_iter().find(|monitor| {
                monitor.capture_index.is_some()
                    && monitor.virtual_candidate
                    && !monitor.primary
                    && !looks_like_superdisplay(monitor)
            })
        })
}

impl DisplayMonitor {
    pub fn summary(&self) -> String {
        format!(
            "adapter={} description={} capture-index={:?} hmonitor={:?} attached={} bundled-vdd={} device-id={}",
            self.adapter_name,
            self.adapter_description,
            self.capture_index,
            self.monitor_handle,
            self.attached,
            self.bundled_virtual_display,
            self.device_id
        )
    }
}

#[cfg(windows)]
fn current_display_mode(device_name: &str) -> Option<DisplayMode> {
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS,
    };

    let wide_name = to_wide(device_name);
    let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
    mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    let ok = unsafe { EnumDisplaySettingsW(wide_name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) };
    (ok != 0).then_some(DisplayMode {
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
        refresh_hz: (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency),
    })
}

#[cfg(not(windows))]
fn current_display_mode(_device_name: &str) -> Option<DisplayMode> {
    None
}

#[cfg(windows)]
fn set_display_mode(
    device_name: &str,
    width: u32,
    height: u32,
    refresh_hz: Option<u32>,
) -> anyhow::Result<()> {
    use anyhow::anyhow;
    use windows_sys::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplaySettingsW, CDS_UPDATEREGISTRY, DEVMODEW,
        DISP_CHANGE_BADMODE, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT,
        DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
    };

    let wide_name = to_wide(device_name);
    let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
    mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    let ok = unsafe { EnumDisplaySettingsW(wide_name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) };
    if ok == 0 {
        return Err(anyhow!("failed to read display mode for {device_name}"));
    }

    if display_mode_matches(
        &DisplayMode {
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            refresh_hz: (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency),
        },
        width,
        height,
        refresh_hz,
    ) {
        return Ok(());
    }

    mode.dmFields |= DM_PELSWIDTH | DM_PELSHEIGHT;
    mode.dmPelsWidth = width;
    mode.dmPelsHeight = height;
    if let Some(refresh_hz) = refresh_hz {
        mode.dmFields |= DM_DISPLAYFREQUENCY;
        mode.dmDisplayFrequency = refresh_hz;
    }

    let mut result = unsafe {
        ChangeDisplaySettingsExW(
            wide_name.as_ptr(),
            &mode,
            std::ptr::null_mut(),
            CDS_UPDATEREGISTRY,
            std::ptr::null(),
        )
    };
    let mut verified_refresh_hz = refresh_hz;
    if result == DISP_CHANGE_BADMODE && refresh_hz.is_some() {
        // Receiver panels commonly report fractional rates rounded to an integer (59/60 Hz).
        // Resolution is the compatibility boundary; let Windows choose a supported refresh
        // before declaring an otherwise valid receiver-sized mode unusable.
        mode.dmFields &= !DM_DISPLAYFREQUENCY;
        mode.dmDisplayFrequency = 0;
        result = unsafe {
            ChangeDisplaySettingsExW(
                wide_name.as_ptr(),
                &mode,
                std::ptr::null_mut(),
                CDS_UPDATEREGISTRY,
                std::ptr::null(),
            )
        };
        verified_refresh_hz = None;
        if result == DISP_CHANGE_SUCCESSFUL {
            crate::logging::append(format!(
                "virtual display {device_name} accepted {width}x{height} using a driver-selected refresh rate"
            ));
        }
    }
    if result != DISP_CHANGE_SUCCESSFUL {
        if result == DISP_CHANGE_BADMODE {
            return Err(anyhow!(
                "virtual display {device_name} does not support {width}x{height}: DISP_CHANGE_BADMODE"
            ));
        }
        return Err(anyhow!(
            "failed to set {device_name} to {width}x{height}: DISP_CHANGE={result}"
        ));
    }

    let deadline = std::time::Instant::now() + DISPLAY_MODE_VERIFY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if current_display_mode(device_name).is_some_and(|current| {
            display_mode_matches(&current, width, height, verified_refresh_hz)
        }) {
            crate::logging::append(format!(
                "virtual display {device_name} synced to {width}x{height}"
            ));
            return Ok(());
        }
        std::thread::sleep(DISPLAY_MODE_VERIFY_INTERVAL);
    }

    Err(anyhow!(
        "virtual display {device_name} did not apply {width}x{height} within {}ms",
        DISPLAY_MODE_VERIFY_TIMEOUT.as_millis()
    ))
}

#[cfg(not(windows))]
fn set_display_mode(
    _device_name: &str,
    _width: u32,
    _height: u32,
    _refresh_hz: Option<u32>,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn enumerate_monitors() -> Vec<DisplayMonitor> {
    use windows_sys::Win32::Graphics::Gdi::EnumDisplayDevicesW;

    const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: u32 = 0x0000_0001;
    const DISPLAY_DEVICE_PRIMARY_DEVICE: u32 = 0x0000_0004;
    const DISPLAY_DEVICE_MIRRORING_DRIVER: u32 = 0x0000_0008;

    let handles = monitor_handles_by_device_name();
    let mut monitors = Vec::new();
    let mut adapter_ordinal = 0;
    let mut capture_index = 0;

    loop {
        let mut adapter = empty_display_device();
        let ok = unsafe { EnumDisplayDevicesW(std::ptr::null(), adapter_ordinal, &mut adapter, 0) };
        if ok == 0 {
            break;
        }

        let attached = adapter.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0;
        let mirroring = adapter.StateFlags & DISPLAY_DEVICE_MIRRORING_DRIVER != 0;
        let primary = adapter.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0;

        let adapter_name = wide_array_to_string(&adapter.DeviceName);
        let adapter_description = wide_array_to_string(&adapter.DeviceString);
        let adapter_device_id = wide_array_to_string(&adapter.DeviceID);

        let monitor = first_child_monitor(&adapter_name);
        let monitor_name = monitor
            .as_ref()
            .map(|monitor| wide_array_to_string(&monitor.DeviceName))
            .filter(|value| !value.is_empty());
        let monitor_description = monitor
            .as_ref()
            .map(|monitor| wide_array_to_string(&monitor.DeviceString))
            .filter(|value| !value.is_empty());
        let monitor_device_id = monitor
            .as_ref()
            .map(|monitor| wide_array_to_string(&monitor.DeviceID))
            .filter(|value| !value.is_empty());

        let combined = format!(
            "{adapter_name} {adapter_description} {adapter_device_id} {} {} {}",
            monitor_name.as_deref().unwrap_or(""),
            monitor_description.as_deref().unwrap_or(""),
            monitor_device_id.as_deref().unwrap_or("")
        );

        let bundled_virtual_display = looks_like_bundled_virtual_display(&combined);
        let handle_info = handles
            .iter()
            .find(|info| info.device_name.eq_ignore_ascii_case(&adapter_name));
        let current_capture_index = if attached && !mirroring {
            let fallback_index = capture_index;
            capture_index += 1;
            handle_info
                .map(|info| info.monitor_index)
                .or(Some(fallback_index))
        } else {
            None
        };
        monitors.push(DisplayMonitor {
            capture_index: current_capture_index,
            monitor_handle: handle_info.map(|info| info.handle),
            adapter_name,
            adapter_description,
            monitor_name,
            monitor_description,
            device_id: monitor_device_id.unwrap_or(adapter_device_id),
            attached,
            primary,
            mirroring,
            virtual_candidate: looks_like_virtual_display(&combined),
            bundled_virtual_display,
        });

        adapter_ordinal += 1;
    }

    monitors
}

#[cfg(not(windows))]
pub fn enumerate_monitors() -> Vec<DisplayMonitor> {
    Vec::new()
}

#[cfg(windows)]
struct MonitorHandleInfo {
    device_name: String,
    handle: u64,
    monitor_index: i32,
}

#[cfg(windows)]
fn monitor_handles_by_device_name() -> Vec<MonitorHandleInfo> {
    use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };

    unsafe extern "system" fn callback(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let handles = &mut *(data as *mut Vec<MonitorHandleInfo>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut _) != 0 {
            handles.push(MonitorHandleInfo {
                device_name: wide_array_to_string(&info.szDevice),
                handle: monitor as u64,
                monitor_index: handles.len() as i32,
            });
        }
        TRUE
    }

    let mut handles = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(callback),
            &mut handles as *mut Vec<MonitorHandleInfo> as LPARAM,
        );
    }
    handles
}

#[cfg(windows)]
fn first_child_monitor(
    adapter_name: &str,
) -> Option<windows_sys::Win32::Graphics::Gdi::DISPLAY_DEVICEW> {
    use windows_sys::Win32::Graphics::Gdi::EnumDisplayDevicesW;

    let wide_name = to_wide(adapter_name);
    let mut monitor = empty_display_device();
    let ok = unsafe { EnumDisplayDevicesW(wide_name.as_ptr(), 0, &mut monitor, 0) };
    (ok != 0).then_some(monitor)
}

fn looks_like_virtual_display(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "virtual display",
        "virtualdisplay",
        "virtual-display",
        "idd",
        "mttvdd",
        "vdd",
        "sudovda",
        "parsec",
        "superdisplay",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn looks_like_bundled_virtual_display(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    ["mtt1337", "mttvdd", "vdd by mtt"]
        .iter()
        .any(|needle| value.contains(needle))
}

fn looks_like_superdisplay(monitor: &DisplayMonitor) -> bool {
    format!(
        "{} {} {} {}",
        monitor.adapter_description,
        monitor.device_id,
        monitor.monitor_description.as_deref().unwrap_or(""),
        monitor.monitor_name.as_deref().unwrap_or("")
    )
    .to_ascii_lowercase()
    .contains("superdisplay")
}

#[cfg(windows)]
fn empty_display_device() -> windows_sys::Win32::Graphics::Gdi::DISPLAY_DEVICEW {
    let mut device = windows_sys::Win32::Graphics::Gdi::DISPLAY_DEVICEW {
        cb: 0,
        DeviceName: [0; 32],
        DeviceString: [0; 128],
        StateFlags: 0,
        DeviceID: [0; 128],
        DeviceKey: [0; 128],
    };
    device.cb = std::mem::size_of::<windows_sys::Win32::Graphics::Gdi::DISPLAY_DEVICEW>() as u32;
    device
}

#[cfg(windows)]
fn wide_array_to_string(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_vdd_requires_mtt_identity() {
        assert!(looks_like_bundled_virtual_display(
            r"MONITOR\MTT1337 Generic Monitor (VDD by MTT)"
        ));
        assert!(!looks_like_bundled_virtual_display(
            r"SuperDisplay Virtual Adapter MONITOR\Default_Monitor"
        ));
        assert!(!looks_like_bundled_virtual_display(
            "Virtual Display Driver"
        ));
    }

    #[test]
    fn display_mode_matching_requires_dimensions_and_requested_refresh() {
        let mode = DisplayMode {
            width: 1920,
            height: 1080,
            refresh_hz: Some(60),
        };

        assert!(display_mode_matches(&mode, 1920, 1080, None));
        assert!(display_mode_matches(&mode, 1920, 1080, Some(60)));
        assert!(!display_mode_matches(&mode, 2560, 1080, Some(60)));
        assert!(!display_mode_matches(&mode, 1920, 1080, Some(30)));
    }
}
