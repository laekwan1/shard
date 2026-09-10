# NOTICE — third-party components

Shard is licensed under the **GNU Affero General Public License, version 3 or
later (AGPL-3.0-or-later)** — see [LICENSE](LICENSE). The AGPL is the strongest
copyleft available to this project: anyone who redistributes it, or offers it to
others over a network, must release their complete corresponding source under the
same terms. That is deliberate — it keeps the project open while denying closed or
commercial re-use.

Shard as a whole is AGPL-3.0 **because it incorporates AGPL-covered code** (see
below). The notices here are preserved to satisfy those upstream licenses.

## Copyleft components — the reason the combined work is AGPL-3.0

- **SideStore** — Apple ID authentication and anisette (`omnisette`, `icloud_auth`),
  ported and adapted into `crates/resign` (`auth.rs`, `dev_api.rs`, parts of
  `engine.rs`) for on-device re-signing.
  License: **AGPL-3.0**. © SideStore contributors — https://github.com/SideStore
  *Modified:* the Rust components were adapted for this app; see the git history of
  `crates/resign`.

- **libVLC / MobileVLCKit** — media playback on iOS (`ios/Podfile`,
  `MobileVLCKit`), dynamically linked.
  License: **LGPL-2.1-or-later**. © VideoLAN and the VLC authors —
  https://www.videolan.org . The LGPL permits relinking against a modified libVLC.

## Permissive components — attribution preserved

- **zsign** — on-device bundle signer, vendored at
  `crates/resign/vendor/zsign` and compiled into the iOS build.
  License: **MIT**. © zhlynn — https://github.com/zhlynn/zsign
  (full MIT text kept at `crates/resign/vendor/zsign/LICENSE`).

- **apple-codesign** (`rcodesign`) — code signing on the desktop target.
  Apache-2.0 / MPL-2.0.

- **idevice** (jkcoxson) — lockdownd / installation_proxy / misagent transport for
  on-device install. See https://github.com/jkcoxson/idevice for its terms.

- Other Rust dependencies are under permissive licenses (MIT / Apache-2.0); see the
  respective crates.

---

If you redistribute Shard or run a modified version as a network service, you must
make the complete corresponding source available under the AGPL-3.0. For anything
outside those terms, contact the copyright holder.
