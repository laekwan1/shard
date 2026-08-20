# Shard

**Serverless DPI/SNI circumvention with a built-in video downloader — for Windows and Android.**

Shard unblocks sites without a proxy, a VPN, or any third party. It rewrites the
outgoing handshake locally so a filter can no longer read which host you are
reaching, then gets out of the way. Because nothing is tunnelled, it costs no
extra hop and no measurable bandwidth. It also carries a browser, so the video a
page plays can be saved to watch offline.

> Shard changes *what a filter can read*, not *who you appear to be*. It gives no
> anonymity — your address is unchanged. When you need to hide your address too,
> its sibling **Veil** tunnels through your own server or Tor.

## Features

- **No server, no account, no VPN permission.** The engine runs inside the app
  and touches only the traffic you browse through it.
- **One window.** Engine switch, browser, and library in a single surface.
- **Video & audio downloads.** Captures HLS/DASH streams and YouTube formats,
  muxes them locally (no FFmpeg), and files them in your Movies/Music folders.
- **Offline library.** Folders, thumbnails, embedded cover art, and a player
  with resume, ordered/shuffle playback, and background audio.
- **Shared core.** The bypass engine, parser, and muxer are one Rust codebase
  behind both the Windows and Android builds.

## Download

Grab the latest build from the [**Releases**](../../releases) page.

| Platform | File | Notes |
|---|---|---|
| Windows 10/11 | `Shard-Windows-*.zip` | Unzip and run `shard.exe`. Keep `WinDivert.dll` and `WinDivert64.sys` beside it. |
| Android 8.0+ | `Shard-android-*.apk` | Sideload the signed APK. Allow notifications to see download progress. |

iOS is in progress — the shared engine already cross-compiles for it.

## Shard vs Veil

The real fork is not "bypass or anonymity" — it is **whether a third party is
involved.** Everything else follows from that one choice.

| | Shard | Veil |
|---|---|---|
| Method | Rewrites the outgoing handshake, locally | Tunnels all traffic through a server or Tor |
| Third party | **none** | one server, or the Tor network |
| Gives you | circumvention | circumvention **and** concealment |
| Speed cost | effectively zero | 5–99%, depending on the route |
| Needs | nothing | a server, and trust in it |

If the goal is simply to reach a blocked site, **Shard is enough.** Reach for
Veil only when a site must not see your home IP, or when a block is one Shard
cannot open.

## Building from source

Requires the Rust toolchain, and — for the Windows UI — the WebView2 runtime.

```bash
# Windows desktop app
cargo build -p shard --release

# Android (needs the Android SDK/NDK and cargo-ndk)
android/gradlew -p android :shard:assembleRelease
```

The engine core also cross-compiles for iOS (`cargo build -p shard-mobile
--target aarch64-apple-ios`) on macOS; a CI workflow verifies it on every push.

## Status & license

Active development. Windows and Android are shipping; iOS is underway.

No open-source license is granted yet — all rights reserved. A formal license
may be added later.
