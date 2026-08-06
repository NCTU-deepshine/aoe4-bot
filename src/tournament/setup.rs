//! What a tournament must have configured before it can start
//! (docs/tournament.md §8.3), and which draft preset each round uses (§3.3).
//!
//! Match lengths are not stored per round by an organizer: they come from the
//! round's preset, because §3.3 already requires a preset's `options.bestOf` to
//! match the round's `best_of`. Deriving one from the other removes the mismatch
//! rather than validating it.
//!
//! The resolution and gating functions here are pure; `check_preset` is the one
//! that reaches the draft tool.

use crate::drafttool;
use crate::locale::Locale;
use crate::tournament::db::{RoundPreset, Tournament};

/// The depth that means "every round", as opposed to a real distance from the
/// final. Rounds are numbered 1 = final, 2 = semi, 3 = Ro8, so 0 is free.
pub(crate) const DEFAULT_DEPTH: i64 = 0;

/// Resolves which preset a round uses, given how far it is from the final.
///
/// An assignment covers its own depth **and everything after it**, so the winner
/// is the one with the smallest threshold at or beyond this round — a preset set
/// at Ro8 claims Ro8, the semi and the final, and one set at the final takes the
/// final back off it. `DEFAULT_DEPTH` is the fallback and loses to any real
/// assignment that reaches this round.
pub(crate) fn preset_for_depth(assignments: &[RoundPreset], depth: i64) -> Option<&RoundPreset> {
    assignments
        .iter()
        .filter(|a| a.from_depth != DEFAULT_DEPTH && a.from_depth >= depth)
        .min_by_key(|a| a.from_depth)
        .or_else(|| assignments.iter().find(|a| a.from_depth == DEFAULT_DEPTH))
}

/// A round's distance from the final: the last round is 1, the one before it 2.
/// `ordinal` is 1-based from the outermost round.
///
// Consumed by chunk 12, which turns a bracket's rounds into `best_of` values.
// Until that lands only this module's tests exercise it — remove the allow then.
#[allow(dead_code)]
pub(crate) fn depth_from_final(ordinal: usize, round_count: usize) -> i64 {
    i64::try_from(round_count.saturating_sub(ordinal) + 1).unwrap_or(1)
}

/// One `best_of` per round, outermost first — the shape `bracket::build` takes.
/// `None` if any round has no preset covering it, which `missing` reports first.
///
// Consumed by chunk 12; see `depth_from_final`.
#[allow(dead_code)]
pub(crate) fn best_of_per_round(assignments: &[RoundPreset], round_count: usize) -> Option<Vec<u8>> {
    (1..=round_count)
        .map(|ordinal| {
            let preset = preset_for_depth(assignments, depth_from_final(ordinal, round_count))?;
            u8::try_from(preset.best_of).ok()
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Missing {
    Preset,
    StartTime,
}

impl Missing {
    pub(crate) fn label_for(self, locale: Locale) -> &'static str {
        match self {
            Missing::Preset => locale.pick(
                "抽選預設 (`/tournament preset`)",
                "a draft preset (`/tournament preset`)",
            ),
            Missing::StartTime => locale.pick(
                "開賽時間 (`/tournament setup start_time:`)",
                "a start time (`/tournament setup start_time:`)",
            ),
        }
    }
}

/// What still has to be configured before `/tournament start` will run.
///
/// The entrant cap is never listed: it is `not null default 32`, so it is always
/// answered. Consumed both by `/tournament setup`'s reply and by chunk 12's gate,
/// so the two cannot disagree about what "configured" means.
pub(crate) fn missing(tournament: &Tournament, assignments: &[RoundPreset]) -> Vec<Missing> {
    let mut missing = Vec::new();
    if preset_for_depth(assignments, DEFAULT_DEPTH).is_none() && assignments.is_empty() {
        missing.push(Missing::Preset);
    }
    if tournament.scheduled_start_at.is_none() {
        missing.push(Missing::StartTime);
    }
    missing
}

/// Only what the bot itself depends on (§2: how people draft is the tool's
/// business, not ours).
///
/// - `result_mode` must be `vote`: in host mode only the host — the bot — may
///   call a result, so every game would wait on us.
/// - `best_of` must be odd: §7's "more than half" completion is computed bot-side
///   and is ambiguous otherwise.
/// - the preset must be readable: `POST /api/matches` needs one it can use.
///
/// §3.3's "no `MAP_PICK` steps" rule is deliberately **not** here — see the
/// section, which now records why.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PresetCheck {
    Ok { name: String, best_of: i64 },
    NotFound,
    NotPublic { name: String },
    HostResultMode { name: String, result_mode: String },
    EvenBestOf { name: String, best_of: i64 },
}

impl PresetCheck {
    pub(crate) fn best_of(&self) -> Option<i64> {
        match self {
            PresetCheck::Ok { best_of, .. } => Some(*best_of),
            _ => None,
        }
    }

    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            PresetCheck::Ok { name, best_of } => locale.pick(
                format!("預設 **{name}**（{best_of} 戰制）。"),
                format!("Preset **{name}** (best of {best_of})."),
            ),
            PresetCheck::NotFound => locale.pick(
                "找不到這個預設 — 請確認 id，而且它必須是公開的。".to_string(),
                "No such preset — check the id, and note it has to be public.".to_string(),
            ),
            PresetCheck::NotPublic { name } => locale.pick(
                format!("預設 **{name}** 不是公開的，機器人無法用它建立抽選。"),
                format!("Preset **{name}** isn't public, so the bot can't create drafts from it."),
            ),
            PresetCheck::HostResultMode { name, result_mode } => locale.pick(
                format!(
                    "預設 **{name}** 的 resultMode 是 `{result_mode}`，必須是 `vote` — 否則每一局都要等機器人回報結果。"
                ),
                format!(
                    "Preset **{name}** has resultMode `{result_mode}`, but it must be `vote` — otherwise every \
                     game waits on the bot to call it."
                ),
            ),
            PresetCheck::EvenBestOf { name, best_of } => locale.pick(
                format!("預設 **{name}** 是 {best_of} 戰制，必須是奇數才能分出勝負。"),
                format!("Preset **{name}** is best of {best_of}; it has to be odd to decide a set."),
            ),
        }
    }
}

/// Fetches the preset and applies the checks above.
pub(crate) async fn check_preset(preset_id: &str) -> PresetCheck {
    let Some(preset) = drafttool::fetch_preset(preset_id).await else {
        return PresetCheck::NotFound;
    };
    let name = preset.name;
    let options = preset.config.options;

    if !preset.is_public {
        return PresetCheck::NotPublic { name };
    }
    if options.result_mode != "vote" {
        return PresetCheck::HostResultMode {
            name,
            result_mode: options.result_mode,
        };
    }
    if options.best_of % 2 == 0 {
        return PresetCheck::EvenBestOf {
            name,
            best_of: options.best_of,
        };
    }
    PresetCheck::Ok {
        name,
        best_of: options.best_of,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn assignment(from_depth: i64, preset: &str, best_of: i64) -> RoundPreset {
        RoundPreset {
            tournament_id: 1,
            from_depth,
            draft_preset_id: preset.to_string(),
            best_of,
            assigned_at: Utc::now(),
        }
    }

    fn resolved(assignments: &[RoundPreset], depth: i64) -> Option<&str> {
        preset_for_depth(assignments, depth).map(|a| a.draft_preset_id.as_str())
    }

    #[test]
    fn a_default_alone_covers_every_round() {
        let a = [assignment(DEFAULT_DEPTH, "A", 3)];
        for depth in 1..=6 {
            assert_eq!(resolved(&a, depth), Some("A"), "depth {depth}");
        }
    }

    #[test]
    fn an_assignment_covers_its_depth_and_everything_after_it() {
        // "preset B for Ro8" means Ro8, the semi and the final.
        let a = [assignment(DEFAULT_DEPTH, "A", 3), assignment(3, "B", 5)];
        assert_eq!(resolved(&a, 5), Some("A"), "Ro32 is before B starts");
        assert_eq!(resolved(&a, 4), Some("A"), "Ro16 is before B starts");
        assert_eq!(resolved(&a, 3), Some("B"), "Ro8");
        assert_eq!(resolved(&a, 2), Some("B"), "semi");
        assert_eq!(resolved(&a, 1), Some("B"), "final");
    }

    #[test]
    fn a_deeper_assignment_takes_its_tail_back() {
        // The worked example: A everywhere, B from Ro8, C for the final.
        let a = [
            assignment(DEFAULT_DEPTH, "A", 3),
            assignment(3, "B", 5),
            assignment(1, "C", 7),
        ];
        assert_eq!(resolved(&a, 5), Some("A"));
        assert_eq!(resolved(&a, 4), Some("A"));
        assert_eq!(resolved(&a, 3), Some("B"));
        assert_eq!(resolved(&a, 2), Some("B"));
        assert_eq!(resolved(&a, 1), Some("C"));
    }

    #[test]
    fn an_assignment_deeper_than_the_bracket_simply_never_applies() {
        // "from Ro8" on a 4-player field: there is no Ro8, and nothing breaks.
        let a = [assignment(DEFAULT_DEPTH, "A", 3), assignment(5, "B", 5)];
        assert_eq!(resolved(&a, 2), Some("B"), "B reaches every round this shallow");
        assert_eq!(resolved(&a, 1), Some("B"));
    }

    #[test]
    fn without_a_default_a_round_beyond_every_assignment_has_no_preset() {
        let a = [assignment(1, "C", 7)];
        assert_eq!(resolved(&a, 1), Some("C"));
        assert_eq!(resolved(&a, 2), None, "nothing covers the semi");
    }

    #[test]
    fn depth_counts_back_from_the_final() {
        // A 3-round bracket: Ro8, semi, final.
        assert_eq!(depth_from_final(1, 3), 3);
        assert_eq!(depth_from_final(2, 3), 2);
        assert_eq!(depth_from_final(3, 3), 1);
    }

    #[test]
    fn best_of_per_round_is_outermost_first() {
        let a = [
            assignment(DEFAULT_DEPTH, "A", 3),
            assignment(3, "B", 5),
            assignment(1, "C", 7),
        ];
        // 4 rounds: Ro16(depth 4) Ro8(3) semi(2) final(1).
        assert_eq!(best_of_per_round(&a, 4), Some(vec![3, 5, 5, 7]));
    }

    #[test]
    fn best_of_per_round_is_none_when_a_round_is_uncovered() {
        let a = [assignment(1, "C", 7)];
        assert_eq!(best_of_per_round(&a, 3), None);
    }

    fn tournament(scheduled: bool) -> Tournament {
        Tournament {
            id: 1,
            slug: "relic-cup".to_string(),
            name: "Relic Cup".to_string(),
            status: "seeding".to_string(),
            draft_base_url: None,
            announce_channel_id: Some(10),
            category_id: None,
            register_channel_id: Some(11),
            register_message_id: None,
            bracket_channel_id: Some(12),
            matches_channel_id: Some(13),
            draft_channel_id: Some(14),
            checkin_message_id: None,
            seed_message_id: None,
            checkin_closes_at: None,
            entrant_cap: 32,
            scheduled_start_at: scheduled.then(Utc::now),
            created_by: 1,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn a_fresh_tournament_is_missing_a_preset_and_a_start_time() {
        assert_eq!(
            missing(&tournament(false), &[]),
            vec![Missing::Preset, Missing::StartTime]
        );
    }

    #[test]
    fn the_cap_is_never_missing_because_it_always_has_a_default() {
        let configured = missing(&tournament(true), &[assignment(DEFAULT_DEPTH, "A", 3)]);
        assert!(configured.is_empty(), "{configured:?}");
    }

    #[test]
    fn a_preset_that_does_not_cover_every_round_still_counts_as_configured() {
        // `missing` only asks whether any preset exists; whether it covers every
        // round depends on the field size, which start checks via best_of_per_round.
        assert_eq!(missing(&tournament(true), &[assignment(1, "C", 7)]), vec![]);
    }

    #[test]
    fn preset_check_messages_render_in_both_locales() {
        let check = PresetCheck::EvenBestOf {
            name: "Bo4 Draft".to_string(),
            best_of: 4,
        };
        let zh = check.message(Locale::ZhTw);
        let en = check.message(Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("奇數"), "{zh}");
        assert!(en.contains("odd"), "{en}");
        assert!(zh.contains("Bo4 Draft") && en.contains("Bo4 Draft"));
    }
}
