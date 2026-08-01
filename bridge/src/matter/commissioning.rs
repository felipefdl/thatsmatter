//! Per-install Matter commissioning material (setup passcode + discriminator).

use std::path::{Path, PathBuf};

use rand::Rng;
use serde::{Deserialize, Serialize};

/// File under the bridge data dir holding this install's commissioning material.
const COMMISSIONING_FILE: &str = "commissioning.json";

/// Setup passcode bounds from the Matter Core spec (Setup Passcode).
const PASSCODE_MIN: u32 = 1;
const PASSCODE_MAX: u32 = 99_999_998;

/// The discriminator is a 12-bit value.
const DISCRIMINATOR_MAX: u16 = 4095;

/// Setup passcodes the Matter Core spec forbids (trivial or well-known patterns).
const INVALID_PASSCODES: [u32; 12] = [
  0, 11_111_111, 22_222_222, 33_333_333, 44_444_444, 55_555_555, 66_666_666, 77_777_777, 88_888_888, 99_999_999,
  12_345_678, 87_654_321,
];

/// Commissioning material for this install: what a controller needs to pair.
///
/// Generated once per data dir and reused on every start, so the pairing code the
/// user sees stays stable. rs-matter performs no passcode validation of its own, so
/// the spec range and denylist are enforced here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommissioningMaterial {
  pub passcode: u32,
  pub discriminator: u16,
}

impl CommissioningMaterial {
  /// Load `<data_dir>/commissioning.json`, generating and persisting it when absent or invalid.
  pub fn load_or_generate(data_dir: &Path) -> anyhow::Result<Self> {
    let path = Self::path(data_dir);
    if let Some(material) = Self::load(&path)? {
      return Ok(material);
    }

    let material = Self::generate();
    material.persist(data_dir)?;
    tracing::info!(
      path = %path.display(),
      discriminator = material.discriminator,
      "generated per-install Matter commissioning material"
    );
    Ok(material)
  }

  /// Read stored material, returning `Ok(None)` when it is absent or unusable.
  ///
  /// A read error is propagated rather than treated as "unusable": rotating the pairing
  /// code because the data dir went unreadable is worse than refusing to start.
  fn load(path: &Path) -> anyhow::Result<Option<Self>> {
    if !path.exists() {
      return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let material: Self = match serde_json::from_str(&raw) {
      Ok(material) => material,
      Err(err) => {
        tracing::warn!(
          path = %path.display(),
          error = %err,
          "commissioning material is not valid JSON; generating new pairing material"
        );
        return Ok(None);
      }
    };
    if !material.is_valid() {
      tracing::warn!(
        path = %path.display(),
        passcode = material.passcode,
        discriminator = material.discriminator,
        "stored commissioning material violates the Matter spec; generating new pairing material"
      );
      return Ok(None);
    }
    Ok(Some(material))
  }

  fn generate() -> Self {
    let mut rng = rand::thread_rng();
    let passcode = loop {
      let candidate = rng.gen_range(PASSCODE_MIN..=PASSCODE_MAX);
      if !INVALID_PASSCODES.contains(&candidate) {
        break candidate;
      }
    };
    Self {
      passcode,
      discriminator: rng.gen_range(0..=DISCRIMINATOR_MAX),
    }
  }

  fn is_valid(&self) -> bool {
    (PASSCODE_MIN..=PASSCODE_MAX).contains(&self.passcode)
      && !INVALID_PASSCODES.contains(&self.passcode)
      && self.discriminator <= DISCRIMINATOR_MAX
  }

  fn persist(&self, data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = Self::path(data_dir);
    let tmp = data_dir.join(format!("{COMMISSIONING_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
  }

  fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(COMMISSIONING_FILE)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  /// Matter Core spec bounds, restated here so the tests do not read the implementation's consts.
  const SPEC_PASSCODE_RANGE: std::ops::RangeInclusive<u32> = 1..=99_999_998;
  const SPEC_MAX_DISCRIMINATOR: u16 = 4095;
  const SPEC_INVALID_PASSCODES: [u32; 12] = [
    0, 11_111_111, 22_222_222, 33_333_333, 44_444_444, 55_555_555, 66_666_666, 77_777_777, 88_888_888, 99_999_999,
    12_345_678, 87_654_321,
  ];

  fn assert_spec_valid(material: &CommissioningMaterial) {
    assert!(
      SPEC_PASSCODE_RANGE.contains(&material.passcode),
      "passcode {} out of spec range",
      material.passcode
    );
    assert!(
      !SPEC_INVALID_PASSCODES.contains(&material.passcode),
      "passcode {} is on the spec denylist",
      material.passcode
    );
    assert!(
      material.discriminator <= SPEC_MAX_DISCRIMINATOR,
      "discriminator {} exceeds 12 bits",
      material.discriminator
    );
  }

  #[test]
  fn generate_then_reload_is_stable() {
    let dir = tempdir().unwrap();
    let first = CommissioningMaterial::load_or_generate(dir.path()).unwrap();
    assert!(dir.path().join("commissioning.json").exists());
    let second = CommissioningMaterial::load_or_generate(dir.path()).unwrap();
    assert_eq!(first, second);
  }

  #[test]
  fn generated_material_is_within_spec_bounds() {
    for _ in 0..32 {
      let dir = tempdir().unwrap();
      let material = CommissioningMaterial::load_or_generate(dir.path()).unwrap();
      assert_spec_valid(&material);
    }
  }

  #[test]
  fn distinct_data_dirs_get_distinct_passcodes() {
    // A collision is astronomically unlikely; three attempts removes it as a flake source.
    let distinct = (0..3).any(|_| {
      let a = tempdir().unwrap();
      let b = tempdir().unwrap();
      let left = CommissioningMaterial::load_or_generate(a.path()).unwrap();
      let right = CommissioningMaterial::load_or_generate(b.path()).unwrap();
      left.passcode != right.passcode
    });
    assert!(distinct, "two fresh data dirs produced the same passcode 3 times");
  }

  #[test]
  fn denylisted_persisted_passcode_is_regenerated() {
    let dir = tempdir().unwrap();
    let stored = CommissioningMaterial {
      passcode: 12_345_678,
      discriminator: 3840,
    };
    stored.persist(dir.path()).unwrap();
    let loaded = CommissioningMaterial::load_or_generate(dir.path()).unwrap();
    assert_ne!(loaded.passcode, stored.passcode);
    assert_spec_valid(&loaded);
    // The regenerated material is what is now on disk.
    assert_eq!(CommissioningMaterial::load_or_generate(dir.path()).unwrap(), loaded);
  }

  #[test]
  fn out_of_range_persisted_values_are_regenerated() {
    let dir = tempdir().unwrap();
    CommissioningMaterial {
      passcode: 100_000_000,
      discriminator: 3840,
    }
    .persist(dir.path())
    .unwrap();
    assert_spec_valid(&CommissioningMaterial::load_or_generate(dir.path()).unwrap());

    let dir = tempdir().unwrap();
    CommissioningMaterial {
      passcode: 20_202_021,
      discriminator: 5000,
    }
    .persist(dir.path())
    .unwrap();
    assert_spec_valid(&CommissioningMaterial::load_or_generate(dir.path()).unwrap());
  }

  #[test]
  fn unreadable_persisted_material_is_regenerated() {
    let dir = tempdir().unwrap();
    std::fs::write(CommissioningMaterial::path(dir.path()), "not json").unwrap();
    let material = CommissioningMaterial::load_or_generate(dir.path()).unwrap();
    assert_spec_valid(&material);
    assert_eq!(CommissioningMaterial::load_or_generate(dir.path()).unwrap(), material);
  }
}
