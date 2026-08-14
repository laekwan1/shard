use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The vendored cores are Windows executables. A phone build links this
    // crate only for its link parser and config generator, so none of the
    // staging below applies there.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    // Stage the core binaries beside the executable so `cargo run` and a copied
    // release folder behave identically.
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest_dir.join("..").join("..").join("vendor");
    if let Some(target) = target_dir() {
        for file in ["sing-box.exe", "wintun.dll"] {
            copy_if_stale(&vendor.join("singbox").join(file), &target.join(file));
        }
        // Tor and its transports are ~50 MB, so only restage what changed.
        for file in [
            "tor.exe",
            "pt_config.json",
            "pluggable_transports/lyrebird.exe",
            "data/geoip",
            "data/geoip6",
        ] {
            let relative: PathBuf = file.split('/').collect();
            copy_if_stale(&vendor.join("tor").join(&relative), &target.join("tor").join(&relative));
        }
    }

    embed_resources();
}

/// The executable's icon and its manifest, in one resource.
///
/// The icon is drawn from the same distance field as the tray icon rather than
/// loaded from a file, so the two cannot drift apart.
///
/// Creating the TUN adapter and the kill-switch firewall rules both require an
/// elevated token, which the manifest asks for up front.
fn embed_resources() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = out.join("veil.ico");
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let marks: Vec<_> = sizes.iter().map(|&s| uikit::icon::veil_at(s, true)).collect();
    std::fs::write(&icon, uikit::icon::to_ico(&marks)).expect("writing the icon");

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(icon.to_str().expect("the path is UTF-8"));

    // Release only, because the resource lands on every executable the crate
    // produces — test harnesses included, and Windows refuses to launch one
    // that demands elevation. This crate's tests could not be run at all: the
    // harness failed to start with "elevation required", and a whole-workspace
    // run stopped there without reaching the crates after it. Debug builds of
    // the app therefore need an elevated terminal to create the adapter, which
    // is where they are run from anyway.
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        resource.set_manifest(MANIFEST);
    }
    resource.compile().expect("embedding icon and manifest");
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#;

/// Copy only when the destination is missing or older, so a rebuild does not
/// shuffle tens of megabytes every time.
fn copy_if_stale(src: &Path, dst: &Path) {
    let Ok(source) = std::fs::metadata(src) else {
        println!("cargo:warning=vendor file missing: {}", src.display());
        return;
    };
    if let Ok(existing) = std::fs::metadata(dst) {
        if existing.len() == source.len() {
            match (existing.modified(), source.modified()) {
                (Ok(a), Ok(b)) if a >= b => return,
                _ => {}
            }
        }
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(src, dst) {
        println!("cargo:warning=could not stage {}: {e}", src.display());
    }
}

/// `target/<profile>`, derived from OUT_DIR (`target/<profile>/build/<pkg>/out`).
fn target_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}
