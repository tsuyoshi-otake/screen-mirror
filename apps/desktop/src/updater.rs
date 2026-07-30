use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Once};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OWNER: &str = "tsuyoshi-otake";
const REPO: &str = "screen-mirror";
const INSTALLER_ASSET: &str = "ScreenMirror.msi";
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const RELEASE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALLER_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MSI_HEADER: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

static START: Once = Once::new();
static UPDATE_STARTED: AtomicBool = AtomicBool::new(false);
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);
static STREAM_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Installing restarts the app, so the background check only does it while nothing is streaming.
pub fn set_session_active(active: bool) {
    SESSION_ACTIVE.store(active, Ordering::SeqCst);
}

/// Sender mode alone is not a session: a sender with no receiver in range waits indefinitely, and
/// treating that as busy would mean an unattended sender never updates itself. The supervisor
/// reports whether it currently has live pipelines instead.
pub fn set_stream_active(active: bool) {
    STREAM_ACTIVE.store(active, Ordering::SeqCst);
}

fn session_active() -> bool {
    SESSION_ACTIVE.load(Ordering::SeqCst) || STREAM_ACTIVE.load(Ordering::SeqCst)
}

#[derive(Debug)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Clone, Debug, Serialize)]
struct UpdateAttempt {
    current_version: String,
    target_version: String,
    started_unix_secs: u64,
}

#[derive(Copy, Clone)]
enum UpdateCheckKind {
    Background,
    Manual,
}

pub fn start_background_update_checks(status: Sender<String>) {
    START.call_once(move || {
        thread::spawn(move || {
            thread::sleep(FIRST_CHECK_DELAY);
            loop {
                crate::logging::append("background update check started (in-process HTTP)");
                // An unattended sender machine never clicks Check for Updates, so install as soon
                // as the app is idle and only defer to a manual run while a session is streaming.
                let kind = if session_active() {
                    UpdateCheckKind::Background
                } else {
                    UpdateCheckKind::Manual
                };
                match check_and_start_update(kind) {
                    Ok(UpdateOutcome::Available { latest }) => {
                        crate::logging::append(format!(
                            "background update available: v{latest}; deferred while a session is active"
                        ));
                        if let Err(error) = status.send(format!(
                            "Status: update v{latest} available; installs when idle"
                        )) {
                            crate::logging::append(format!(
                                "failed to publish background update status: {error}"
                            ));
                        }
                    }
                    Ok(UpdateOutcome::UpToDate { current, latest }) => {
                        crate::logging::append(format!(
                            "background update check finished: up to date current={current} latest={latest}"
                        ));
                    }
                    Ok(UpdateOutcome::UpdateStarted { latest }) => {
                        crate::logging::append(format!(
                            "background update started while idle: v{latest}; exiting for installer"
                        ));
                        let _ = status.send(format!("Status: updating to v{latest}"));
                        thread::sleep(Duration::from_secs(2));
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("update check failed: {error:#}");
                        crate::logging::append(format!(
                            "background update check failed: {error:#}"
                        ));
                    }
                }
                thread::sleep(CHECK_INTERVAL);
            }
        });
    });
}

pub fn start_manual_update_check(status: Sender<String>) {
    thread::spawn(move || {
        let message = match check_and_start_update(UpdateCheckKind::Manual) {
            Ok(UpdateOutcome::UpToDate { current, latest }) => {
                crate::logging::append(format!(
                    "manual update check finished: up to date current={current} latest={latest}"
                ));
                format!("Status: latest version (v{current})")
            }
            Ok(UpdateOutcome::UpdateStarted { latest }) => {
                crate::logging::append(format!("manual update started: v{latest}"));
                format!("Status: updating to v{latest}; app will restart")
            }
            Ok(UpdateOutcome::Available { latest }) => {
                format!("Status: update v{latest} available")
            }
            Err(error) => {
                eprintln!("manual update check failed: {error:#}");
                crate::logging::append(format!("manual update check failed: {error:#}"));
                format!("Error: update check failed: {error:#}")
            }
        };
        if let Err(error) = status.send(message) {
            crate::logging::append(format!("failed to publish update status: {error}"));
        }
        if UPDATE_STARTED.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(2));
            std::process::exit(0);
        }
    });
}

enum UpdateOutcome {
    UpToDate { current: String, latest: String },
    UpdateStarted { latest: String },
    Available { latest: String },
}

fn check_and_start_update(kind: UpdateCheckKind) -> Result<UpdateOutcome> {
    let release = latest_release()?;
    let latest = parse_version(&release.tag_name).with_context(|| {
        format!(
            "release tag is not a semantic version: {}",
            release.tag_name
        )
    })?;
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;

    if latest <= current {
        clear_completed_update_attempt();
        return Ok(UpdateOutcome::UpToDate {
            current: env!("CARGO_PKG_VERSION").to_string(),
            latest: release.tag_name.trim_start_matches('v').to_string(),
        });
    }

    let latest_text = release.tag_name.trim_start_matches('v').to_string();
    if matches!(kind, UpdateCheckKind::Background) {
        return Ok(UpdateOutcome::Available {
            latest: latest_text,
        });
    }

    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(INSTALLER_ASSET))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.ends_with(".msi"))
        })
        .ok_or_else(|| anyhow!("release {} has no MSI asset", release.tag_name))?;
    let installer = download_installer(&asset.browser_download_url, &release.tag_name)?;
    let attempt_path = record_update_attempt(&latest_text)?;
    if let Err(error) = start_update_runner(&installer, &latest_text, &attempt_path) {
        let _ = fs::remove_file(&attempt_path);
        return Err(error);
    }
    UPDATE_STARTED.store(true, Ordering::SeqCst);
    Ok(UpdateOutcome::UpdateStarted {
        latest: latest_text,
    })
}

fn latest_release() -> Result<GithubRelease> {
    use ureq::ResponseExt;

    let url = format!("https://github.com/{OWNER}/{REPO}/releases/latest");
    let agent = http_agent(RELEASE_CHECK_TIMEOUT);
    let response = agent
        .get(&url)
        .header("User-Agent", "screen-mirror-updater")
        .call()
        .context("GitHub release check failed")?;
    let tag_name = parse_latest_release_tag(response.get_uri())?;
    let browser_download_url =
        format!("https://github.com/{OWNER}/{REPO}/releases/download/{tag_name}/{INSTALLER_ASSET}");
    Ok(GithubRelease {
        tag_name,
        assets: vec![GithubAsset {
            name: INSTALLER_ASSET.to_string(),
            browser_download_url,
        }],
    })
}

fn parse_latest_release_tag(uri: &ureq::http::Uri) -> Result<String> {
    anyhow::ensure!(
        uri.scheme_str() == Some("https") && uri.host() == Some("github.com"),
        "latest release redirected outside GitHub"
    );
    let prefix = format!("/{OWNER}/{REPO}/releases/tag/");
    let tag = uri
        .path()
        .strip_prefix(&prefix)
        .filter(|tag| !tag.is_empty() && !tag.contains('/'))
        .ok_or_else(|| anyhow!("latest release redirect has no valid tag"))?;
    parse_version(tag).context("latest GitHub release tag is not semantic")?;
    Ok(tag.to_string())
}

fn download_installer(url: &str, tag_name: &str) -> Result<PathBuf> {
    let dir = update_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let installer = dir.join(format!("ScreenMirror-{tag_name}.msi"));
    let partial = installer.with_extension("msi.part");
    let _ = fs::remove_file(&partial);

    let agent = http_agent(INSTALLER_DOWNLOAD_TIMEOUT);
    let mut response = agent
        .get(url)
        .header("User-Agent", "screen-mirror-updater")
        .call()
        .context("MSI download failed")?;
    let mut file = File::create(&partial)
        .with_context(|| format!("failed to create {}", partial.display()))?;
    let written = io::copy(
        &mut response
            .body_mut()
            .as_reader()
            .take(MAX_INSTALLER_BYTES + 1),
        &mut file,
    )
    .context("failed to download MSI body")?;
    file.sync_all()
        .with_context(|| format!("failed to flush {}", partial.display()))?;
    drop(file);

    if written > MAX_INSTALLER_BYTES {
        let _ = fs::remove_file(&partial);
        return Err(anyhow!("downloaded MSI exceeds 512 MiB limit"));
    }
    validate_msi(&partial)?;
    if installer.exists() {
        fs::remove_file(&installer)
            .with_context(|| format!("failed to replace {}", installer.display()))?;
    }
    fs::rename(&partial, &installer).with_context(|| {
        format!(
            "failed to move downloaded MSI from {} to {}",
            partial.display(),
            installer.display()
        )
    })?;

    Ok(installer)
}

fn validate_msi(path: &PathBuf) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut header = [0_u8; MSI_HEADER.len()];
    file.read_exact(&mut header)
        .with_context(|| format!("downloaded MSI is incomplete: {}", path.display()))?;
    if header != MSI_HEADER {
        let _ = fs::remove_file(path);
        return Err(anyhow!("downloaded update is not a valid MSI container"));
    }
    Ok(())
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};

    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

fn start_update_runner(installer: &PathBuf, latest: &str, attempt_path: &PathBuf) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let dir = update_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let script = dir.join("run-update.ps1");
    let log = dir.join(format!("ScreenMirror-update-v{latest}.log"));
    let failure = dir.join("ScreenMirror-update-last-failure.txt");
    fs::write(&script, update_runner_script())
        .with_context(|| format!("failed to write {}", script.display()))?;

    crate::process::hidden_command("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(&script)
        .arg("-Installer")
        .arg(installer)
        .arg("-CurrentExe")
        .arg(exe)
        .arg("-LogPath")
        .arg(log)
        .arg("-CurrentPid")
        .arg(std::process::id().to_string())
        .arg("-AttemptPath")
        .arg(attempt_path)
        .arg("-FailurePath")
        .arg(failure)
        .arg("-TargetVersion")
        .arg(latest)
        .spawn()
        .context("failed to start update runner")?;

    Ok(())
}

fn update_runner_script() -> &'static str {
    r#"
param(
    [Parameter(Mandatory = $true)]
    [string] $Installer,

    [Parameter(Mandatory = $true)]
    [string] $CurrentExe,

    [Parameter(Mandatory = $true)]
    [string] $LogPath,

    [Parameter(Mandatory = $true)]
    [int] $CurrentPid,

    [Parameter(Mandatory = $true)]
    [string] $AttemptPath,

    [Parameter(Mandatory = $true)]
    [string] $FailurePath,

    [Parameter(Mandatory = $true)]
    [string] $TargetVersion
)

$ErrorActionPreference = "Continue"

function Write-UpdateFailure {
    param(
        [string] $Message,
        [int] $ExitCode
    )

    @(
        "Screen Mirror Update Failure",
        "Generated: $(Get-Date -Format s)",
        "TargetVersion: $TargetVersion",
        "ExitCode: $ExitCode",
        "Message: $Message",
        "Installer: $Installer",
        "MSI log: $LogPath",
        "Attempt state: $AttemptPath"
    ) | Set-Content -LiteralPath $FailurePath -Encoding UTF8
}

$exitDeadline = (Get-Date).AddSeconds(30)
while ((Get-Process -Id $CurrentPid -ErrorAction SilentlyContinue) -and
       (Get-Date) -lt $exitDeadline) {
    Start-Sleep -Milliseconds 250
}

if (Get-Process -Id $CurrentPid -ErrorAction SilentlyContinue) {
    Write-UpdateFailure "Timed out waiting for Screen Mirror process $CurrentPid to exit." 1460
    exit 1460
}

$arguments = @(
    "/i",
    "`"$Installer`"",
    "/qn",
    "/norestart",
    "/L*v",
    "`"$LogPath`""
)
try {
    $process = Start-Process -FilePath "msiexec.exe" -Verb RunAs -ArgumentList $arguments -WindowStyle Hidden -Wait -PassThru
} catch {
    Write-UpdateFailure $_.Exception.Message 1
    if (Test-Path -LiteralPath $CurrentExe) {
        Start-Process -FilePath $CurrentExe -ArgumentList "tray" -WindowStyle Hidden | Out-Null
    }
    exit 1
}

if ($process.ExitCode -ne 0) {
    Write-UpdateFailure "msiexec failed." $process.ExitCode
    if (Test-Path -LiteralPath $CurrentExe) {
        Start-Process -FilePath $CurrentExe -ArgumentList "tray" -WindowStyle Hidden | Out-Null
    }
    exit $process.ExitCode
}

Remove-Item -LiteralPath $AttemptPath, $FailurePath -Force -ErrorAction SilentlyContinue

Start-Sleep -Seconds 5
$running = @(Get-Process screen-mirror -ErrorAction SilentlyContinue)
if ($running.Count -eq 0) {
    $installedExe = Join-Path $env:ProgramFiles "Screen Mirror\screen-mirror.exe"
    if (Test-Path -LiteralPath $installedExe) {
        Start-Process -FilePath $installedExe -ArgumentList "tray" -WindowStyle Hidden | Out-Null
    } elseif (Test-Path -LiteralPath $CurrentExe) {
        Start-Process -FilePath $CurrentExe -ArgumentList "tray" -WindowStyle Hidden | Out-Null
    }
}
"#
}

fn record_update_attempt(target_version: &str) -> Result<PathBuf> {
    let path = update_attempt_path()?;
    let attempt = UpdateAttempt {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        target_version: target_version.to_string(),
        started_unix_secs: unix_now_secs(),
    };
    let bytes = serde_json::to_vec_pretty(&attempt).context("failed to encode update attempt")?;
    fs::write(&path, bytes)
        .with_context(|| format!("failed to write update attempt state {}", path.display()))?;
    Ok(path)
}

fn clear_completed_update_attempt() {
    let Ok(path) = update_attempt_path() else {
        return;
    };
    let _ = fs::remove_file(path);
}

fn update_attempt_path() -> Result<PathBuf> {
    let dir = update_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir.join("update-attempt.json"))
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn update_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir().context("failed to resolve local app data directory")?;
    Ok(base.join("ScreenMirror").join("Updates"))
}

fn parse_version(version: &str) -> Result<(u64, u64, u64)> {
    let trimmed = version.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts
        .next()
        .ok_or_else(|| anyhow!("missing major version"))?
        .parse()?;
    let minor = parts
        .next()
        .ok_or_else(|| anyhow!("missing minor version"))?
        .parse()?;
    let patch = parts
        .next()
        .ok_or_else(|| anyhow!("missing patch version"))?
        .parse()?;
    Ok((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::parse_latest_release_tag;

    #[test]
    fn parses_latest_tag_from_github_redirect() {
        let uri = "https://github.com/tsuyoshi-otake/screen-mirror/releases/tag/v0.1.29"
            .parse()
            .unwrap();

        assert_eq!(parse_latest_release_tag(&uri).unwrap(), "v0.1.29");
    }

    #[test]
    fn rejects_latest_redirect_for_another_repository() {
        let uri = "https://github.com/other/repository/releases/tag/v9.9.9"
            .parse()
            .unwrap();

        assert!(parse_latest_release_tag(&uri).is_err());
    }
}
