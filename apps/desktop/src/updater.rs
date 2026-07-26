use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::thread;
use std::time::Duration;

const OWNER: &str = "tsuyoshi-otake";
const REPO: &str = "screen-mirror";
const INSTALLER_ASSET: &str = "ScreenMirror.msi";
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);

static START: Once = Once::new();

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
                if let Err(error) = check_and_start_update() {
                    eprintln!("update check failed: {error:#}");
                }
                thread::sleep(CHECK_INTERVAL);
            }
        });
    });
}

pub fn start_manual_update_check() {
    thread::spawn(|| {
        if let Err(error) = check_and_start_update() {
            eprintln!("manual update check failed: {error:#}");
            crate::logging::append(format!("manual update check failed: {error:#}"));
        } else {
            crate::logging::append("manual update check finished: no update");
        }
    });
}

fn check_and_start_update() -> Result<()> {
    let release = latest_release()?;
    let latest = parse_version(&release.tag_name).with_context(|| {
        format!(
            "release tag is not a semantic version: {}",
            release.tag_name
        )
    })?;
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;

    if latest <= current {
        return Ok(());
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
    start_installer_and_exit(&installer)?;
    Ok(())
}

fn latest_release() -> Result<GithubRelease> {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let output = Command::new("curl.exe")
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

    let status = Command::new("curl.exe")
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

fn start_installer_and_exit(installer: &PathBuf) -> Result<()> {
    let command = format!(
        "timeout /t 2 /nobreak > nul && msiexec /i \"{}\" /qn /norestart",
        installer.display()
    );
    Command::new("cmd.exe")
        .args([
            "/C",
            "start",
            "\"ScreenMirrorUpdate\"",
            "/MIN",
            "cmd.exe",
            "/C",
            &command,
        ])
        .spawn()
        .context("failed to start MSI update process")?;

    std::process::exit(0);
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
