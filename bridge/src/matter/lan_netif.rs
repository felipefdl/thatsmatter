//! Matterbridge-inspired LAN interface selection for multi-NIC hosts (HAOS).
//!
//! `rs-matter-stack` `load_netif_state` keeps the **last** operational interface
//! that has any IPv6 address. On Home Assistant OS that often lands on a Docker
//! / hassio virtual face with a zero MAC and ULA-only IPv6, which breaks
//! HomeKit commissioning. This module filters to a single best real LAN
//! interface (or an explicit pin) so the stack never sees the bad faces.
//!
//! Matter IP transport needs **IPv6 on the selected face** (link-local `fe80::`
//! is enough). IPv4-only interfaces are never auto-selected.

use std::net::{Ipv4Addr, Ipv6Addr};

use rs_matter_stack::matter::dm::clusters::gen_diag::{NetifDiag, NetifInfo};
use rs_matter_stack::matter::dm::networks::NetChangeNotif;
use rs_matter_stack::matter::dm::networks::unix::{UnixNetif, UnixNetifs};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::utils::sync::DynBase;

/// Exact interface names treated as virtual / non-LAN.
const VIRTUAL_EXACT: &[&str] = &["lo", "cni0", "docker0"];

/// Name prefixes treated as virtual (Docker, k8s CNI, VPN, tunnels, …).
const VIRTUAL_PREFIXES: &[&str] = &[
  "docker",
  "veth",
  "br-",
  "hassio",
  "flannel",
  "cni",
  "cali",
  "weave",
  "virbr",
  "vboxnet",
  "zt",
  "tailscale",
  "wg",
  "tun",
  "tap",
  "dummy",
  "sit",
  "ip6tnl",
  "kube",
  "podman",
];

/// Filtered network-interface source: exposes only the single best LAN iface
/// (or a pinned name) to the Matter stack.
#[derive(Debug, Clone)]
pub struct LanNetifs {
  /// When set, require this interface name (exact match). Missing pin fails start.
  pin: Option<String>,
}

impl LanNetifs {
  /// Build a selector. Empty / whitespace-only pin is treated as auto-select.
  pub fn new(pin: Option<String>) -> Self {
    let pin = pin.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Self { pin }
  }

  /// Validate selection before the Matter stack claims readiness.
  ///
  /// - **Pin missing:** hard error with available interface names.
  /// - **Pin present, no IPv6:** hard error (Matter needs IPv6; link-local OK).
  /// - **Pin present, down:** hard error (do not claim ready until the face is up).
  /// - **Auto:** require at least one IPv6-capable non-virtual operational face.
  pub fn validate_for_start(&self) -> Result<(), String> {
    let ifaces = UnixNetifs
      .get()
      .map_err(|err| format!("failed to enumerate network interfaces: {err}"))?;

    if let Some(pin) = self.pin.as_deref() {
      match ifaces.iter().find(|n| n.name == pin) {
        None => {
          let available = available_iface_names(&ifaces);
          return Err(format!(
            "mdns_interface pin `{pin}` not found among host interfaces. \
             Available: [{available}]. Clear the option for auto-select or set a valid name."
          ));
        }
        Some(iface) if !has_matter_ipv6(iface) => {
          return Err(format!(
            "pinned interface `{pin}` has no IPv6 address; Matter requires IPv6 on the LAN face \
             (link-local fe80:: is enough). Enable IPv6 or pick another interface."
          ));
        }
        Some(iface) if !iface.operational => {
          return Err(format!(
            "pinned interface `{pin}` is down (not operational). Bring the link up or clear \
             the LAN interface option for auto-select."
          ));
        }
        Some(_) => {}
      }
      return Ok(());
    }

    // Auto-select: need a real (non-virtual) operational face with IPv6.
    let has_ipv6_lan = ifaces.iter().any(|iface| {
      !is_loopback_name(&iface.name) && !is_virtual_name(&iface.name) && iface.operational && has_matter_ipv6(iface)
    });
    if !has_ipv6_lan {
      let available = available_iface_names(&ifaces);
      return Err(format!(
        "no operational IPv6-capable non-virtual LAN interface for Matter \
         (link-local fe80:: is enough). Host interfaces: [{available}]. \
         Enable IPv6 on eth0/wlan0 or set mdns_interface to a face that has IPv6."
      ));
    }
    Ok(())
  }

  /// Log every host interface with score and selected flag (call once at start).
  pub fn log_inventory(&self) {
    let ifaces = match UnixNetifs.get() {
      Ok(v) => v,
      Err(err) => {
        tracing::warn!(error = %err, "failed to enumerate network interfaces");
        return;
      }
    };

    let selected_name = self.pick(&ifaces).map(|n| n.name.clone());

    for iface in &ifaces {
      let score = score_iface(iface);
      let virtual_name = is_virtual_name(&iface.name);
      let selected = selected_name.as_deref() == Some(iface.name.as_str());
      tracing::info!(
        name = %iface.name,
        operational = iface.operational,
        mac = %format_mac(&iface.hw_addr),
        ipv4s = ?iface.ipv4addrs,
        ipv6s = ?iface.ipv6addrs,
        has_ipv6 = has_matter_ipv6(iface),
        score,
        virtual = virtual_name,
        selected,
        "LAN netif inventory"
      );
    }

    match selected_name {
      Some(name) => {
        if let Some(pin) = self.pin.as_deref() {
          if pin == name {
            tracing::info!(%name, "Matter LAN interface pinned by config");
          } else {
            // Should not happen: missing pin fails validate_for_start and pick returns None.
            tracing::error!(
              %name,
              pin,
              "Matter LAN interface selected differs from pin (unexpected)"
            );
          }
        } else {
          tracing::info!(%name, "Matter LAN interface auto-selected (requires IPv6)");
        }
      }
      None => {
        if let Some(pin) = self.pin.as_deref() {
          tracing::error!(
            pin,
            "pinned Matter LAN interface is not usable (missing, or no IPv6); stack will not advertise"
          );
        } else {
          tracing::error!(
            "no operational IPv6-capable non-virtual LAN interface for Matter; \
             stack will not advertise until one appears"
          );
        }
      }
    }
  }

  /// Choose the single interface the stack should see.
  ///
  /// Pin rules:
  /// - name missing → `None` (no silent auto-fallback; start already fails hard)
  /// - present but down → still selected (wait for recovery)
  /// - present, no IPv6 → `None` (unhealthy; Matter needs IPv6)
  ///
  /// Auto rules: only operational non-virtual faces **with IPv6** compete; never
  /// claim an IPv4-only selection. Virtual fallback only if it has IPv6, with a warn.
  fn pick(&self, ifaces: &[UnixNetif]) -> Option<UnixNetif> {
    if let Some(pin) = self.pin.as_deref() {
      if let Some(iface) = ifaces.iter().find(|n| n.name == pin) {
        if !has_matter_ipv6(iface) {
          tracing::error!(
            pin,
            "pinned interface has no IPv6; Matter requires IPv6 on the LAN face \
             (link-local fe80:: is enough) — not exposing this face to the stack"
          );
          return None;
        }
        if !iface.operational {
          tracing::warn!(pin, "pinned interface is down; waiting for recovery via NetChangeNotif");
        }
        return Some(iface.clone());
      }
      let available = available_iface_names(ifaces);
      tracing::error!(
        pin,
        available = %available,
        "mdns_interface pin not found among host interfaces; not falling back to auto-select"
      );
      return None;
    }

    // Prefer real LAN: non-virtual, operational, with IPv6, scored.
    let mut best: Option<(i32, &UnixNetif)> = None;
    for iface in ifaces {
      if is_loopback_name(&iface.name) || is_virtual_name(&iface.name) {
        continue;
      }
      if !iface.operational || !has_matter_ipv6(iface) {
        continue;
      }
      let score = score_iface(iface);
      match best {
        Some((best_score, _)) if best_score >= score => {}
        _ => best = Some((score, iface)),
      }
    }

    if let Some((_, iface)) = best {
      return Some(iface.clone());
    }

    // No non-virtual IPv6 LAN: check whether any IPv4-only real faces exist so we
    // can log a precise diagnosis (never select them).
    let ipv4_only_real: Vec<&str> = ifaces
      .iter()
      .filter(|iface| {
        !is_loopback_name(&iface.name)
          && !is_virtual_name(&iface.name)
          && iface.operational
          && !has_matter_ipv6(iface)
          && iface.ipv4addrs.iter().copied().any(usable_ipv4)
      })
      .map(|i| i.name.as_str())
      .collect();
    if !ipv4_only_real.is_empty() {
      tracing::error!(
        faces = ?ipv4_only_real,
        "IPv4-only LAN face(s) present but Matter needs IPv6 (link-local OK); not selecting them"
      );
    }

    // Fallback: best operational non-loopback **with IPv6** (may be virtual) — never silent.
    let mut fallback: Option<(i32, &UnixNetif)> = None;
    for iface in ifaces {
      if is_loopback_name(&iface.name) || !iface.operational || !has_matter_ipv6(iface) {
        continue;
      }
      let score = score_iface(iface);
      match fallback {
        Some((best_score, _)) if best_score >= score => {}
        _ => fallback = Some((score, iface)),
      }
    }

    if let Some((_, iface)) = fallback {
      tracing::warn!(
        name = %iface.name,
        "no preferred IPv6 LAN interface; using best operational non-loopback face with IPv6"
      );
      Some(iface.clone())
    } else {
      None
    }
  }
}

impl DynBase for LanNetifs {}

impl NetifDiag for LanNetifs {
  fn netifs(&self, f: &mut dyn FnMut(&NetifInfo<'_>) -> Result<(), Error>) -> Result<(), Error> {
    let ifaces = UnixNetifs.get()?;
    if let Some(selected) = self.pick(&ifaces) {
      f(&selected.to_netif_info())?;
    }
    Ok(())
  }
}

impl NetChangeNotif for LanNetifs {
  async fn wait_changed(&self) {
    // Same polling fallback as UnixNetifs: stack re-reads after wake.
    UnixNetifs.wait_changed().await;
  }
}

/// Loopback names: `lo`, `lo0`, `lo1`, …
pub(crate) fn is_loopback_name(name: &str) -> bool {
  name == "lo" || (name.starts_with("lo") && name.len() > 2 && name[2..].chars().all(|c| c.is_ascii_digit()))
}

/// Whether the interface name looks like a virtual / container / VPN face.
pub(crate) fn is_virtual_name(name: &str) -> bool {
  if is_loopback_name(name) {
    return true;
  }
  if VIRTUAL_EXACT.contains(&name) {
    return true;
  }
  VIRTUAL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// Non-zero Ethernet MAC in the first 6 bytes of the 8-byte Matter hw_addr.
pub(crate) fn has_real_mac(hw_addr: &[u8; 8]) -> bool {
  hw_addr[..6].iter().any(|&b| b != 0)
}

/// Non-loopback, non-link-local IPv4 suitable for LAN Matter.
pub(crate) fn usable_ipv4(addr: Ipv4Addr) -> bool {
  !addr.is_unspecified() && !addr.is_loopback() && !addr.is_link_local() && !addr.is_multicast() && !addr.is_broadcast()
}

/// Preferable IPv6 (ULA or global — not loopback / unspecified / multicast / link-local).
pub(crate) fn usable_ipv6(addr: Ipv6Addr) -> bool {
  !addr.is_unspecified() && !addr.is_loopback() && !addr.is_multicast() && !addr.is_unicast_link_local()
}

/// Any unicast IPv6 usable by Matter, including link-local `fe80::` (enough for LAN).
pub(crate) fn matter_ipv6(addr: Ipv6Addr) -> bool {
  !addr.is_unspecified() && !addr.is_loopback() && !addr.is_multicast()
}

/// Whether the interface has at least one Matter-usable IPv6 address (link-local OK).
pub(crate) fn has_matter_ipv6(iface: &UnixNetif) -> bool {
  iface.ipv6addrs.iter().copied().any(matter_ipv6)
}

/// Higher is better. IPv6-less faces score 0 for the IPv6 axes so they never beat
/// an IPv6-capable peer when both are otherwise equal (auto-select also filters them out).
pub(crate) fn score_iface(iface: &UnixNetif) -> i32 {
  let mut score = 0i32;
  if iface.operational {
    score += 1000;
  }
  if has_real_mac(&iface.hw_addr) {
    score += 100;
  }
  if iface.ipv4addrs.iter().copied().any(usable_ipv4) {
    score += 50;
  }
  // Any Matter-usable IPv6 (including link-local) is required in practice; score it high.
  if has_matter_ipv6(iface) {
    score += 200;
  }
  // Prefer non-link-local (ULA/global) when choosing among IPv6 faces.
  if iface.ipv6addrs.iter().copied().any(usable_ipv6) {
    score += 20;
  }
  score
}

fn format_mac(hw_addr: &[u8; 8]) -> String {
  format!(
    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
    hw_addr[0], hw_addr[1], hw_addr[2], hw_addr[3], hw_addr[4], hw_addr[5]
  )
}

fn available_iface_names(ifaces: &[UnixNetif]) -> String {
  if ifaces.is_empty() {
    return "(none)".to_string();
  }
  ifaces.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// Select among a provided inventory (unit-test helper; mirrors [`LanNetifs::pick`]).
#[cfg(test)]
pub(crate) fn select_from_inventory(ifaces: &[UnixNetif], pin: Option<&str>) -> Option<String> {
  LanNetifs::new(pin.map(str::to_string)).pick(ifaces).map(|n| n.name)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::{Ipv4Addr, Ipv6Addr};

  fn iface(name: &str, operational: bool, mac: [u8; 6], ipv4: &[Ipv4Addr], ipv6: &[Ipv6Addr]) -> UnixNetif {
    let mut hw = [0u8; 8];
    hw[..6].copy_from_slice(&mac);
    UnixNetif {
      name: name.to_string(),
      hw_addr: hw,
      ipv4addrs: ipv4.to_vec(),
      ipv6addrs: ipv6.to_vec(),
      operational,
      netif_index: 1,
    }
  }

  #[test]
  fn is_virtual_name_skips_container_and_vpn() {
    assert!(is_virtual_name("lo"));
    assert!(is_virtual_name("lo0"));
    assert!(is_virtual_name("docker0"));
    assert!(is_virtual_name("veth1234"));
    assert!(is_virtual_name("br-abc123"));
    assert!(is_virtual_name("hassio"));
    assert!(is_virtual_name("flannel.1"));
    assert!(is_virtual_name("cni0"));
    assert!(is_virtual_name("cali123"));
    assert!(is_virtual_name("tailscale0"));
    assert!(is_virtual_name("wg0"));
    assert!(is_virtual_name("tun0"));
    assert!(is_virtual_name("podman0"));
    assert!(!is_virtual_name("eth0"));
    assert!(!is_virtual_name("enp1s0"));
    assert!(!is_virtual_name("wlan0"));
    assert!(!is_virtual_name("en0"));
    // Real bridge names without docker-style `br-` prefix.
    assert!(!is_virtual_name("br0"));
  }

  #[test]
  fn has_real_mac_rejects_zeros() {
    assert!(!has_real_mac(&[0; 8]));
    assert!(has_real_mac(&[0x02, 0x42, 0xac, 0x11, 0x00, 0x02, 0, 0]));
  }

  #[test]
  fn usable_ipv4_rejects_loopback_and_link_local() {
    assert!(!usable_ipv4(Ipv4Addr::new(0, 0, 0, 0)));
    assert!(!usable_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
    assert!(!usable_ipv4(Ipv4Addr::new(169, 254, 1, 1)));
    assert!(usable_ipv4(Ipv4Addr::new(192, 168, 1, 10)));
    assert!(usable_ipv4(Ipv4Addr::new(10, 0, 0, 5)));
  }

  #[test]
  fn matter_ipv6_accepts_link_local() {
    assert!(matter_ipv6("fe80::1".parse().unwrap()));
    assert!(matter_ipv6("fd00::1".parse().unwrap()));
    assert!(!matter_ipv6(Ipv6Addr::UNSPECIFIED));
    assert!(!matter_ipv6(Ipv6Addr::LOCALHOST));
  }

  #[test]
  fn score_prefers_real_lan_over_virtual_ula() {
    let eth = iface(
      "eth0",
      true,
      [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
      &[Ipv4Addr::new(192, 168, 1, 10)],
      &["fe80::1".parse().unwrap(), "fd00::1".parse().unwrap()],
    );
    let docker = iface(
      "docker0",
      true,
      [0; 6],
      &[Ipv4Addr::new(172, 17, 0, 1)],
      &["fe80::2".parse().unwrap()],
    );
    assert!(score_iface(&eth) > score_iface(&docker));
  }

  #[test]
  fn score_ipv6_beats_ipv4_only_peer() {
    let with_v6 = iface(
      "eth0",
      true,
      [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
      &[Ipv4Addr::new(192, 168, 1, 10)],
      &["fe80::1".parse().unwrap()],
    );
    let v4_only = iface(
      "eth1",
      true,
      [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
      &[Ipv4Addr::new(192, 168, 1, 11)],
      &[],
    );
    assert!(score_iface(&with_v6) > score_iface(&v4_only));
  }

  #[test]
  fn select_skips_virtual_and_picks_best_lan() {
    let ifaces = vec![
      iface(
        "lo",
        true,
        [0; 6],
        &[Ipv4Addr::new(127, 0, 0, 1)],
        &["::1".parse().unwrap()],
      ),
      iface(
        "docker0",
        true,
        [0; 6],
        &[Ipv4Addr::new(172, 17, 0, 1)],
        &["fe80::d".parse().unwrap()],
      ),
      iface(
        "hassio",
        true,
        [0x02, 0x42, 0x00, 0x00, 0x00, 0x01],
        &[Ipv4Addr::new(172, 30, 32, 1)],
        &["fe80::1".parse().unwrap()],
      ),
      iface(
        "enp1s0",
        true,
        [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        &[Ipv4Addr::new(192, 168, 1, 50)],
        &["fe80::abcd".parse().unwrap(), "fd12:3456::1".parse().unwrap()],
      ),
      iface(
        "veth99",
        true,
        [0x02, 0x42, 0xac, 0x11, 0x00, 0x02],
        &[],
        &["fe80::99".parse().unwrap()],
      ),
    ];
    assert_eq!(select_from_inventory(&ifaces, None).as_deref(), Some("enp1s0"));
  }

  #[test]
  fn select_prefers_link_local_ipv6_over_ipv4_only() {
    let ifaces = vec![
      iface(
        "eth0",
        true,
        [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        &[Ipv4Addr::new(192, 168, 1, 10)],
        &["fe80::1".parse().unwrap()],
      ),
      iface(
        "eth1",
        true,
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        &[Ipv4Addr::new(10, 0, 0, 2)],
        &[],
      ),
    ];
    assert_eq!(select_from_inventory(&ifaces, None).as_deref(), Some("eth0"));
  }

  #[test]
  fn select_rejects_ipv4_only_auto() {
    let ifaces = vec![iface(
      "eth0",
      true,
      [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
      &[Ipv4Addr::new(192, 168, 1, 10)],
      &[],
    )];
    assert_eq!(select_from_inventory(&ifaces, None), None);
  }

  #[test]
  fn pin_selects_exact_name() {
    let ifaces = vec![
      iface(
        "eth0",
        true,
        [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        &[Ipv4Addr::new(192, 168, 1, 10)],
        &["fd00::1".parse().unwrap()],
      ),
      iface(
        "wlan0",
        true,
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        &[Ipv4Addr::new(192, 168, 1, 20)],
        &["fd00::2".parse().unwrap()],
      ),
    ];
    assert_eq!(select_from_inventory(&ifaces, Some("wlan0")).as_deref(), Some("wlan0"));
  }

  #[test]
  fn pin_missing_does_not_fallback() {
    let ifaces = vec![
      iface(
        "docker0",
        true,
        [0; 6],
        &[Ipv4Addr::new(172, 17, 0, 1)],
        &["fe80::1".parse().unwrap()],
      ),
      iface(
        "eth0",
        true,
        [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        &[Ipv4Addr::new(10, 0, 0, 2)],
        &["fd00::a".parse().unwrap()],
      ),
    ];
    assert_eq!(select_from_inventory(&ifaces, Some("does-not-exist")), None);
  }

  #[test]
  fn pin_down_still_selected_when_has_ipv6() {
    let ifaces = vec![iface(
      "eth0",
      false,
      [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
      &[Ipv4Addr::new(192, 168, 1, 10)],
      &["fe80::1".parse().unwrap()],
    )];
    assert_eq!(select_from_inventory(&ifaces, Some("eth0")).as_deref(), Some("eth0"));
  }

  #[test]
  fn pin_no_ipv6_is_rejected() {
    let ifaces = vec![iface(
      "eth0",
      true,
      [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
      &[Ipv4Addr::new(192, 168, 1, 10)],
      &[],
    )];
    assert_eq!(select_from_inventory(&ifaces, Some("eth0")), None);
  }

  #[test]
  fn fallback_uses_virtual_with_ipv6_when_only_virtual_operational() {
    let ifaces = vec![
      iface(
        "lo",
        true,
        [0; 6],
        &[Ipv4Addr::new(127, 0, 0, 1)],
        &["::1".parse().unwrap()],
      ),
      iface(
        "docker0",
        true,
        [0x02, 0x42, 0xac, 0x11, 0x00, 0x02],
        &[Ipv4Addr::new(172, 17, 0, 1)],
        &["fe80::1".parse().unwrap()],
      ),
    ];
    assert_eq!(select_from_inventory(&ifaces, None).as_deref(), Some("docker0"));
  }

  #[test]
  fn empty_pin_is_auto() {
    let lan = LanNetifs::new(Some("  ".into()));
    assert!(lan.pin.is_none());
    let lan = LanNetifs::new(None);
    assert!(lan.pin.is_none());
  }
}
