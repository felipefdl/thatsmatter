//! Matter pairing material derived from this install's commissioning data.

use rs_matter::BasicCommData;
use rs_matter::dm::devices::test::TEST_DEV_DET;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::pairing::qr::{CommFlowType, QrPayload, no_optional_data};

use super::commissioning::CommissioningMaterial;
use crate::catalog::PairingMaterial;

/// rs-matter commissioning data for this install (passcode + discriminator).
///
/// The Matter stack and the pairing codes we show must be built from the same
/// values, so both go through here.
pub fn basic_comm_data(material: &CommissioningMaterial) -> BasicCommData {
  BasicCommData {
    password: material.passcode.to_le_bytes().into(),
    discriminator: material.discriminator,
  }
}

/// Build commissionable pairing material from this install's commissioning data.
///
/// Controllers (including Home Assistant Matter Server) can use these values to
/// start commissioning when the rs-matter stack is advertising on the network.
pub fn pairing_material_for(material: &CommissioningMaterial) -> PairingMaterial {
  let comm = basic_comm_data(material);
  PairingMaterial {
    setup_code: comm.compute_pretty_pairing_code().to_string(),
    qr_payload: encode_qr_payload(&comm),
    discriminator: u32::from(material.discriminator),
    passcode: material.passcode,
  }
}

fn encode_qr_payload(comm: &BasicCommData) -> String {
  let qr = QrPayload::new_from_basic_info(
    DiscoveryCapabilities::IP,
    CommFlowType::Standard,
    comm.clone(),
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
  use tempfile::tempdir;

  #[test]
  fn pairing_material_is_non_empty_and_stable() {
    let dir = tempdir().unwrap();
    let material = CommissioningMaterial::load_or_generate(dir.path()).unwrap();
    let a = pairing_material_for(&material);
    let b = pairing_material_for(&material);
    assert!(!a.setup_code.is_empty());
    assert!(!a.qr_payload.is_empty());
    assert!(a.qr_payload.starts_with("MT:"));
    assert_eq!(a.discriminator, u32::from(material.discriminator));
    assert_eq!(a.passcode, material.passcode);
    assert_eq!(a.setup_code, b.setup_code);
    assert_eq!(a.qr_payload, b.qr_payload);
    // Pretty code is 11 digits split by 2 dashes: NNNN-NNNN-NNN.
    assert_eq!(a.setup_code.len(), 13);
    assert_eq!(a.setup_code.matches('-').count(), 2);
  }

  #[test]
  fn distinct_material_yields_distinct_codes() {
    let a = pairing_material_for(&CommissioningMaterial {
      passcode: 20_202_021,
      discriminator: 3840,
    });
    let b = pairing_material_for(&CommissioningMaterial {
      passcode: 20_202_022,
      discriminator: 3841,
    });
    assert_ne!(a.setup_code, b.setup_code);
    assert_ne!(a.qr_payload, b.qr_payload);
  }

  #[test]
  fn comm_data_round_trips_the_passcode() {
    let material = CommissioningMaterial {
      passcode: 87_654_320,
      discriminator: 1234,
    };
    let comm = basic_comm_data(&material);
    assert_eq!(u32::from_le_bytes(*comm.password.access()), material.passcode);
    assert_eq!(comm.discriminator, material.discriminator);
  }
}
