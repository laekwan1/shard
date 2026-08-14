//! Tor Browser integration.
//!
//! Veil's own Tor profile hides the address, but it cannot do anything about
//! the browser. Fonts, screen size, canvas and WebGL readback, timezone and
//! extension list identify a browser far more precisely than an address does,
//! and they do not change when the packets take a different route. Tor Browser
//! exists to make every user look identical at that layer, which is the part
//! no network tool can supply.
//!
//! So the useful thing to build is not a replacement but a handover: find it,
//! launch it, and keep Veil out of its way.

use anyhow::{anyhow, Context, Result};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const DOWNLOAD_URL: &str = "https://www.torproject.org/download/";
/// Tor Browser ships as a Firefox fork and keeps the executable name.
pub const PROCESS_NAME: &str = "firefox.exe";

pub struct Install {
    /// The bundle root, the directory holding `Browser`.
    pub root: PathBuf,
    pub exe: PathBuf,
}

/// Look where the installer and the usual manual extractions put it.
pub fn locate() -> Option<Install> {
    for root in candidate_roots() {
        let exe = root.join("Browser").join(PROCESS_NAME);
        if exe.exists() {
            return Some(Install { root, exe });
        }
    }
    None
}

fn candidate_roots() -> Vec<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut roots = Vec::new();

    // The bundle is most often extracted to the desktop, including the
    // OneDrive-redirected desktop that many Windows installs use.
    if let Some(profile) = env("USERPROFILE") {
        roots.push(profile.join("Desktop"));
        roots.push(profile.join("OneDrive").join("Desktop"));
        roots.push(profile.join("Downloads"));
        roots.push(profile);
    }
    roots.extend(["LOCALAPPDATA", "APPDATA", "ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(|n| env(n)));
    roots.into_iter().map(|base| base.join("Tor Browser")).collect()
}

/// Start Tor Browser. It brings its own tor daemon and circuits.
pub fn launch(install: &Install) -> Result<()> {
    if !install.exe.exists() {
        return Err(anyhow!("{} 이(가) 없습니다", install.exe.display()));
    }
    Command::new(&install.exe)
        .current_dir(install.root.join("Browser"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("{} 실행 실패", install.exe.display()))?;
    Ok(())
}

/// Open the download page in the default browser.
pub fn open_download_page() -> Result<()> {
    Command::new("cmd")
        .args(["/C", "start", "", DOWNLOAD_URL])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("브라우저를 열 수 없습니다")?;
    Ok(())
}

/// Make sure Tor Browser bypasses the tunnel.
///
/// Running it through Veil would put Tor inside Tor: slower, and it breaks the
/// assumption Tor's own path selection makes about the first hop.
///
/// The rule matches the **full executable path**, not the process name. Tor
/// Browser's binary is called `firefox.exe` like any other Firefox, so matching
/// by name would drag an ordinary Firefox install out of the tunnel as well.
/// Returns true if the rule had to be added.
pub fn ensure_bypassed(install: &Install, direct_paths: &mut Vec<String>) -> bool {
    let path = install.exe.display().to_string();
    if direct_paths.iter().any(|p| p.eq_ignore_ascii_case(&path)) {
        return false;
    }
    direct_paths.push(path);
    true
}

// --- Chromium-family browsers over Tor -------------------------------------

/// A Chromium-based browser we can point at a SOCKS port.
pub struct Chromium {
    pub name: &'static str,
    pub exe: PathBuf,
}

/// Chromium browsers in their usual install locations.
pub fn find_chromium() -> Vec<Chromium> {
    const CANDIDATES: &[(&str, &str)] = &[
        ("Chrome", r"Google\Chrome\Application\chrome.exe"),
        ("Brave", r"BraveSoftware\Brave-Browser\Application\brave.exe"),
        ("Edge", r"Microsoft\Edge\Application\msedge.exe"),
    ];
    let bases: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|n| std::env::var_os(n).map(PathBuf::from))
        .collect();

    let mut found = Vec::new();
    for (name, tail) in CANDIDATES {
        if let Some(exe) = bases.iter().map(|b| b.join(tail)).find(|p| p.exists()) {
            found.push(Chromium { name, exe });
        }
    }
    found
}

/// Launch a Chromium browser whose traffic goes through `socks_port`.
///
/// Nothing is routed at the system level: the proxy is a flag on this one
/// process, so an ordinary browser window started any other way is completely
/// unaffected. That is why this needs no bypass rule at all — and why it is a
/// better answer than excluding `chrome.exe` from the tunnel would be.
///
/// A separate profile directory keeps its cookies and history away from the
/// everyday one.
pub fn launch_via_socks(browser: &Chromium, socks_port: u16, profile_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(profile_dir).context("프로필 폴더를 만들 수 없습니다")?;

    Command::new(&browser.exe)
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        // socks5:// (not socks4) makes the browser resolve names through the
        // proxy, so lookups do not leak to the local network.
        .arg(format!("--proxy-server=socks5://127.0.0.1:{socks_port}"))
        // Without this, WebRTC can open direct UDP paths that reveal the real
        // address regardless of the proxy.
        .arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("{} 실행 실패", browser.name))?;
    Ok(())
}

/// Human-readable location for the UI.
pub fn describe(install: &Install) -> String {
    install.root.display().to_string()
}

/// True when the given path looks like a Tor Browser bundle root.
pub fn is_bundle_root(root: &Path) -> bool {
    root.join("Browser").join(PROCESS_NAME).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_install() -> Install {
        let root = PathBuf::from(r"C:\Users\x\Desktop\Tor Browser");
        Install { exe: root.join("Browser").join(PROCESS_NAME), root }
    }

    #[test]
    fn bypass_rule_uses_the_full_path_not_the_name() {
        // Matching on "firefox.exe" would pull an ordinary Firefox install out
        // of the tunnel too, which is not what the user asked for.
        let install = fake_install();
        let mut direct = Vec::new();
        assert!(ensure_bypassed(&install, &mut direct));
        assert_eq!(direct.len(), 1);
        assert!(direct[0].contains("Tor Browser"), "got: {}", direct[0]);
        assert_ne!(direct[0], PROCESS_NAME);
    }

    #[test]
    fn bypass_rule_is_added_once_and_is_case_insensitive() {
        let install = fake_install();
        let mut direct = vec![install.exe.display().to_string().to_uppercase()];
        assert!(!ensure_bypassed(&install, &mut direct));
        assert_eq!(direct.len(), 1);
    }

    #[test]
    fn candidate_roots_cover_the_usual_places() {
        let roots = candidate_roots();
        assert!(!roots.is_empty());
        let text: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
        assert!(text.iter().any(|r| r.contains("Desktop")), "desktop is the default extraction target");
        assert!(text.iter().any(|r| r.contains("OneDrive")), "many desktops are redirected to OneDrive");
        assert!(text.iter().all(|r| r.ends_with("Tor Browser")));
    }

    #[test]
    fn a_directory_without_the_browser_is_not_a_bundle() {
        assert!(!is_bundle_root(Path::new("C:/definitely/not/here")));
    }
}
