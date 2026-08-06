//! Two languages for the tournament feature's reply text (docs/tournament.md
//! §8.10): Traditional Chinese and English, the fallback for everything else.
//!
//! **Detection is per-interaction, not per-guild.** Every interaction carries the
//! invoking user's own client language; `guild_locale` also exists and is
//! deliberately unused, being the server's default rather than anyone's setting.
//!
//! **Shared surfaces don't use this.** A panel is one message many people read
//! and re-render by pressing its buttons, so picking any one reader's language
//! would make it flip; `panel` and `checkin_panel` render both languages instead.
//! This type is for text with exactly one reader.

use crate::Context;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Locale {
    ZhTw,
    En,
}

impl Locale {
    /// Only the exact string `"zh-TW"`; everything else is English. An exact
    /// match rather than prefix-matching or case folding, so the set of locales
    /// that get Chinese is closed and obvious.
    pub(crate) fn from_discord_locale(code: &str) -> Self {
        match code {
            "zh-TW" => Locale::ZhTw,
            _ => Locale::En,
        }
    }

    /// The invoking user's language for a slash command. `None` — a locale
    /// Discord didn't send — is English, like any unrecognized code.
    pub(crate) fn from_context(ctx: Context<'_>) -> Self {
        ctx.locale().map_or(Locale::En, Locale::from_discord_locale)
    }

    /// Picks between two renderings of the same message. Chinese first, matching
    /// the server's primary language.
    pub(crate) fn pick<T>(self, zh: T, en: T) -> T {
        match self {
            Locale::ZhTw => zh,
            Locale::En => en,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_code_is_chinese() {
        assert_eq!(Locale::from_discord_locale("zh-TW"), Locale::ZhTw);
    }

    #[test]
    fn near_misses_fall_back_to_english() {
        // Pins "exact match": no prefix matching, no case folding.
        for code in ["zh", "zh-tw", "ZH-TW", "zh_TW", "zh-TW ", "", "en-US", "xx-YY"] {
            assert_eq!(
                Locale::from_discord_locale(code),
                Locale::En,
                "{code:?} should be English"
            );
        }
    }

    #[test]
    fn pick_returns_the_matching_side() {
        assert_eq!(Locale::ZhTw.pick("中", "en"), "中");
        assert_eq!(Locale::En.pick("中", "en"), "en");
    }
}
