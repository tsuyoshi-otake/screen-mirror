use anyhow::{anyhow, Context, Result};
use sm_core::diagnostics::{DiagnosticsRequest, DIAGNOSTICS_PORT};
use sm_core::discovery::pin_hash;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct DiagnosticsServer {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl DiagnosticsServer {
    pub fn start(pin: &str) -> Result<Self> {
        let expected_pin_hash = pin_hash(pin)?;
        let listener =
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DIAGNOSTICS_PORT))
                .context("failed to bind diagnostics TCP port")?;
        listener
            .set_nonblocking(true)
            .context("failed to set diagnostics listener nonblocking")?;
        let (stop, stop_rx) = mpsc::channel();
        let thread = thread::spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }

            match listener.accept() {
                Ok((stream, source)) => {
                    if let Err(error) = handle_request(stream, &expected_pin_hash) {
                        crate::logging::append(format!(
                            "diagnostics request from {source} failed: {error:#}"
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    crate::logging::append(format!("diagnostics listener failed: {error}"));
                    thread::sleep(Duration::from_millis(500));
                }
            }
        });

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DiagnosticsServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

pub fn request_remote_report(host: Ipv4Addr, port: u16, pin: &str) -> Result<String> {
    let address = SocketAddrV4::new(host, port);
    let mut stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(3))
        .with_context(|| format!("failed to connect to diagnostics endpoint {host}:{port}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(90)))
        .context("failed to set diagnostics read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .context("failed to set diagnostics write timeout")?;

    let request = DiagnosticsRequest::new(pin)?;
    stream.write_all(&request.encode()?)?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish diagnostics request")?;

    let mut bytes = Vec::new();
    stream
        .take(4 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .context("failed to read diagnostics response")?;
    if bytes.is_empty() {
        return Err(anyhow!("diagnostics endpoint returned no report"));
    }
    String::from_utf8(bytes).context("diagnostics response was not valid UTF-8")
}

pub fn save_report_to_clipboard_and_notepad(report: &str, peer_name: &str) -> Result<PathBuf> {
    let safe_peer = peer_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = std::env::temp_dir().join(format!(
        "ScreenMirror-peer-diagnostics-{}-{}.txt",
        safe_peer,
        timestamp_for_filename()
    ));
    fs::write(&path, report).with_context(|| format!("failed to write {}", path.display()))?;

    let script = r#"
$path = $args[0]
Get-Content -LiteralPath $path -Raw | Set-Clipboard
Start-Process -FilePath "notepad.exe" -ArgumentList "`"$path`""
"#;
    crate::process::hidden_command("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .arg(&path)
        .spawn()
        .context("failed to copy/open peer diagnostics report")?;
    Ok(path)
}

fn handle_request(mut stream: TcpStream, expected_pin_hash: &str) -> Result<()> {
    stream
        .set_nonblocking(false)
        .context("failed to set diagnostics stream blocking")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("failed to set diagnostics server read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .context("failed to set diagnostics server write timeout")?;

    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut stream)
        .take(16 * 1024)
        .read_to_end(&mut bytes)
        .context("failed to read diagnostics request")?;
    let request = DiagnosticsRequest::decode(&bytes)?;
    if request.pin_hash != expected_pin_hash {
        return Err(anyhow!("PIN mismatch"));
    }

    let report = collect_local_report()?;
    stream
        .write_all(report.as_bytes())
        .context("failed to write diagnostics report")?;
    Ok(())
}

fn collect_local_report() -> Result<String> {
    let script = bundled_script_path("diagnose-screen-mirror.ps1")
        .ok_or_else(|| anyhow!("failed to resolve diagnostics script path"))?;
    if !script.exists() {
        return Err(anyhow!(
            "diagnostics script not found: {}",
            script.display()
        ));
    }

    let output = crate::process::hidden_command("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(script)
        .args(["-NoClipboard", "-NoNotepad", "-Stdout"])
        .output()
        .context("failed to run diagnostics script")?;

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            text.push_str("\n\n==== Diagnostics Script Error ====\n");
            text.push_str(stderr.trim_end());
        }
        return Err(anyhow!(
            "diagnostics script failed with status {}",
            output.status
        ));
    }
    if text.trim().is_empty() {
        return Err(anyhow!("diagnostics script produced no output"));
    }
    Ok(text)
}

fn bundled_script_path(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app_dir = exe.parent()?;
    Some(app_dir.join(name))
}

fn timestamp_for_filename() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    seconds.to_string()
}
