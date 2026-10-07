//! The structural fingerprint of a disc: a hash over the durations and
//! chapter counts of its substantial titles.
//!
//! An output MKV varies by ripper version and settings, so it is useless as
//! a shared key; the fingerprint is computed from MakeMKV's title table
//! instead.  MakeMKV normalizes DVD and Blu-ray structures into the same
//! title model, so one recipe covers both.
//! Only substantial titles take part: the short fringe (menus, trailers,
//! warnings, play-all fragments) is exactly what drifts with MakeMKV's
//! version and minimum-title setting, while features and episodes do not.
//! A single-feature disc carries too little structure to trust on its own,
//! which `is_strong` reports so a cache hit alone is not treated as proof.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tap::Tap;

use crate::rip::makemkv::TitleInfo;

/// Titles shorter than this are fringe and excluded from the recipe.
pub const SUBSTANTIAL_SECS: u64 = 600;
/// Discs with fewer substantial titles than this are weak fingerprints.
pub const STRONG_MIN_TITLES: usize = 2;
/// A version tag in the preimage, so a future recipe change cannot collide
/// with hashes computed under this one.
const RECIPE: &str = "loku-disc-fingerprint-v1";
const RECIPE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
  pub sha256: String,
  pub substantial_titles: usize,
}

impl Fingerprint {
  /// Enough structure to trust a match on its own.
  pub fn is_strong(&self) -> bool {
    self.substantial_titles >= STRONG_MIN_TITLES
  }

  /// Worth remembering at all: a disc with no substantial title hashes to
  /// the same value as every other such disc.
  pub fn is_cacheable(&self) -> bool {
    self.substantial_titles >= 1
  }

  pub fn stored(&self) -> StoredFingerprint {
    StoredFingerprint {
      version: RECIPE_VERSION,
      sha256: self.sha256.clone(),
      substantial_titles: self.substantial_titles,
    }
  }
}

/// The fingerprint as recorded in `disc.json` for external consumers; the
/// index recomputes from the title table rather than trusting this copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoredFingerprint {
  pub version: u32,
  pub sha256: String,
  pub substantial_titles: usize,
}

/// Compute the fingerprint over a title table.  Order-independent: the
/// (duration, chapters) pairs are sorted before hashing.
pub fn compute<'a>(
  titles: impl IntoIterator<Item = &'a TitleInfo>,
) -> Fingerprint {
  let pairs = titles
    .into_iter()
    .filter_map(|title| {
      title
        .duration_secs
        .filter(|secs| *secs >= SUBSTANTIAL_SECS)
        .map(|secs| (secs, title.chapters.unwrap_or(0)))
    })
    .collect::<Vec<(u64, u32)>>()
    .tap_mut(|pairs| pairs.sort_unstable());
  let preimage = std::iter::once(RECIPE.to_string())
    .chain(
      pairs
        .iter()
        .map(|(secs, chapters)| format!("{secs}:{chapters}")),
    )
    .collect::<Vec<_>>()
    .join("\n");
  Fingerprint {
    sha256: format!("{:x}", Sha256::digest(preimage.as_bytes())),
    substantial_titles: pairs.len(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn title(secs: u64, chapters: u32) -> TitleInfo {
    TitleInfo {
      duration_secs: Some(secs),
      chapters: Some(chapters),
      ..TitleInfo::default()
    }
  }

  #[test]
  fn order_does_not_matter() {
    let a = [title(5535, 12), title(1300, 3), title(700, 1)];
    let b = [title(700, 1), title(5535, 12), title(1300, 3)];
    assert_eq!(compute(&a), compute(&b));
  }

  #[test]
  fn fringe_titles_are_ignored() {
    let with_fringe = [title(5535, 12), title(599, 1), title(30, 1)];
    let without = [title(5535, 12)];
    assert_eq!(compute(&with_fringe), compute(&without));
    assert_eq!(compute(&[title(600, 1)]).substantial_titles, 1);
  }

  #[test]
  fn strength_follows_the_substantial_count() {
    assert!(!compute(&[title(5535, 12)]).is_strong());
    assert!(compute(&[title(5535, 12)]).is_cacheable());
    assert!(compute(&[title(5535, 12), title(1300, 3)]).is_strong());
    let empty = compute(&[title(30, 1)]);
    assert!(!empty.is_cacheable());
    assert_eq!(empty.sha256.len(), 64);
  }

  #[test]
  fn golden_hash_is_stable() {
    // Pinned so a recipe change is a deliberate, visible act: cached
    // identities and TheDiscDB matching key on this exact value.
    let fingerprint = compute(&[title(5535, 12), title(1300, 3)]);
    assert_eq!(
      fingerprint.sha256,
      "ea4212b7f1b1976a2c71044ade403c43f0f4f7d0b284ebfdf3c1b7fa30af6b12"
    );
    assert_eq!(fingerprint.stored().version, RECIPE_VERSION);
  }
}
