//! Kill switch and IPv6 containment, via the Windows Firewall.
//!
//! The window between a tunnel dropping and the user noticing is exactly when
//! the real address leaks, so the default outbound action is flipped to Block
//! and only the core process and the tunnel adapter are allowed out.
//!
//! Because that state survives a crash, engaging it writes a marker file
//! recording what to restore. `recover_if_needed` is called at startup so a
//! previous crash cannot leave the machine offline with no explanation.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Every rule we create carries this prefix so cleanup never guesses.
const RULE_PREFIX: &str = "Veil-";

fn powershell(script: &str) -> Result<String> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("PowerShell을 실행할 수 없습니다")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What the firewall looked like before we touched it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Saved {
    /// Per-profile default outbound action, keyed by profile name.
    pub outbound_actions: Vec<(String, String)>,
}

fn marker_path() -> PathBuf {
    uikit::config::app_dir(crate::config::APP_NAME).join("killswitch.json")
}

fn read_default_outbound_actions() -> Result<Vec<(String, String)>> {
    let json = powershell(
        "Get-NetFirewallProfile -All | Select-Object Name,DefaultOutboundAction | ConvertTo-Json -Compress",
    )?;
    parse_profiles(&json)
}

/// `ConvertTo-Json` collapses a single row into a bare object.
fn parse_profiles(json: &str) -> Result<Vec<(String, String)>> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "DefaultOutboundAction")]
        action: serde_json::Value,
    }

    let json = json.trim();
    if json.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Row> = match serde_json::from_str::<Vec<Row>>(json) {
        Ok(rows) => rows,
        Err(_) => vec![serde_json::from_str::<Row>(json).context("방화벽 상태를 해석할 수 없습니다")?],
    };
    Ok(rows
        .into_iter()
        .map(|r| {
            // The cmdlet returns an enum that serialises as a number or a name
            // depending on the Windows build.
            let action = match &r.action {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => match n.as_u64() {
                    Some(2) => "Allow".to_string(),
                    Some(4) => "Block".to_string(),
                    _ => "NotConfigured".to_string(),
                },
                _ => "NotConfigured".to_string(),
            };
            (r.name, action)
        })
        .collect())
}

pub struct KillSwitch {
    saved: Saved,
}

impl KillSwitch {
    /// Flip the firewall to deny-by-default and punch through only what the
    /// tunnel needs. `core_binary` is allowed out so it can reach the server;
    /// everything else has to travel via the tunnel adapter.
    pub fn engage(core_binary: &Path, block_ipv6: bool) -> Result<Self> {
        let saved = Saved { outbound_actions: read_default_outbound_actions()? };
        // Persist before changing anything: a crash between the two would
        // otherwise leave no record of what to restore.
        let _ = std::fs::create_dir_all(marker_path().parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(&marker_path(), serde_json::to_vec_pretty(&saved)?);

        remove_rules();

        // Allow rules first, so there is never an instant where the core itself
        // is blocked from reconnecting.
        let core = core_binary.display().to_string();
        powershell(&format!(
            "New-NetFirewallRule -DisplayName '{RULE_PREFIX}core-out' -Direction Outbound \
             -Action Allow -Program '{core}' -Profile Any | Out-Null"
        ))
        .context("코어 허용 규칙을 만들 수 없습니다")?;

        // DHCP has to keep working or the machine loses its lease and the
        // tunnel goes down with it.
        powershell(&format!(
            "New-NetFirewallRule -DisplayName '{RULE_PREFIX}dhcp-out' -Direction Outbound \
             -Action Allow -Protocol UDP -RemotePort 67,68 -Profile Any | Out-Null"
        ))
        .ok();

        // Traffic that has already entered the tunnel leaves through the
        // virtual adapter, which sing-box names.
        powershell(&format!(
            "New-NetFirewallRule -DisplayName '{RULE_PREFIX}tun-out' -Direction Outbound \
             -Action Allow -InterfaceAlias 'sing-box*' -Profile Any | Out-Null"
        ))
        .ok();

        if block_ipv6 {
            powershell(&format!(
                "New-NetFirewallRule -DisplayName '{RULE_PREFIX}v6-block' -Direction Outbound \
                 -Action Block -RemoteAddress ::/0 -Profile Any | Out-Null"
            ))
            .ok();
        }

        powershell("Set-NetFirewallProfile -All -DefaultOutboundAction Block")
            .context("기본 아웃바운드 정책을 변경할 수 없습니다")?;

        Ok(Self { saved })
    }

    pub fn disengage(&mut self) {
        restore(&self.saved);
    }
}

impl Drop for KillSwitch {
    fn drop(&mut self) {
        self.disengage();
    }
}

fn remove_rules() {
    let _ = powershell(&format!(
        "Get-NetFirewallRule -DisplayName '{RULE_PREFIX}*' -ErrorAction SilentlyContinue | Remove-NetFirewallRule"
    ));
}

fn restore(saved: &Saved) {
    remove_rules();
    for (profile, action) in &saved.outbound_actions {
        let action = if action == "Block" { "Block" } else { "Allow" };
        if let Err(e) =
            powershell(&format!("Set-NetFirewallProfile -Name {profile} -DefaultOutboundAction {action}"))
        {
            tracing::error!("방화벽 프로필 {profile} 복구 실패: {e}");
        }
    }
    if saved.outbound_actions.is_empty() {
        // Nothing recorded: the safe fallback is to let traffic out again
        // rather than leave the machine offline.
        let _ = powershell("Set-NetFirewallProfile -All -DefaultOutboundAction Allow");
    }
    let _ = std::fs::remove_file(marker_path());
}

/// Undo a kill switch left engaged by a crash. Call once at startup.
///
/// Returns true when something was actually recovered, so the UI can say so
/// instead of the user silently wondering why the network came back.
pub fn recover_if_needed() -> bool {
    let path = marker_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return false };
    let saved: Saved = match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(_) => {
            // A corrupt marker still means the switch was engaged.
            tracing::warn!("킬 스위치 기록이 손상되었습니다; 기본값으로 복구합니다");
            Saved::default()
        }
    };
    tracing::warn!("이전 실행이 킬 스위치를 남겼습니다; 방화벽을 복구합니다");
    restore(&saved);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_array_of_profiles() {
        let json = r#"[{"Name":"Domain","DefaultOutboundAction":"Allow"},
                       {"Name":"Private","DefaultOutboundAction":"Block"}]"#;
        let rows = parse_profiles(json).unwrap();
        assert_eq!(rows, vec![
            ("Domain".to_string(), "Allow".to_string()),
            ("Private".to_string(), "Block".to_string()),
        ]);
    }

    #[test]
    fn parses_the_single_profile_shape() {
        let json = r#"{"Name":"Public","DefaultOutboundAction":"Allow"}"#;
        assert_eq!(parse_profiles(json).unwrap(), vec![("Public".to_string(), "Allow".to_string())]);
    }

    #[test]
    fn maps_numeric_enum_values() {
        // Some Windows builds serialise the enum as its numeric value.
        let json = r#"[{"Name":"Domain","DefaultOutboundAction":2},
                       {"Name":"Private","DefaultOutboundAction":4},
                       {"Name":"Public","DefaultOutboundAction":0}]"#;
        let rows = parse_profiles(json).unwrap();
        assert_eq!(rows[0].1, "Allow");
        assert_eq!(rows[1].1, "Block");
        assert_eq!(rows[2].1, "NotConfigured");
    }

    #[test]
    fn empty_output_is_not_an_error() {
        assert!(parse_profiles("").unwrap().is_empty());
    }

    #[test]
    fn rejects_unparseable_output() {
        assert!(parse_profiles("nonsense").is_err());
    }

    #[test]
    fn saved_state_round_trips_through_json() {
        let saved = Saved {
            outbound_actions: vec![("Domain".into(), "Allow".into()), ("Public".into(), "Allow".into())],
        };
        let text = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<Saved>(&text).unwrap(), saved);
    }
}
