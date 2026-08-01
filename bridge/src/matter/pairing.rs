//! Real Matter pairing material derived from CSA test commissioning constants.

use rs_matter::dm::devices::test::{TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::pairing::qr::{no_optional_data, CommFlowType, QrPayload};
use rs_matter::pairing::DiscoveryCapabilities;

use crate::catalog::PairingMaterial;

/// Passcode used by `TEST_DEV_COMM` (chip-tool / HA Matter test path).
pub const TEST_PASSCODE: u32 = 20_202_021;

/// Discriminator used by `TEST_DEV_COMM`.
pub const TEST_DISCRIMINATOR: u32 = 3840;

/// Build commissionable pairing material from rs-matter test device constants.
///
/// Controllers (including Home Assistant Matter Server) can use these values to
/// start commissioning when the rs-matter stack is advertising on the network.
pub fn test_device_pairing_material() -> PairingMaterial {
  let setup_code = TEST_DEV_COMM.compute_pretty_pairing_code().to_string();
  let qr_payload = encode_qr_payload();
  PairingMaterial {
    setup_code,
    qr_payload,
    discriminator: TEST_DISCRIMINATOR,
    passcode: TEST_PASSCODE,
  }
}

fn encode_qr_payload() -> String {
  let qr = QrPayload::new_from_basic_info(
    DiscoveryCapabilities::IP,
    CommFlowType::Standard,
    TEST_DEV_COMM.clone(),
    &TEST_DEV_DET,
    no_optional_data,
  );
  let mut buf = [0u8; 1024];
  match qr.as_str(&mut buf) {
    Ok((text, _)) => text.to_string(),
    Err(_) => String::new(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pairing_material_is_non_empty_and_stable() {
    let a = test_device_pairing_material();
    let b = test_device_pairing_material();
    assert!(!a.setup_code.is_empty());
    assert!(!a.qr_payload.is_empty());
    assert!(a.qr_payload.starts_with("MT:"));
    assert_eq!(a.discriminator, TEST_DISCRIMINATOR);
    assert_eq!(a.passcode, TEST_PASSCODE);
    assert_eq!(a.setup_code, b.setup_code);
    assert_eq!(a.qr_payload, b.qr_payload);
    // Pretty code has dashes (11 digit base + 2 dashes for 20202021/3840 style).
    assert!(a.setup_code.contains('-'));
  }
}
