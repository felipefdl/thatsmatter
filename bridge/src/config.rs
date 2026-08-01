//! CLI configuration for the bridge process.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// Matter stack implementation selected at runtime.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum BackendKind {
  /// Offline IPC backend: catalog + command queue, real pairing codes, no network advertise.
  Dev,
  /// Commissionable `rs-matter` stack (IP OnOff; HA Matter / chip-tool can commission).
  #[value(alias = "rs-matter")]
  RsMatter,
}

impl BackendKind {
  /// Wire value matching `protocol/schema.json` `BridgeStatus.matter_backend`.
  pub fn as_wire_str(self) -> &'static str {
    match self {
      Self::Dev => "dev",
      Self::RsMatter => "rs_matter",
    }
  }
}

/// Process configuration from CLI / environment.
#[derive(Debug, Clone, Parser)]
#[command(name = "thatsmatter-bridge", about = "ThatsMatter Matter bridge process", version)]
pub struct Config {
  /// Loopback HTTP IPC listen address (must be 127.0.0.1 or ::1).
  #[arg(long, env = "THATSMATTER_LISTEN", default_value = "127.0.0.1:18465")]
  pub listen: SocketAddr,

  /// Directory for fabric material, exports, and runtime state.
  #[arg(long, env = "THATSMATTER_DATA_DIR", default_value = "./data")]
  pub data_dir: PathBuf,

  /// Bridge name advertised until HA updates config.
  #[arg(long, env = "THATSMATTER_BRIDGE_NAME", default_value = "ThatsMatter")]
  pub bridge_name: String,

  /// Matter backend implementation (`rs_matter` = commissionable; `dev` = offline IPC tests).
  #[arg(
    long,
    env = "THATSMATTER_MATTER_BACKEND",
    value_enum,
    default_value_t = BackendKind::RsMatter
  )]
  pub matter_backend: BackendKind,

  /// Allow non-loopback `--listen` (Docker / LAN). Default rejects non-loopback.
  #[arg(long, env = "THATSMATTER_ALLOW_NON_LOOPBACK", default_value_t = false)]
  pub allow_non_loopback: bool,

  /// LAN interface name for Matter / mDNS (e.g. `eth0`, `enp1s0`). Empty = auto-select.
  #[arg(long, env = "THATSMATTER_MDNS_INTERFACE")]
  pub mdns_interface: Option<String>,
}

impl Config {
  /// Reject non-loopback bind addresses unless explicitly allowed.
  pub fn ensure_loopback(&self) -> anyhow::Result<()> {
    if self.allow_non_loopback {
      return Ok(());
    }
    if !self.listen.ip().is_loopback() {
      anyhow::bail!(
        "--listen must be a loopback address (or pass --allow-non-loopback), got {}",
        self.listen
      );
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

  fn sample_cfg(listen: SocketAddr, allow_non_loopback: bool, backend: BackendKind) -> Config {
    Config {
      listen,
      data_dir: PathBuf::from("./data"),
      bridge_name: "ThatsMatter".into(),
      matter_backend: backend,
      allow_non_loopback,
      mdns_interface: None,
    }
  }

  #[test]
  fn accepts_loopback_v4() {
    let cfg = sample_cfg(
      SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18465),
      false,
      BackendKind::Dev,
    );
    assert!(cfg.ensure_loopback().is_ok());
  }

  #[test]
  fn accepts_loopback_v6() {
    let cfg = sample_cfg(
      SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 18465),
      false,
      BackendKind::Dev,
    );
    assert!(cfg.ensure_loopback().is_ok());
  }

  #[test]
  fn rejects_non_loopback() {
    let cfg = sample_cfg(
      SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 18465),
      false,
      BackendKind::Dev,
    );
    assert!(cfg.ensure_loopback().is_err());
  }

  #[test]
  fn allows_non_loopback_when_flag_set() {
    let cfg = sample_cfg(
      SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 18465),
      true,
      BackendKind::RsMatter,
    );
    assert!(cfg.ensure_loopback().is_ok());
  }

  #[test]
  fn mdns_interface_defaults_to_none() {
    let cfg = sample_cfg(
      SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18465),
      false,
      BackendKind::RsMatter,
    );
    assert_eq!(cfg.mdns_interface, None);
  }
}
