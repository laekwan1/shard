//! Point the system resolver at the local DoH forwarder, and put it back.
//!
//! Encrypting lookups only helps if the OS actually sends them to us. Netsh's
//! output is localised and painful to parse, so this drives the DNS client
//! cmdlets instead, which return structured data in any locale.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::process::Command;

/// Keep PowerShell from flashing a console window on every call.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What an adapter's DNS configuration was before we touched it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Saved {
    pub interface_index: u32,
    pub servers: Vec<String>,
}

/// Whether this adapter is already pointing at the address given.
///
/// Used to keep our own forwarder out of what is remembered as the machine's
/// own setting: a run that ended without putting it back leaves the adapter
/// pointing here, and recording that as the original would make it permanent.
pub fn points_at(saved: &Saved, server: &str) -> bool {
    saved.servers.iter().any(|entry| entry == server)
}

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

/// Record the current IPv4 DNS servers of every connected adapter.
pub fn snapshot() -> Result<Vec<Saved>> {
    let json = powershell(
        "Get-DnsClientServerAddress -AddressFamily IPv4 | \
         Where-Object { (Get-NetAdapter -InterfaceIndex $_.InterfaceIndex -ErrorAction SilentlyContinue).Status -eq 'Up' } | \
         Select-Object InterfaceIndex,ServerAddresses | ConvertTo-Json -Compress -Depth 4",
    )?;
    parse_snapshot(&json)
}

/// `ConvertTo-Json` emits a bare object rather than an array for a single
/// adapter, so both shapes have to be accepted.
fn parse_snapshot(json: &str) -> Result<Vec<Saved>> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "InterfaceIndex")]
        interface_index: u32,
        #[serde(rename = "ServerAddresses")]
        server_addresses: Option<Vec<String>>,
    }

    let json = json.trim();
    if json.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Row> = match serde_json::from_str::<Vec<Row>>(json) {
        Ok(rows) => rows,
        Err(_) => vec![serde_json::from_str::<Row>(json).context("DNS 설정을 해석할 수 없습니다")?],
    };
    Ok(rows
        .into_iter()
        .map(|r| Saved { interface_index: r.interface_index, servers: r.server_addresses.unwrap_or_default() })
        .collect())
}

/// Send every connected adapter's DNS to `server`.
pub fn apply(server: &str, adapters: &[Saved]) -> Result<()> {
    if adapters.is_empty() {
        bail!("설정을 변경할 어댑터가 없습니다");
    }
    for adapter in adapters {
        let script = format!(
            "Set-DnsClientServerAddress -InterfaceIndex {} -ServerAddresses '{server}'",
            adapter.interface_index
        );
        if let Err(e) = powershell(&script) {
            tracing::warn!("adapter {} DNS 설정 실패: {e}", adapter.interface_index);
        }
    }
    // Stale entries would keep resolving through the old path for their TTL.
    let _ = powershell("Clear-DnsClientCache");
    Ok(())
}

/// Restore what `snapshot` recorded. Adapters that had no explicit servers go
/// back to DHCP rather than being pinned to whatever they happened to inherit.
pub fn restore(adapters: &[Saved]) -> Result<()> {
    for adapter in adapters {
        let script = if adapter.servers.is_empty() {
            format!(
                "Set-DnsClientServerAddress -InterfaceIndex {} -ResetServerAddresses",
                adapter.interface_index
            )
        } else {
            let list = adapter
                .servers
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "Set-DnsClientServerAddress -InterfaceIndex {} -ServerAddresses {list}",
                adapter.interface_index
            )
        };
        if let Err(e) = powershell(&script) {
            tracing::warn!("adapter {} DNS 복구 실패: {e}", adapter.interface_index);
        }
    }
    let _ = powershell("Clear-DnsClientCache");
    Ok(())
}

/// Address part of a `host:port` listen string, for handing to the OS.
pub fn server_from_listen(listen: &str) -> &str {
    listen.rsplit_once(':').map_or(listen, |(host, _)| host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_array_of_adapters() {
        let json = r#"[{"InterfaceIndex":12,"ServerAddresses":["1.1.1.1","8.8.8.8"]},
                       {"InterfaceIndex":5,"ServerAddresses":[]}]"#;
        let rows = parse_snapshot(json).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], Saved { interface_index: 12, servers: vec!["1.1.1.1".into(), "8.8.8.8".into()] });
        assert!(rows[1].servers.is_empty());
    }

    #[test]
    fn parses_the_single_adapter_shape() {
        // PowerShell collapses a one-element array into a bare object.
        let json = r#"{"InterfaceIndex":7,"ServerAddresses":["192.168.0.1"]}"#;
        let rows = parse_snapshot(json).unwrap();
        assert_eq!(rows, vec![Saved { interface_index: 7, servers: vec!["192.168.0.1".into()] }]);
    }

    #[test]
    fn treats_a_null_server_list_as_dhcp() {
        let json = r#"{"InterfaceIndex":3,"ServerAddresses":null}"#;
        assert!(parse_snapshot(json).unwrap()[0].servers.is_empty());
    }

    #[test]
    fn empty_output_means_no_adapters() {
        assert!(parse_snapshot("   ").unwrap().is_empty());
    }

    #[test]
    fn rejects_unparseable_output() {
        assert!(parse_snapshot("not json").is_err());
    }

    #[test]
    fn strips_the_port_from_a_listen_address() {
        assert_eq!(server_from_listen("127.0.0.1:53"), "127.0.0.1");
        assert_eq!(server_from_listen("127.0.0.1"), "127.0.0.1");
    }
}
