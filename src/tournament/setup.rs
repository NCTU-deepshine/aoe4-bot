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
use chrono::{DateTime, Duration, Utc};

/// Check-in opens this long before the scheduled start (§8.3).
pub(crate) const CHECKIN_LEAD: Duration = Duration::hours(1);

/// How far out a new tournament's start time is placed. Deliberately far: it is
/// a tripwire, not a convenience. An organizer running an event tomorrow cannot
/// open check-in until they correct it, which is the point.
pub(crate) const DEFAULT_START_LEAD: Duration = Duration::days(7);

pub(crate) fn checkin_opens_at(scheduled_start_at: DateTime<Utc>) -> DateTime<Utc> {
    scheduled_start_at - CHECKIN_LEAD
}

/// Whether the start time is still the untouched placeholder `insert_tournament`
/// wrote. Both timestamps come from one statement's clock, so this is an exact
/// comparison rather than a tolerance.
pub(crate) fn start_time_is_default(tournament: &Tournament) -> bool {
    tournament.scheduled_start_at == Some(tournament.created_at + DEFAULT_START_LEAD)
}

/// Whether the event may begin.
pub(crate) fn may_start_at(scheduled_start_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    scheduled_start_at.is_none_or(|at| now >= at)
}

/// The depth that means "every round", as opposed to a real distance from the
/// final. Rounds are numbered 1 = final, 2 = semi, 3 = Ro8, so 0 is free.
pub(crate) const DEFAULT_DEPTH: i64 = 0;

impl RoundPreset {
    /// How deep into the bracket this assignment reaches. The default covers every
    /// round, so it reaches past any real depth.
    fn reach(&self) -> i64 {
        if self.from_depth == DEFAULT_DEPTH {
            i64::MAX
        } else {
            self.from_depth
        }
    }
}

/// Resolves which preset a round uses, given how far it is from the final.
///
/// An assignment covers its own depth **and everything after it**, so the winner is
/// the one that reaches least far while still reaching this round — a preset set at
/// Ro8 claims Ro8, the semi and the final, and one set at the final takes the final
/// back off it. The default reaches furthest, so any real assignment that reaches
/// this round beats it.
pub(crate) fn preset_for_depth(assignments: &[RoundPreset], depth: i64) -> Option<&RoundPreset> {
    assignments
        .iter()
        .filter(|a| a.reach() >= depth)
        .min_by_key(|a| a.reach())
}

/// Translates between the two ways a round is numbered: its **ordinal**, 1-based
/// from the outermost round, which is how rounds are stored and iterated; and its
/// **depth**, 1 = final, which is how presets are configured, because rounds do not
/// exist until `start` and how many there are depends on the field size (§3.3).
///
/// `round_count - x + 1` is its own inverse, so this converts either way and there
/// is no opposite helper to keep straight.
pub(crate) fn depth_from_final(ordinal: usize, round_count: usize) -> i64 {
    i64::try_from(round_count.saturating_sub(ordinal) + 1).unwrap_or(1)
}

/// The preset each round runs, outermost first. `None` if any round has no preset
/// covering it, which is what stops a half-configured field from starting.
///
/// The **only** place an ordinal becomes a depth, so it is the only place the two
/// numbering schemes meet. Callers take what they need off each preset — `best_of`
/// for `bracket::build`, the id for `insert_bracket` — which is what keeps the two
/// from ever disagreeing about which preset a round runs.
pub(crate) fn presets_per_round(assignments: &[RoundPreset], round_count: usize) -> Option<Vec<&RoundPreset>> {
    (1..=round_count)
        .map(|ordinal| preset_for_depth(assignments, depth_from_final(ordinal, round_count)))
        .collect()
}

/// Which depths a newly assigned preset is written to, default first.
///
/// A scoped assignment made while no default exists becomes the default as well, so
/// **the first preset an organizer sets always covers the whole bracket**. Without
/// it, "Final only" as an opening move leaves every earlier round with no preset,
/// and `start` refuses with `NotConfigured` while `/tournament setup` — which
/// cannot know the field size — reports nothing missing.
///
/// Default first so that a failure between the two writes leaves the bracket
/// covered rather than scoped to one round.
pub(crate) fn depths_to_assign(existing: &[RoundPreset], depth: i64) -> Vec<i64> {
    let has_default = existing.iter().any(|a| a.from_depth == DEFAULT_DEPTH);
    if depth == DEFAULT_DEPTH || has_default {
        vec![depth]
    } else {
        vec![DEFAULT_DEPTH, depth]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Missing {
    Preset,
}

impl Missing {
    pub(crate) fn label_for(self, locale: Locale) -> &'static str {
        match self {
            Missing::Preset => locale.pick(
                "抽選預設 (`/tournament preset`)",
                "a draft preset (`/tournament preset`)",
            ),
        }
    }
}

/// What still has to be configured before `/tournament start` will run.
///
/// The entrant cap is never listed: it is `not null default 32`, so it is always
/// answered. Consumed both by `/tournament setup`'s reply and by chunk 12's gate,
/// so the two cannot disagree about what "configured" means.
///
/// Only asks whether *any* preset exists. Whether one covers every round depends on
/// the field size, which is not known here — `presets_per_round` decides it.
pub(crate) fn missing(assignments: &[RoundPreset]) -> Vec<Missing> {
    let mut missing = Vec::new();
    if assignments.is_empty() {
        missing.push(Missing::Preset);
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

    /// The preset's name on the tool, stored so the setup panel can link it by name
    /// rather than by id.
    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            PresetCheck::Ok { name, .. } => Some(name),
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
            preset_name: None,
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
    fn converting_between_ordinal_and_depth_is_its_own_inverse() {
        // Which is why there is one helper and not a pair pointing opposite ways:
        // applying it to a depth hands back the ordinal it came from.
        for round_count in 1..=5 {
            for ordinal in 1..=round_count {
                let depth = depth_from_final(ordinal, round_count);
                let back = depth_from_final(usize::try_from(depth).unwrap(), round_count);
                assert_eq!(back, i64::try_from(ordinal).unwrap(), "R{ordinal} of {round_count}");
            }
        }
    }

    /// The `best_of` each round runs, outermost first — what `start` derives from
    /// `presets_per_round` to build the bracket.
    fn best_of_per_round(assignments: &[RoundPreset], round_count: usize) -> Option<Vec<i64>> {
        Some(
            presets_per_round(assignments, round_count)?
                .iter()
                .map(|preset| preset.best_of)
                .collect(),
        )
    }

    #[test]
    fn presets_resolve_per_round_outermost_first() {
        let a = [
            assignment(DEFAULT_DEPTH, "A", 3),
            assignment(3, "B", 5),
            assignment(1, "C", 7),
        ];
        // 4 rounds: Ro16(depth 4) Ro8(3) semi(2) final(1).
        let per_round = presets_per_round(&a, 4).expect("every round is covered");
        let ids: Vec<&str> = per_round.iter().map(|p| p.draft_preset_id.as_str()).collect();
        assert_eq!(ids, vec!["A", "B", "B", "C"]);
        // The series lengths come off the same resolution, so they cannot disagree.
        assert_eq!(best_of_per_round(&a, 4), Some(vec![3, 5, 5, 7]));
    }

    #[test]
    fn presets_per_round_is_none_when_a_round_is_uncovered() {
        let a = [assignment(1, "C", 7)];
        assert!(presets_per_round(&a, 3).is_none());
    }

    #[test]
    fn the_first_preset_becomes_the_default_whatever_scope_it_was_given() {
        // "Final only" as an opening move would otherwise leave every earlier round
        // with no preset at all.
        assert_eq!(depths_to_assign(&[], 1), vec![DEFAULT_DEPTH, 1]);
        assert_eq!(depths_to_assign(&[], DEFAULT_DEPTH), vec![DEFAULT_DEPTH]);
    }

    #[test]
    fn a_later_scoped_preset_leaves_an_existing_default_alone() {
        let a = [assignment(DEFAULT_DEPTH, "A", 3)];
        assert_eq!(depths_to_assign(&a, 3), vec![3], "A stays the default");
        assert_eq!(
            depths_to_assign(&a, DEFAULT_DEPTH),
            vec![DEFAULT_DEPTH],
            "replaced in place"
        );
    }

    #[test]
    fn a_scoped_preset_still_defaults_when_only_other_scoped_ones_exist() {
        // Reachable for a tournament configured before the default was written
        // alongside: nothing covers the rounds before depth 1.
        let a = [assignment(1, "C", 7)];
        assert_eq!(depths_to_assign(&a, 2), vec![DEFAULT_DEPTH, 2]);
    }

    #[test]
    fn the_first_preset_covers_every_round_of_any_bracket() {
        // The point of the rule: what `depths_to_assign` writes is always startable.
        let opening = depths_to_assign(&[], 1);
        let written: Vec<RoundPreset> = opening.iter().map(|d| assignment(*d, "A", 3)).collect();
        for round_count in 1..=5 {
            assert!(
                presets_per_round(&written, round_count).is_some(),
                "{round_count} rounds should be covered"
            );
        }
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
            seed_source: "suggested".to_string(),
            created_by: 1,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn a_fresh_tournament_is_missing_only_a_preset() {
        // The cap and the start time both always have values, so neither can be
        // absent; an untouched start time is warned about, not blocked on.
        assert_eq!(missing(&[]), vec![Missing::Preset]);
    }

    #[test]
    fn checkin_opens_an_hour_before_the_start() {
        let start = Utc::now() + Duration::days(1);
        assert_eq!(checkin_opens_at(start), start - Duration::hours(1));
    }

    #[test]
    fn the_placeholder_start_time_is_recognised_and_an_edited_one_is_not() {
        let mut t = tournament(false);
        t.created_at = Utc::now();
        t.scheduled_start_at = Some(t.created_at + DEFAULT_START_LEAD);
        assert!(start_time_is_default(&t), "the untouched default should be spotted");

        t.scheduled_start_at = Some(t.created_at + Duration::days(2));
        assert!(!start_time_is_default(&t), "an edited time is not the default");
    }

    #[test]
    fn starting_waits_for_the_scheduled_time() {
        let now = Utc::now();
        assert!(!may_start_at(Some(now + Duration::minutes(1)), now));
        assert!(may_start_at(Some(now), now), "the boundary itself is allowed");
        assert!(may_start_at(Some(now - Duration::hours(1)), now));
        assert!(may_start_at(None, now), "an unscheduled tournament is not blocked");
    }

    #[test]
    fn the_cap_is_never_missing_because_it_always_has_a_default() {
        let configured = missing(&[assignment(DEFAULT_DEPTH, "A", 3)]);
        assert!(configured.is_empty(), "{configured:?}");
    }

    #[test]
    fn a_preset_that_does_not_cover_every_round_still_counts_as_configured() {
        // `missing` only asks whether any preset exists; whether it covers every
        // round depends on the field size, which start checks via presets_per_round.
        assert_eq!(missing(&[assignment(1, "C", 7)]), vec![]);
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
