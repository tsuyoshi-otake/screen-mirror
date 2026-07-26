use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Once};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const OWNER: &str = "tsuyoshi-otake";
const REPO: &str = "screen-mirror";
const INSTALLER_ASSET: &str = "ScreenMirror.msi";
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

static START: Once = Once::new();
static UPDATE_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub fn start_background_update_checks() {
    START.call_once(|| {
        thread::spawn(|| {
            thread::sleep(FIRST_CHECK_DELAY);
            loop {
                match check_and_start_update() {
                    Ok(UpdateOutcome::UpdateStarted { latest }) => {
                        crate::logging::append(format!(
                            "background update started: v{latest}; exiting for installer"
                        ));
                        thread::sleep(Duration::from_secs(2));
                        std::process::exit(0);
                    }
                    Ok(UpdateOutcome::UpToDate { .. }) => {}
                    Err(error) => eprintln!("update check failed: {error:#}"),
                }
                thread::sleep(CHECK_INTERVAL);
            }
        });
    });
}

pub fn start_manual_update_check(status: Sender<String>) {
    thread::spawn(move || {
        let message = match check_and_start_update() {
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
}

fn check_and_start_update() -> Result<UpdateOutcome> {
    let release = latest_release()?;
    let latest = parse_version(&release.tag_name).with_context(|| {
        format!(
            "release tag is not a semantic version: {}",
            release.tag_name
        )
    })?;
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;

    if latest <= current {
        return Ok(UpdateOutcome::UpToDate {
            current: env!("CARGO_PKG_VERSION").to_string(),
            latest: release.tag_name.trim_start_matches('v').to_string(),
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
    let latest_text = release.tag_name.trim_start_matches('v').to_string();
    start_update_runner(&installer, &latest_text)?;
    UPDATE_STARTED.store(true, Ordering::SeqCst);
    Ok(UpdateOutcome::UpdateStarted {
        latest: latest_text,
    })
}

fn latest_release() -> Result<GithubRelease> {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let output = hidden_command("curl.exe")
        .args(["-fsSL", "-H", "User-Agent: screen-mirror-updater", &url])
        .output()
        .context("failed to start curl.exe for release check")?;

    if !output.status.success() {
        return Err(anyhow!(
            "GitHub release check failed with status {}",
            output.status
        ));
    }

    serde_json::from_slice(&output.stdout).context("failed to parse GitHub release response")
}

fn download_installer(url: &str, tag_name: &str) -> Result<PathBuf> {
    let dir = update_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let installer = dir.join(format!("ScreenMirror-{tag_name}.msi"));

    let status = hidden_command("curl.exe")
        .args([
            "-fL",
            "-H",
            "User-Agent: screen-mirror-updater",
            "-o",
            installer
                .to_str()
                .ok_or_else(|| anyhow!("installer path is not valid UTF-8"))?,
            url,
        ])
        .status()
        .context("failed to start curl.exe for MSI download")?;

    if !status.success() {
        return Err(anyhow!("MSI download failed with status {status}"));
    }

    Ok(installer)
}

fn start_update_runner(installer: &PathBuf, latest: &str) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let dir = update_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let script = dir.join("run-update.ps1");
    let log = dir.join(format!("ScreenMirror-update-v{latest}.log"));
    fs::write(&script, update_runner_script())
        .with_context(|| format!("failed to write {}", script.display()))?;

    hidden_command("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Installer")
        .arg(installer)
        .arg("-CurrentExe")
        .arg(exe)
        .arg("-LogPath")
        .arg(log)
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
    [string] $LogPath
)

$ErrorActionPreference = "Continue"
Start-Sleep -Seconds 2

$arguments = @(
    "/i",
    "`"$Installer`"",
    "/qn",
    "/norestart",
    "/L*v",
    "`"$LogPath`""
)
$process = Start-Process -FilePath "msiexec.exe" -ArgumentList $arguments -WindowStyle Hidden -Wait -PassThru

if ($process.ExitCode -ne 0) {
    if (Test-Path -LiteralPath $CurrentExe) {
        Start-Process -FilePath $CurrentExe -ArgumentList "tray" | Out-Null
    }
    exit $process.ExitCode
}

Start-Sleep -Seconds 5
$running = @(Get-Process screen-mirror -ErrorAction SilentlyContinue)
if ($running.Count -eq 0) {
    $installedExe = Join-Path $env:ProgramFiles "Screen Mirror\screen-mirror.exe"
    if (Test-Path -LiteralPath $installedExe) {
        Start-Process -FilePath $installedExe -ArgumentList "tray" | Out-Null
    } elseif (Test-Path -LiteralPath $CurrentExe) {
        Start-Process -FilePath $CurrentExe -ArgumentList "tray" | Out-Null
    }
}
"#
}

fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    hide_window(&mut command);
    command
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

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
