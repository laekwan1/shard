use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // WinDivert exists only on Windows. The phone build links this same crate
    // for its parsers and strategy model, so none of the staging below may run
    // when cross-compiling — a link directive for an absent import library
    // fails the build outright.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest_dir.join("..").join("..").join("vendor").join("windivert");

    // WinDivert.lib is an import library; the DLL is loaded at run time and in
    // turn installs WinDivert64.sys, so both must sit beside the executable.
    println!("cargo:rustc-link-search=native={}", vendor.display());
    println!("cargo:rustc-link-lib=dylib=WinDivert");

    // The import library makes WinDivert.dll a load-time dependency, so it must
    // be beside every executable we produce — including test binaries, which
    // cargo puts in `deps/`.
    if let Some(target) = target_dir() {
        for dir in [target.clone(), target.join("deps")] {
            let _ = std::fs::create_dir_all(&dir);
            for file in ["WinDivert.dll", "WinDivert64.sys"] {
                if let Err(e) = std::fs::copy(vendor.join(file), dir.join(file)) {
                    println!("cargo:warning=could not stage {file} into {}: {e}", dir.display());
                }
            }
        }
    }

    embed_resources();
}

/// The executable's icon and its manifest, in one resource.
///
/// The icon is drawn from the same distance field as the tray icon rather than
/// loaded from a file, so the two cannot drift apart — the moment an icon is a
/// checked-in image it is a second place to remember to edit.
///
/// Opening the WinDivert driver requires an elevated token, but only the engine
/// needs it — the browser, downloads, library and player do not. So the
/// manifest asks for no elevation up front (`asInvoker`): the program opens
/// without a UAC prompt, and only when the engine is switched on does it
/// relaunch itself elevated. This is also what lets it be a file handler for
/// media without a prompt on every double-click.
fn embed_resources() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = out.join("shard.ico");
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let marks: Vec<_> = sizes.iter().map(|&s| uikit::icon::shard_at(s, true)).collect();
    std::fs::write(&icon, uikit::icon::to_ico(&marks)).expect("writing the icon");

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(icon.to_str().expect("the path is UTF-8"));

    resource.set_manifest(MANIFEST);
    resource.compile().expect("embedding icon and manifest");
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#;

/// `target/<profile>`, derived from OUT_DIR (`target/<profile>/build/<pkg>/out`).
fn target_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}
