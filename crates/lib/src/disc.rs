//! MakeMKV rip filename conventions.
//!
//! The ripper names its output `${CLEAN_TITLE}_${BASENAME}` where MakeMKV's
//! basename is usually `title_tNN.mkv` but is sometimes derived from the disc
//! label instead (`LABEL_tNN.mkv`), and an older single-title path emitted
//! `${CLEAN_TITLE}.mkv` with no index at all.  Parsing recovers two facts:
//! the shared prefix that groups one disc's titles into a set, and the title
//! index within that set.

/// A rip filename decomposed into its disc-set prefix and title index.  A
/// name with no recognizable `_tNN` suffix is a singleton: its own set, no
/// index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RipName {
  pub prefix: String,
  pub title_index: Option<u32>,
}

/// Decompose a rip's file name (not path).  The extension is dropped, a
/// trailing `_tNN` becomes the title index, and a `_title` remnant left by
/// MakeMKV's default basename is stripped from the prefix.
pub fn parse_rip_name(file_name: &str) -> RipName {
  let stem = file_name
    .rfind('.')
    .map_or(file_name, |dot| &file_name[..dot]);
  split_title_index(stem).map_or_else(
    || RipName {
      prefix: stem.to_string(),
      title_index: None,
    },
    |(head, index)| RipName {
      prefix: head.strip_suffix("_title").unwrap_or(head).to_string(),
      title_index: Some(index),
    },
  )
}

/// A human-facing title derived from a rip prefix: underscores become
/// spaces and runs collapse.  Deliberately nothing fancier — real identity
/// resolution (TMDB and friends) is a separate concern; this only has to
/// beat showing the raw filename.
pub fn display_title(prefix: &str) -> String {
  prefix
    .split(|c: char| c == '_' || c.is_whitespace())
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

/// Split a trailing `_t<digits>` title index off a stem, if present.
fn split_title_index(stem: &str) -> Option<(&str, u32)> {
  let idx = stem.rfind("_t")?;
  let digits = &stem[idx + 2..];
  if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  // The .ok() discard is deliberate: a digit run too long for u32 is not a
  // plausible MakeMKV title index, so the name is treated as having no index
  // rather than as an error.
  digits
    .parse::<u32>()
    .ok()
    .map(|index| (&stem[..idx], index))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_default_makemkv_naming() {
    assert_eq!(
      parse_rip_name("The_Matrix_title_t00.mkv"),
      RipName {
        prefix: "The_Matrix".to_string(),
        title_index: Some(0),
      }
    );
    assert_eq!(
      parse_rip_name("The_Matrix_title_t12.mkv"),
      RipName {
        prefix: "The_Matrix".to_string(),
        title_index: Some(12),
      }
    );
  }

  #[test]
  fn parses_label_based_naming() {
    // MakeMKV sometimes bases the file name on the disc label rather than
    // the literal "title"; the label stays part of the prefix, which still
    // groups the set consistently.
    assert_eq!(
      parse_rip_name("Some_Movie_LOGICAL_VOLUME_ID_t03.mkv"),
      RipName {
        prefix: "Some_Movie_LOGICAL_VOLUME_ID".to_string(),
        title_index: Some(3),
      }
    );
  }

  #[test]
  fn treats_unindexed_names_as_singletons() {
    assert_eq!(
      parse_rip_name("Some_Movie.mkv"),
      RipName {
        prefix: "Some_Movie".to_string(),
        title_index: None,
      }
    );
  }

  #[test]
  fn does_not_mistake_words_starting_with_t_for_indexes() {
    assert_eq!(
      parse_rip_name("Night_train.mkv"),
      RipName {
        prefix: "Night_train".to_string(),
        title_index: None,
      }
    );
  }

  #[test]
  fn ignores_absurd_digit_runs() {
    assert_eq!(
      parse_rip_name("Movie_t99999999999999999999.mkv"),
      RipName {
        prefix: "Movie_t99999999999999999999".to_string(),
        title_index: None,
      }
    );
  }

  #[test]
  fn display_title_replaces_and_collapses_underscores() {
    assert_eq!(display_title("The_Matrix"), "The Matrix");
    assert_eq!(display_title("Weird__Name_"), "Weird Name");
    assert_eq!(display_title("Already Fine"), "Already Fine");
  }
}
