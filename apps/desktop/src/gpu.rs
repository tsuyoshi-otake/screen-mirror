//! DXGI adapter discovery so the sender and the receiver can each pin their work to one GPU.
//!
//! The adapter index here is the DXGI enumeration index, which is exactly what the GStreamer
//! `d3d11` elements take in their `adapter` property, and the LUID is packed the way their
//! `adapter-luid` properties expose it. That lets pipeline building match a user choice against
//! the per-device encoder/decoder element variants GStreamer registers.

pub const AUTO: &str = "auto";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GpuAdapter {
    /// DXGI adapter index; matches the `adapter` property of the d3d11 elements.
    pub index: u32,
    /// DXGI adapter LUID; matches the `adapter-luid` property of the d3d11/nvcodec/qsv/amf elements.
    pub luid: i64,
    pub vendor_id: u32,
    /// PCI device identifier reported by DXGI. This is diagnostic context only; routing is
    /// capability-based so new devices do not require a hard-coded generation table.
    pub device_id: u32,
    pub description: String,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl GpuAdapter {
    pub fn vendor(&self) -> GpuVendor {
        match self.vendor_id {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            _ => GpuVendor::Other,
        }
    }

    /// Stable, human-editable identifier written to config.toml and shown in the tray menu.
    pub fn selection(&self) -> String {
        self.description.clone()
    }

    pub fn summary(&self) -> String {
        format!(
            "index={} luid={} vendor=0x{:04X} device=0x{:04X} name={}",
            self.index, self.luid, self.vendor_id, self.device_id, self.description
        )
    }
}

pub fn is_auto(selection: &str) -> bool {
    let selection = selection.trim();
    selection.is_empty() || selection.eq_ignore_ascii_case(AUTO)
}

/// Resolves a config/CLI selection to one adapter. `None` means "let GStreamer decide", either
/// because the selection is `auto` or because the named GPU is not present anymore.
pub fn resolve(selection: &str) -> Option<GpuAdapter> {
    if is_auto(selection) {
        return None;
    }

    let needle = selection.trim();
    let adapters = adapters();
    if let Ok(index) = needle.parse::<u32>() {
        if let Some(found) = adapters.iter().find(|adapter| adapter.index == index) {
            return Some(found.clone());
        }
    }

    let needle = needle.to_ascii_lowercase();
    let found = adapters
        .iter()
        .find(|adapter| adapter.description.to_ascii_lowercase() == needle)
        .or_else(|| {
            adapters
                .iter()
                .find(|adapter| adapter.description.to_ascii_lowercase().contains(&needle))
        });

    if found.is_none() {
        crate::logging::append(format!(
            "configured GPU {:?} was not found; using automatic GPU selection",
            selection.trim()
        ));
    }
    found.cloned()
}

/// Resolves the GPU used to render a receiver session. Explicit choices retain the normal
/// resolution rules, while `auto` follows the primary attached Windows display so decoding and
/// presentation prefer the GPU that owns the visible output.
pub fn resolve_receiver(selection: &str) -> Option<GpuAdapter> {
    if !is_auto(selection) {
        return resolve(selection);
    }

    let monitors = crate::monitors::enumerate_monitors();
    let target = monitors
        .iter()
        .find(|monitor| monitor.attached && monitor.primary)
        .or_else(|| monitors.iter().find(|monitor| monitor.attached));
    let gpu = target.and_then(|monitor| {
        monitor
            .monitor_handle
            .and_then(adapter_for_monitor_handle)
            .or_else(|| adapter_for_display_device_name(&monitor.adapter_name))
    });

    if let Some(gpu) = gpu.as_ref() {
        crate::logging::append(format!(
            "receiver automatic GPU selected from display: {}",
            gpu.summary()
        ));
    } else {
        crate::logging::append(
            "receiver automatic GPU could not be resolved from an attached display; using GStreamer default",
        );
    }

    gpu
}

pub fn print_adapters() {
    let adapters = adapters();
    if adapters.is_empty() {
        crate::console::line("No DXGI adapters found.");
        return;
    }
    for adapter in adapters {
        crate::console::line(adapter.summary());
    }
}

#[cfg(windows)]
pub fn adapters() -> Vec<GpuAdapter> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let mut adapters = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            crate::logging::append("failed to create a DXGI factory; GPU selection unavailable");
            return adapters;
        };

        let mut index = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(index) {
            if let Ok(desc) = adapter.GetDesc1() {
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                    adapters.push(GpuAdapter {
                        index,
                        luid: luid_to_i64(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart),
                        vendor_id: desc.VendorId,
                        device_id: desc.DeviceId,
                        description: wide_array_to_string(&desc.Description),
                    });
                }
            }
            index += 1;
        }
    }
    adapters
}

#[cfg(not(windows))]
pub fn adapters() -> Vec<GpuAdapter> {
    Vec::new()
}

/// The adapter that owns a monitor, which is also the adapter DXGI desktop duplication capture
/// runs on for that monitor.
#[cfg(windows)]
pub fn adapter_for_monitor_handle(handle: u64) -> Option<GpuAdapter> {
    outputs()
        .into_iter()
        .find_map(|output| (output.monitor_handle == handle).then_some(output.adapter))
}

#[cfg(not(windows))]
pub fn adapter_for_monitor_handle(_handle: u64) -> Option<GpuAdapter> {
    None
}

/// Same lookup keyed by the GDI display device name, e.g. `\\.\DISPLAY2`.
#[cfg(windows)]
pub fn adapter_for_display_device_name(device_name: &str) -> Option<GpuAdapter> {
    outputs().into_iter().find_map(|output| {
        output
            .device_name
            .eq_ignore_ascii_case(device_name)
            .then_some(output.adapter)
    })
}

#[cfg(not(windows))]
pub fn adapter_for_display_device_name(_device_name: &str) -> Option<GpuAdapter> {
    None
}

#[cfg(windows)]
struct AdapterOutput {
    adapter: GpuAdapter,
    device_name: String,
    monitor_handle: u64,
}

#[cfg(windows)]
fn outputs() -> Vec<AdapterOutput> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let mut outputs = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return outputs;
        };

        let mut adapter_index = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(adapter_index) {
            if let Ok(desc) = adapter.GetDesc1() {
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                    let info = GpuAdapter {
                        index: adapter_index,
                        luid: luid_to_i64(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart),
                        vendor_id: desc.VendorId,
                        device_id: desc.DeviceId,
                        description: wide_array_to_string(&desc.Description),
                    };

                    let mut output_index = 0u32;
                    while let Ok(output) = adapter.EnumOutputs(output_index) {
                        if let Ok(output_desc) = output.GetDesc() {
                            outputs.push(AdapterOutput {
                                adapter: info.clone(),
                                device_name: wide_array_to_string(&output_desc.DeviceName),
                                monitor_handle: output_desc.Monitor.0 as u64,
                            });
                        }
                        output_index += 1;
                    }
                }
            }
            adapter_index += 1;
        }
    }
    outputs
}

/// GStreamer packs the LUID the same way `LARGE_INTEGER` does, so the high part keeps its sign.
fn luid_to_i64(high_part: i32, low_part: u32) -> i64 {
    ((high_part as i64) << 32) | low_part as i64
}

#[cfg(windows)]
fn wide_array_to_string(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_selection_covers_blank_and_keyword() {
        assert!(is_auto(""));
        assert!(is_auto("  "));
        assert!(is_auto("Auto"));
        assert!(!is_auto("NVIDIA GeForce RTX 4060 Ti"));
    }

    #[test]
    fn luid_keeps_the_signed_high_part() {
        assert_eq!(luid_to_i64(0, 0x0000_ABCD), 0x0000_ABCD);
        assert_eq!(luid_to_i64(1, 2), 0x1_0000_0002);
        assert_eq!(luid_to_i64(-1, 0), -4_294_967_296);
    }

    #[test]
    fn vendor_ids_map_to_known_vendors() {
        let adapter = |vendor_id| GpuAdapter {
            index: 0,
            luid: 0,
            vendor_id,
            device_id: 0,
            description: String::new(),
        };
        assert_eq!(adapter(0x10DE).vendor(), GpuVendor::Nvidia);
        assert_eq!(adapter(0x1002).vendor(), GpuVendor::Amd);
        assert_eq!(adapter(0x8086).vendor(), GpuVendor::Intel);
        assert_eq!(adapter(0x1414).vendor(), GpuVendor::Other);
    }

    #[test]
    fn summary_includes_pci_device_id_for_diagnostics() {
        let adapter = GpuAdapter {
            index: 2,
            luid: 42,
            vendor_id: 0x8086,
            device_id: 0xB080,
            description: "Intel Arc B390".to_string(),
        };

        assert!(adapter.summary().contains("vendor=0x8086"));
        assert!(adapter.summary().contains("device=0xB080"));
    }
}
