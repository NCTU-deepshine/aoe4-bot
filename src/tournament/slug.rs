//! Validates `/tournament create`'s `slug` argument (docs/tournament.md §8.1).
//! Discord already lowercases and hyphenates channel names on its own, but
//! validating here means the slug stored in `tournaments.slug` and the channel
//! names actually created match what the organizer typed, rather than silently
//! diverging from it.

/// `-register` is the longest of the four suffixes (`-register`/`-bracket`/
/// `-draft`/`-matches`), so bounding the slug to leave room for it bounds every
/// created channel name under the platform's 100-character limit.
const LONGEST_SUFFIX_LEN: usize = "-register".len();
const MAX_SLUG_LEN: usize = 100 - LONGEST_SUFFIX_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlugError {
    Empty,
    TooLong,
    InvalidCharacters,
}

impl SlugError {
    /// User-facing text for the ephemeral refusal in `commands::create`.
    pub(crate) fn message(self) -> String {
        match self {
            SlugError::Empty => "Slug cannot be empty.".to_string(),
            SlugError::TooLong => format!("Slug must be at most {MAX_SLUG_LEN} characters."),
            SlugError::InvalidCharacters => "Slug must be lowercase letters, digits and hyphens only \
                 (e.g. `relic-cup`), with no leading, trailing or doubled hyphen."
                .to_string(),
        }
    }
}

pub(crate) fn validate_slug(slug: &str) -> Result<(), SlugError> {
    if slug.is_empty() {
        return Err(SlugError::Empty);
    }
    if slug.len() > MAX_SLUG_LEN {
        return Err(SlugError::TooLong);
    }
    let legal_chars = slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !legal_chars || slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return Err(SlugError::InvalidCharacters);
    }
    Ok(())
}

/// Derives a slug from a display name (`/tournament create`'s fallback when no
/// `slug` argument is given): lowercase, non-alphanumeric runs collapsed to a
/// single hyphen, trimmed and bounded to `MAX_SLUG_LEN`. Always produces
/// something `validate_slug` accepts, or `None` if the name had nothing
/// sluggable in it (e.g. entirely CJK or punctuation) — the caller falls back to
/// asking for an explicit slug in that case.
pub(crate) fn slugify(name: &str) -> Option<String> {
    let mut slug = String::new();
    let mut last_was_hyphen = true; // suppresses a leading hyphen
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            slug.push(lower);
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(MAX_SLUG_LEN);
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() { None } else { Some(slug) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_kebab_case() {
        assert!(validate_slug("relic-cup").is_ok());
        assert!(validate_slug("a").is_ok());
        assert!(validate_slug("cup-2026").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(validate_slug(""), Err(SlugError::Empty));
    }

    #[test]
    fn rejects_uppercase_and_spaces() {
        assert_eq!(validate_slug("Relic-Cup"), Err(SlugError::InvalidCharacters));
        assert_eq!(validate_slug("relic cup"), Err(SlugError::InvalidCharacters));
    }

    #[test]
    fn rejects_leading_trailing_and_double_hyphen() {
        assert_eq!(validate_slug("-relic"), Err(SlugError::InvalidCharacters));
        assert_eq!(validate_slug("relic-"), Err(SlugError::InvalidCharacters));
        assert_eq!(validate_slug("re--lic"), Err(SlugError::InvalidCharacters));
    }

    #[test]
    fn enforces_the_length_boundary_exactly() {
        let max = "a".repeat(MAX_SLUG_LEN);
        assert!(validate_slug(&max).is_ok());
        let too_long = "a".repeat(MAX_SLUG_LEN + 1);
        assert_eq!(validate_slug(&too_long), Err(SlugError::TooLong));
    }

    #[test]
    fn slugify_lowercases_and_collapses_separators() {
        assert_eq!(slugify("Relic Cup"), Some("relic-cup".to_string()));
        assert_eq!(slugify("  Relic   Cup!!  "), Some("relic-cup".to_string()));
        assert_eq!(slugify("2026 Cup"), Some("2026-cup".to_string()));
    }

    #[test]
    fn slugify_returns_none_when_nothing_sluggable_survives() {
        assert_eq!(slugify("接力賽"), None);
        assert_eq!(slugify("!!!"), None);
        assert_eq!(slugify(""), None);
    }

    #[test]
    fn slugify_always_produces_something_validate_slug_accepts() {
        let long_name = "Relic Cup ".repeat(20);
        let slug = slugify(&long_name).unwrap();
        assert!(validate_slug(&slug).is_ok(), "slugify produced an invalid slug: {slug}");
        assert!(!slug.is_empty());
    }
}
