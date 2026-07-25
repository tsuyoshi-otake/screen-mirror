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
    let _ = writeln!(file, "{}", message.as_ref());
}
