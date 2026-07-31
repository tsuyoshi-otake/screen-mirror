use std::fs::{self, OpenOptions};
use std::io::Write;

pub fn append(message: impl AsRef<str>) {
    let Some(base) = dirs::data_local_dir() else {
        return;
    };
    let dir = base.join("ScreenMirror");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("screen-mirror.log");
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = file.write_all(log_line(&timestamp(), message.as_ref()).as_bytes());
}

/// One line, built whole before it is written.
///
/// `writeln!` writes each piece of its format string separately, and the sender, the receiver, and
/// the update checker all append to this file from different threads and processes - two of them
/// mid-line produced `19:59:38.2461959:38.246 receiver...receiver...`. A single write of a single
/// buffer is what makes an append atomic, so the line is assembled first.
fn log_line(timestamp: &str, message: &str) -> String {
    format!("{timestamp} {message}\n")
}

/// This log exists to answer what the stream was doing when the user noticed it stutter, and an
/// undated line cannot be lined up with that moment. Milliseconds are kept because the events being
/// correlated - a packet gap, a decoder renegotiating, a pipeline restart - happen inside a second.
fn timestamp() -> String {
    format_timestamp(&wall_clock())
}

struct WallClock {
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millisecond: u16,
}

fn format_timestamp(clock: &WallClock) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        clock.year,
        clock.month,
        clock.day,
        clock.hour,
        clock.minute,
        clock.second,
        clock.millisecond
    )
}

/// Local time, not UTC: this file is read next to the clock in the Windows tray, and a reader who
/// has to convert time zones to place an event will not bother.
#[cfg(windows)]
fn wall_clock() -> WallClock {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    let mut now: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut now) };
    WallClock {
        year: now.wYear,
        month: now.wMonth,
        day: now.wDay,
        hour: now.wHour,
        minute: now.wMinute,
        second: now.wSecond,
        millisecond: now.wMilliseconds,
    }
}

#[cfg(not(windows))]
fn wall_clock() -> WallClock {
    use std::time::{SystemTime, UNIX_EPOCH};

    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let time_of_day = seconds % 86_400;
    WallClock {
        year,
        month,
        day,
        hour: (time_of_day / 3_600) as u16,
        minute: (time_of_day % 3_600 / 60) as u16,
        second: (time_of_day % 60) as u16,
        millisecond: since_epoch.subsec_millis() as u16,
    }
}

/// Hinnant's days-to-civil conversion, for platforms with no `GetLocalTime`. The result is UTC.
#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (u16, u16, u16) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year as u16, month as u16, day as u16)
}

#[cfg(test)]
mod tests {
    use super::{format_timestamp, log_line, timestamp, WallClock};

    #[test]
    fn a_line_is_one_buffer_ending_in_one_newline() {
        let line = log_line("2026-07-31 20:03:14.510", "receiver GPU selected: index=0");

        assert_eq!(line, "2026-07-31 20:03:14.510 receiver GPU selected: index=0\n");
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn a_timestamp_is_fixed_width_so_lines_stay_aligned_and_sort_by_time() {
        let clock = WallClock {
            year: 2026,
            month: 7,
            day: 4,
            hour: 9,
            minute: 5,
            second: 3,
            millisecond: 42,
        };

        assert_eq!(format_timestamp(&clock), "2026-07-04 09:05:03.042");
    }

    #[test]
    fn the_clock_the_platform_reports_is_a_plausible_wall_clock() {
        let stamped = timestamp();

        assert_eq!(stamped.len(), "2026-07-04 09:05:03.042".len(), "{stamped}");
        assert!(stamped.starts_with("20"), "{stamped}");
    }
}
