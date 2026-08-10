//! Ratings and suggested seeding.
//!
//! The ordering functions are pure and are the whole point of this module's
//! tests; `refresh_ratings` is the one path that touches aoe4world, following
//! `registration.rs`'s precedent of being logic-first with a single HTTP call
//! rather than a client of its own.

use crate::aoe4world;
use crate::locale::Locale;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use sqlx::SqlitePool;

/// The field a seeding applies to: checked-in entrants only. Closing check-in
/// has already marked everyone else `no_show`, so a no-show or a withdrawal
/// never occupies a seed.
///
/// `eliminated` counts, because losing a set does not unmake a seed. The seeding
/// is a record of the field that started, and dropping people from it as the
/// bracket runs would leave a refreshed panel listing only the survivors — and
/// eventually only the champion.
pub(crate) fn seedable(entries: &[TournamentEntry]) -> Vec<&TournamentEntry> {
    entries
        .iter()
        .filter(|e| matches!(e.status.as_str(), "active" | "eliminated"))
        .collect()
}

/// The profiles in a field there is anything to look up for.
///
/// Pure, so the property that matters — an entrant with no profile costs no
/// request — is checked here rather than inferred from the loop below.
pub(crate) fn rated_ids(field: &[&TournamentEntry]) -> Vec<i64> {
    field.iter().filter_map(|e| e.aoe4_id).collect()
}

/// The default order, as user ids in seed order.
///
/// ATR (~1000–2292, tournament-derived) and ELO are different scales and are
/// **never blended into one sort key**: everyone with an ATR outranks everyone
/// without, regardless of how the raw numbers compare. Ties break on
/// `display_name` so the order is deterministic — the tests depend on it, and so
/// does not reshuffling the field on an unrelated refresh.
pub(crate) fn suggested_order(entries: &[TournamentEntry]) -> Vec<i64> {
    let mut field = seedable(entries);
    field.sort_by(|a, b| {
        let tier = |e: &TournamentEntry| u8::from(e.atr.is_none());
        tier(a)
            .cmp(&tier(b))
            .then_with(|| match (a.atr, b.atr) {
                // Rated entrants: by ATR. Unrated: by ELO. Never across the two.
                (Some(x), Some(y)) => y.total_cmp(&x),
                _ => b.elo.cmp(&a.elo),
            })
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    field.iter().map(|e| e.user_id).collect()
}

/// Whether the seeding panel belongs in `#{slug}-bracket` right now: it appears
/// when `close-checkin` computes the first seeding, and stays afterwards.
///
/// Asked of the phase rather than of `seed_message_id` for the same reason as
/// `checkin::checkin_panel_expected` — a panel whose first post failed has no
/// id, and that is exactly the one a repair must be able to put back.
pub(crate) fn seed_panel_expected(status: &str) -> bool {
    matches!(status, "seeding" | "running" | "completed")
}

/// How the field is shown, wherever it is shown: seeded entrants first in seed
/// order, everyone else after them by the default tiering.
///
/// Defined once because the seeding panel and the bracket drawing must agree —
/// they render the same entrants, and a reader comparing them should not find
/// two different orders. A field nobody has seeded is therefore pure tiering,
/// and a fully seeded one pure seed order.
pub(crate) fn display_order(entries: &[TournamentEntry]) -> Vec<&TournamentEntry> {
    let tiering = suggested_order(entries);
    let rank = |user_id: i64| tiering.iter().position(|id| *id == user_id).unwrap_or(usize::MAX);

    let mut field = seedable(entries);
    field.sort_by_key(|e| (e.seed.unwrap_or(i64::MAX), rank(e.user_id)));
    field
}

/// The organizers' own order, as user ids — `display_order` without the rows.
///
/// Seeded entrants keep their relative order and anyone unseeded follows by the
/// tiering, so writing this back **compacts** the field: a gap left by a no-show
/// or a withdrawal closes, which is what keeps `start`'s contiguous 1..n
/// requirement true with no separate renumber step.
pub(crate) fn manual_order(entries: &[TournamentEntry]) -> Vec<i64> {
    display_order(entries).iter().map(|e| e.user_id).collect()
}

/// What a rating refresh does to the field's order, from
/// `tournaments.seed_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedPolicy {
    /// Re-tier the whole field. The default.
    Suggest,
    /// Update every rating, keep the organizers' order.
    KeepManual,
}

impl SeedPolicy {
    /// Total on purpose: the column's `check` makes anything else unreachable, and
    /// falling back to the default beats a panic on a value a future migration adds.
    pub(crate) fn from_source(seed_source: &str) -> Self {
        match seed_source {
            "manual" => SeedPolicy::KeepManual,
            _ => SeedPolicy::Suggest,
        }
    }

    pub(crate) fn as_source(self) -> &'static str {
        match self {
            SeedPolicy::Suggest => "suggested",
            SeedPolicy::KeepManual => "manual",
        }
    }
}

/// Moves `user_id` to `new_seed` (1-based) and shifts everyone between, returning
/// the whole new order.
///
/// Total by construction: the result is always a permutation of `order`, so
/// writing it back always leaves seeds 1..n contiguous. An out-of-range seed is
/// clamped rather than rejected — the command validates and reports separately,
/// and this function having no failure mode is what keeps the field startable.
pub(crate) fn reorder(order: &[i64], user_id: i64, new_seed: i64) -> Vec<i64> {
    let mut reordered: Vec<i64> = order.iter().copied().filter(|id| *id != user_id).collect();
    if reordered.len() == order.len() {
        return order.to_vec(); // not in the field; nothing to move
    }
    let index = usize::try_from(new_seed.max(1) - 1).unwrap_or(0).min(reordered.len());
    reordered.insert(index, user_id);
    reordered
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RefreshOutcome {
    /// `atr_count` is how many of `total` the esports leaderboard knew about —
    /// usually a small minority, which is expected rather than a failure.
    Refreshed {
        total: usize,
        atr_count: usize,
    },
    /// Ratings updated, the organizers' order left alone.
    KeptManual {
        total: usize,
        atr_count: usize,
    },
    NoField,
}

impl RefreshOutcome {
    pub(crate) fn message(&self, tournament_name: &str, locale: Locale) -> String {
        match self {
            RefreshOutcome::Refreshed { total, atr_count } => locale.pick(
                format!(
                    "已更新 **{tournament_name}** 的評分並重新排種子：{total} 位參賽者，其中 {atr_count} 位有 ATR。"
                ),
                format!(
                    "Refreshed ratings and reseeded **{tournament_name}**: {total} entrants, \
                     {atr_count} with an ATR."
                ),
            ),
            RefreshOutcome::KeptManual { total, atr_count } => locale.pick(
                format!(
                    "已更新 **{tournament_name}** 的評分：{total} 位參賽者，其中 {atr_count} 位有 ATR。\
                     種子順序是手動排定的，因此保持不變；若要改回建議順序，請使用 `/tournament seed refresh`。"
                ),
                format!(
                    "Refreshed ratings for **{tournament_name}**: {total} entrants, \
                     {atr_count} with an ATR. The seed order was set by hand, so it was kept — \
                     use `/tournament seed refresh` to take the suggestion back."
                ),
            ),
            RefreshOutcome::NoField => locale.pick(
                format!("**{tournament_name}** 沒有已簽到的參賽者可以排種子。"),
                format!("**{tournament_name}** has no checked-in entrants to seed."),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SeedOutcome {
    Moved { display_name: String, from: i64, to: i64 },
    NotInField,
    OutOfRange { field_size: i64 },
}

impl SeedOutcome {
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            SeedOutcome::Moved { display_name, from, to } => locale.pick(
                format!("已將 **{display_name}** 從第 {from} 種子改到第 {to} 種子，其他人依序遞移。"),
                format!("Moved **{display_name}** from seed {from} to seed {to}; everyone between shifts along."),
            ),
            SeedOutcome::NotInField => locale.pick(
                "那位玩家不在已簽到的參賽名單中。".to_string(),
                "That player isn't in the checked-in field.".to_string(),
            ),
            SeedOutcome::OutOfRange { field_size } => locale.pick(
                format!("種子序號必須介於 1 到 {field_size} 之間。"),
                format!("Seed must be between 1 and {field_size}."),
            ),
        }
    }
}

/// Re-fetches both ratings for the checked-in field and rewrites the seed order.
/// Ratings are snapshotted onto the entries at seeding time; there is no cache.
///
/// ELO is one request per entrant — there is no bulk profile endpoint — while
/// ATR for the whole field is one batched call. Both are tolerant: `ranked.rs`
/// drops unrated players with `?`, and seeding must not — an unrated entrant
/// still has to take a seat, which is why every rating column is nullable.
///
/// **Ratings are always refreshed; only the ordering branches on `policy`** — an
/// organizer who arranged the field by hand still wants current numbers on the
/// panel, just not a re-tiering underneath them.
pub(crate) async fn refresh_ratings(
    pool: &SqlitePool,
    tournament: &Tournament,
    policy: SeedPolicy,
) -> Result<RefreshOutcome, sqlx::Error> {
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let field = seedable(&entries);
    if field.is_empty() {
        return Ok(RefreshOutcome::NoField);
    }

    let atr_by_id = aoe4world::fetch_esports_ratings(&rated_ids(&field)).await;

    let mut atr_count = 0;
    for entry in &field {
        // Skipped outright rather than written as nulls: an entrant with no profile
        // has nothing to look up, and blanking the row would discard an `atr` an
        // organizer had set by hand.
        let Some(aoe4_id) = entry.aoe4_id else {
            continue;
        };
        let elo = aoe4world::fetch_profile(aoe4_id)
            .await
            .and_then(|p| p.modes.rm_1v1_elo.map(|e| i64::from(e.rating)));
        let atr = atr_by_id.get(&aoe4_id).copied();
        if atr.is_some() {
            atr_count += 1;
        }
        db::set_entry_ratings(pool, tournament.id, entry.user_id, elo, atr, atr.map(|_| "esports")).await?;
    }

    // Re-read: the rows above are what the ordering must sort on.
    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let total = field.len();
    match policy {
        SeedPolicy::Suggest => {
            db::set_seed_order(pool, tournament.id, &suggested_order(&entries), true).await?;
            Ok(RefreshOutcome::Refreshed { total, atr_count })
        },
        // `also_suggested: false` — the order being kept is the organizers', so
        // recording it as what the tiering proposed would erase the comparison
        // the panel shows.
        SeedPolicy::KeepManual => {
            db::set_seed_order(pool, tournament.id, &manual_order(&entries), false).await?;
            Ok(RefreshOutcome::KeptManual { total, atr_count })
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(user_id: i64, display_name: &str, atr: Option<f64>, elo: Option<i64>) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: Some(user_id * 100),
            invited_by: None,
            seed: None,
            suggested_seed: None,
            display_name: display_name.to_string(),
            elo,
            atr,
            atr_source: atr.map(|_| "esports".to_string()),
            status: "active".to_string(),
            registered_at: Utc::now(),
            checked_in_at: Some(Utc::now()),
        }
    }

    #[test]
    fn only_entrants_with_a_profile_cost_a_lookup() {
        // An entrant with none has nothing to fetch, so they must not reach the
        // batched request at all — order preserved for everyone else.
        let mut unbound = entry(2, "Invitee", None, None);
        unbound.aoe4_id = None;
        let entries = vec![
            entry(1, "Bound", None, Some(1000)),
            unbound,
            entry(3, "Also", None, Some(900)),
        ];
        let field = seedable(&entries);
        assert_eq!(rated_ids(&field), vec![100, 300]);
    }

    #[test]
    fn a_field_of_entrants_without_profiles_asks_for_nothing() {
        let mut one = entry(1, "A", None, None);
        one.aoe4_id = None;
        let mut two = entry(2, "B", None, None);
        two.aoe4_id = None;
        let entries = vec![one, two];
        assert!(rated_ids(&seedable(&entries)).is_empty());
    }

    #[test]
    fn the_seeding_panel_is_expected_from_seeding_onward() {
        for status in ["seeding", "running", "completed"] {
            assert!(seed_panel_expected(status), "{status} should still show the panel");
        }
        for status in ["registration", "checkin", "canceled"] {
            assert!(!seed_panel_expected(status), "{status} should not have one");
        }
    }

    #[test]
    fn atr_rated_entrants_outrank_everyone_else_regardless_of_the_numbers() {
        // The trap: 1500 ELO is a bigger number than 1100 ATR, but
        // the two scales must never be compared, so the ATR player still leads.
        let entries = vec![
            entry(1, "EloOnly", None, Some(1500)),
            entry(2, "LowAtr", Some(1100.0), None),
        ];
        assert_eq!(suggested_order(&entries), vec![2, 1]);
    }

    #[test]
    fn rated_by_atr_descending_then_unrated_by_elo_descending() {
        let entries = vec![
            entry(1, "MidElo", None, Some(1200)),
            entry(2, "TopAtr", Some(2292.5), None),
            entry(3, "HighElo", None, Some(1400)),
            entry(4, "MidAtr", Some(1500.0), None),
        ];
        assert_eq!(suggested_order(&entries), vec![2, 4, 3, 1]);
    }

    #[test]
    fn entrants_with_neither_rating_come_last_ordered_by_name() {
        let entries = vec![
            entry(1, "Zoe", None, None),
            entry(2, "Adam", None, None),
            entry(3, "Rated", None, Some(900)),
        ];
        assert_eq!(suggested_order(&entries), vec![3, 2, 1]);
    }

    #[test]
    fn ties_break_by_display_name_so_the_order_is_deterministic() {
        let entries = vec![
            entry(1, "Bravo", Some(1500.0), None),
            entry(2, "Alpha", Some(1500.0), None),
        ];
        assert_eq!(suggested_order(&entries), vec![2, 1]);
    }

    #[test]
    fn no_shows_and_withdrawals_never_occupy_a_seed() {
        let mut no_show = entry(2, "NoShow", Some(2000.0), None);
        no_show.status = "no_show".to_string();
        let mut withdrawn = entry(3, "Withdrawn", Some(2100.0), None);
        withdrawn.status = "withdrawn".to_string();
        let entries = vec![entry(1, "Active", None, Some(1000)), no_show, withdrawn];
        assert_eq!(suggested_order(&entries), vec![1]);
    }

    fn seeded(user_id: i64, display_name: &str, seed: i64, atr: Option<f64>, elo: Option<i64>) -> TournamentEntry {
        TournamentEntry {
            seed: Some(seed),
            suggested_seed: Some(seed),
            ..entry(user_id, display_name, atr, elo)
        }
    }

    #[test]
    fn the_policy_comes_from_the_column_and_is_total() {
        assert_eq!(SeedPolicy::from_source("manual"), SeedPolicy::KeepManual);
        assert_eq!(SeedPolicy::from_source("suggested"), SeedPolicy::Suggest);
        // The `check` constraint makes these unreachable; defaulting beats panicking.
        for unknown in ["", "MANUAL", "invited"] {
            assert_eq!(SeedPolicy::from_source(unknown), SeedPolicy::Suggest, "{unknown}");
        }
    }

    #[test]
    fn the_policy_round_trips_through_the_column_vocabulary() {
        for policy in [SeedPolicy::Suggest, SeedPolicy::KeepManual] {
            assert_eq!(SeedPolicy::from_source(policy.as_source()), policy);
        }
    }

    #[test]
    fn keeping_a_manual_order_preserves_it_while_closing_a_gap() {
        // Seed 3 no-showed, so the field is 1, 2, 4. Writing this order back is
        // what renumbers it 1..3, so the field stays startable with no separate step.
        let mut no_show = seeded(3, "NoShow", 3, Some(2200.0), None);
        no_show.status = "no_show".to_string();
        let entries = vec![
            seeded(4, "Fourth", 4, None, Some(900)),
            seeded(1, "First", 1, None, Some(1000)),
            no_show,
            seeded(2, "Second", 2, None, Some(950)),
        ];
        assert_eq!(manual_order(&entries), vec![1, 2, 4]);
    }

    #[test]
    fn a_manual_order_wins_over_ratings_that_say_the_exact_opposite() {
        // The organizers seeded the weakest player first and the strongest last,
        // so the ratings disagree with every single position. Anything less than
        // a total reversal would let a partly-rating-driven sort pass by luck.
        let entries = vec![
            seeded(1, "Weakest", 1, None, Some(900)),
            seeded(2, "Middle", 2, None, Some(1200)),
            seeded(3, "Strongest", 3, None, Some(1500)),
        ];

        assert_eq!(manual_order(&entries), vec![1, 2, 3], "the seeds decide, not the ELO");
        // And the suggestion the override is overriding really is the reverse —
        // otherwise the assertion above proves nothing.
        assert_eq!(suggested_order(&entries), vec![3, 2, 1]);
    }

    #[test]
    fn a_later_registrant_lands_at_the_end_of_a_manual_order_rather_than_inside_it() {
        // The price of keeping an order: the newcomer is not merged in by rating,
        // however strong they are, until someone refreshes.
        let entries = vec![
            seeded(1, "First", 1, None, Some(900)),
            seeded(2, "Second", 2, None, Some(950)),
            entry(3, "Latecomer", Some(2292.0), None),
        ];
        assert_eq!(manual_order(&entries), vec![1, 2, 3]);
    }

    #[test]
    fn suggesting_still_discards_an_override() {
        // The same field the two tests above keep: under `Suggest` the seeds
        // count for nothing and the tiering decides.
        let entries = vec![
            seeded(1, "First", 1, None, Some(900)),
            seeded(2, "Second", 2, None, Some(950)),
            entry(3, "Latecomer", Some(2292.0), None),
        ];
        assert_eq!(suggested_order(&entries), vec![3, 2, 1]);
    }

    #[test]
    fn reorder_moves_a_seed_up_and_shifts_the_rest_down() {
        assert_eq!(reorder(&[10, 20, 30, 40], 40, 2), vec![10, 40, 20, 30]);
    }

    #[test]
    fn reorder_moves_a_seed_down() {
        assert_eq!(reorder(&[10, 20, 30, 40], 10, 3), vec![20, 30, 10, 40]);
    }

    #[test]
    fn reorder_to_the_current_seed_is_a_no_op() {
        let order = [10, 20, 30];
        assert_eq!(reorder(&order, 20, 2), order.to_vec());
    }

    #[test]
    fn reorder_clamps_out_of_range_seeds_rather_than_dropping_anyone() {
        assert_eq!(reorder(&[10, 20, 30], 30, 0), vec![30, 10, 20]);
        assert_eq!(reorder(&[10, 20, 30], 10, 99), vec![20, 30, 10]);
    }

    #[test]
    fn reorder_always_returns_a_permutation_so_the_field_stays_contiguous() {
        let order = [10, 20, 30, 40, 50];
        for target in 1..=5 {
            for moved in order {
                let result = reorder(&order, moved, target);
                assert_eq!(result.len(), order.len());
                let mut sorted = result.clone();
                sorted.sort();
                assert_eq!(sorted, vec![10, 20, 30, 40, 50], "moving {moved} to {target}");
            }
        }
    }

    #[test]
    fn reorder_leaves_the_order_alone_for_someone_not_in_the_field() {
        let order = [10, 20, 30];
        assert_eq!(reorder(&order, 99, 1), order.to_vec());
    }

    #[test]
    fn seed_messages_render_in_both_locales() {
        let outcome = SeedOutcome::Moved {
            display_name: "MarineLorD".to_string(),
            from: 3,
            to: 1,
        };
        let zh = outcome.message(Locale::ZhTw);
        let en = outcome.message(Locale::En);
        assert_ne!(zh, en);
        // Both seeds are data and must survive either rendering.
        assert!(zh.contains('3') && zh.contains('1'), "{zh}");
        assert!(en.contains("seed 3") && en.contains("seed 1"), "{en}");
    }

    #[test]
    fn messages_render_in_both_locales() {
        let outcome = RefreshOutcome::Refreshed { total: 8, atr_count: 2 };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains("ATR"), "{zh}");
        assert!(en.contains("with an ATR"), "{en}");
        assert!(zh.contains('8') && en.contains('8'));
    }

    #[test]
    fn a_kept_order_says_so_and_names_the_way_back_in_both_locales() {
        let outcome = RefreshOutcome::KeptManual { total: 8, atr_count: 2 };
        let zh = outcome.message("Relic Cup", Locale::ZhTw);
        let en = outcome.message("Relic Cup", Locale::En);
        assert_ne!(zh, en);
        // The reply has to name the command that takes the suggestion back, or a
        // kept order looks like a refresh that did nothing.
        assert!(zh.contains("/tournament seed refresh"), "{zh}");
        assert!(en.contains("/tournament seed refresh"), "{en}");
        assert!(zh.contains('8') && en.contains('8'));
        // Not to be confused with the reseeding message.
        assert_ne!(
            zh,
            RefreshOutcome::Refreshed { total: 8, atr_count: 2 }.message("Relic Cup", Locale::ZhTw)
        );
    }
}
