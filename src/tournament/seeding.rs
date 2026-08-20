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
use std::collections::HashSet;

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

/// The profiles in a field there is anything to look up for — every one of
/// them, since an entrant with no profile is no longer a state the schema
/// can hold.
pub(crate) fn rated_ids(field: &[&TournamentEntry]) -> Vec<i64> {
    field.iter().map(|e| e.aoe4_id).collect()
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

/// The number an entrant shows as: the closed, final `seed` once one exists,
/// or its own pin before that — a pin is visible immediately, even though it
/// is not written into `seed` until close (`resolved_order` runs there, not
/// per pin, so an unreached pin isn't compacted before it's clear whether more
/// entrants arrive to fill the gap in front of it).
pub(crate) fn effective_seed(entry: &TournamentEntry) -> Option<i64> {
    entry.seed.or(entry.manual_seed)
}

/// How the field is shown, wherever it is shown: seeded (or pinned) entrants
/// first by that number, everyone else after them by the default tiering.
///
/// Defined once because the seeding panel and the bracket drawing must agree —
/// they render the same entrants, and a reader comparing them should not find
/// two different orders. A field nobody has seeded is therefore pure tiering,
/// and a fully seeded one pure seed order.
pub(crate) fn display_order(entries: &[TournamentEntry]) -> Vec<&TournamentEntry> {
    let tiering = suggested_order(entries);
    let rank = |user_id: i64| tiering.iter().position(|id| *id == user_id).unwrap_or(usize::MAX);

    let mut field = seedable(entries);
    field.sort_by_key(|e| (effective_seed(e).unwrap_or(i64::MAX), rank(e.user_id)));
    field
}

/// The field's final order: every pinned entrant on the seat it was pinned to,
/// everyone else filling what is left, in `suggested_order`.
///
/// Total, and always a permutation of the field, so writing it back leaves
/// seeds 1..n contiguous — `start`'s requirement, met by construction rather
/// than a renumber step. A pin past the end of the field lands on the last
/// seat instead, and climbs back to its own seat as the field grows into it —
/// which is also what closes the gap a no-show or a withdrawal leaves, with no
/// separate compaction pass.
pub(crate) fn resolved_order(entries: &[TournamentEntry]) -> Vec<i64> {
    let field = seedable(entries);
    let seats = field.len();

    let mut pins: Vec<(i64, i64)> = field
        .iter()
        .filter_map(|e| e.manual_seed.map(|seat| (seat, e.user_id)))
        .collect();
    pins.sort_by_key(|&(seat, _)| seat);
    let pinned_ids: HashSet<i64> = pins.iter().map(|&(_, user_id)| user_id).collect();
    let rest: Vec<i64> = suggested_order(entries)
        .into_iter()
        .filter(|id| !pinned_ids.contains(id))
        .collect();

    let mut pins = pins.into_iter().peekable();
    let mut rest = rest.into_iter().peekable();
    let mut order = Vec::with_capacity(seats);
    for seat in 1..=i64::try_from(seats).unwrap_or(i64::MAX) {
        // A pin is taken the moment the sweep reaches its own seat — or, for
        // one past the field's end, once the tiering has nothing left to fill
        // the remaining seats with.
        let due = matches!(pins.peek(), Some(&(pin_seat, _)) if pin_seat == seat) || rest.peek().is_none();
        let user_id = if due {
            pins.next().map(|(_, id)| id)
        } else {
            rest.next()
        };
        order.push(user_id.expect("pins and the tiering exhaust exactly at `seats`, by construction"));
    }
    order
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
    Pinned {
        display_name: String,
        seed: i64,
        /// Whoever held that seat before this pin took it, if anyone.
        displaced: Option<String>,
    },
    NotInField,
    OutOfRange {
        cap: i64,
    },
}

impl SeedOutcome {
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            SeedOutcome::Pinned {
                display_name,
                seed,
                displaced,
            } => {
                let displaced_clause = displaced.as_deref().map_or_else(String::new, |name| {
                    locale.pick(
                        format!("，讓 **{name}** 讓出該種子"),
                        format!(", displacing **{name}**"),
                    )
                });
                locale.pick(
                    format!(
                        "已將 **{display_name}** 釘選在第 {seed} 種子{displaced_clause}；其餘參賽者依建議順序遞補。"
                    ),
                    format!(
                        "Pinned **{display_name}** to seed {seed}{displaced_clause}; everyone else fills the rest \
                         by the suggested order."
                    ),
                )
            },
            SeedOutcome::NotInField => locale.pick(
                "那位玩家不在已簽到的參賽名單中。".to_string(),
                "That player isn't in the checked-in field.".to_string(),
            ),
            SeedOutcome::OutOfRange { cap } => locale.pick(
                format!("種子序號必須介於 1 到 {cap} 之間。"),
                format!("Seed must be between 1 and {cap}."),
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
        let elo = aoe4world::fetch_profile(entry.aoe4_id)
            .await
            .and_then(|p| p.modes.rm_1v1_elo.map(|e| i64::from(e.rating)));
        let atr = atr_by_id.get(&entry.aoe4_id).copied();
        if atr.is_some() {
            atr_count += 1;
        }
        db::set_entry_ratings(pool, tournament.id, entry.user_id, elo, atr, atr.map(|_| "esports")).await?;
    }

    let total = field.len();
    match policy {
        // "Take the suggestion back" means dropping every pin, not just
        // outrunning them: with none left, `resolved_order` is `suggested_order`.
        SeedPolicy::Suggest => {
            db::clear_manual_seeds(pool, tournament.id).await?;
            let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
            db::set_seed_order(pool, tournament.id, &resolved_order(&entries), true).await?;
            Ok(RefreshOutcome::Refreshed { total, atr_count })
        },
        // `also_suggested: false` — pinned seats are the organizers', not the
        // tiering's proposal, and recording them as such would erase the
        // comparison the panel shows for anyone left to the default order.
        SeedPolicy::KeepManual => {
            // Re-read: the ratings written above are what `resolved_order`'s own
            // tiering pass sorts the unpinned entrants by.
            let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
            db::set_seed_order(pool, tournament.id, &resolved_order(&entries), false).await?;
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
            aoe4_id: user_id * 100,
            invited_by: None,
            seed: None,
            suggested_seed: None,
            manual_seed: None,
            display_name: display_name.to_string(),
            elo,
            atr,
            atr_source: atr.map(|_| "esports".to_string()),
            status: "active".to_string(),
            registered_at: Utc::now(),
            checked_in_at: Some(Utc::now()),
        }
    }

    fn pinned(user_id: i64, display_name: &str, seat: i64, atr: Option<f64>, elo: Option<i64>) -> TournamentEntry {
        TournamentEntry {
            manual_seed: Some(seat),
            ..entry(user_id, display_name, atr, elo)
        }
    }

    #[test]
    fn every_entrant_in_the_field_is_a_profile_to_look_up() {
        // An unbound entrant is unreachable now, so the batched request
        // covers the whole field, in order.
        let entries = vec![entry(1, "Bound", None, Some(1000)), entry(2, "Also", None, Some(900))];
        let field = seedable(&entries);
        assert_eq!(rated_ids(&field), vec![100, 200]);
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
    fn a_pin_holds_its_seat_while_the_rest_tier_around_it() {
        // Pinning the weakest player to seed 1 does not touch anyone else's
        // relative order — the strongest two still rank Strongest above Second.
        let entries = vec![
            pinned(1, "Weakest", 1, None, Some(900)),
            entry(2, "Second", None, Some(1200)),
            entry(3, "Strongest", None, Some(1500)),
        ];
        assert_eq!(resolved_order(&entries), vec![1, 3, 2]);
        // And the suggestion the pin overrides really would have put Weakest
        // last — otherwise the assertion above proves nothing.
        assert_eq!(suggested_order(&entries), vec![3, 2, 1]);
    }

    #[test]
    fn a_pin_wins_over_ratings_that_say_the_exact_opposite() {
        // The organizers pinned the weakest player first and the strongest
        // last, so the ratings disagree with every single seat. Anything less
        // than a total reversal would let a partly-rating-driven sort pass by luck.
        let entries = vec![
            pinned(1, "Weakest", 1, None, Some(900)),
            pinned(2, "Middle", 2, None, Some(1200)),
            pinned(3, "Strongest", 3, None, Some(1500)),
        ];
        assert_eq!(resolved_order(&entries), vec![1, 2, 3], "the pins decide, not the ELO");
        assert_eq!(suggested_order(&entries), vec![3, 2, 1]);
    }

    #[test]
    fn a_pin_past_the_end_of_the_field_compacts_to_the_last_seat() {
        let entries = vec![
            pinned(10, "Pinned", 12, None, None),
            entry(1, "A", None, Some(998)),
            entry(2, "B", None, Some(997)),
        ];
        assert_eq!(resolved_order(&entries), vec![1, 2, 10]);
    }

    #[test]
    fn a_compacted_pin_climbs_back_to_its_own_seat_as_the_field_grows_into_it() {
        let mut entries = vec![pinned(1, "Pinned", 12, None, None)];
        for id in 2..=3 {
            entries.push(entry(id, &format!("Filler{id}"), None, Some(1000 - id)));
        }
        assert_eq!(
            resolved_order(&entries),
            vec![2, 3, 1],
            "a field of 3 compacts the pin onto the last seat"
        );

        for id in 4..=12 {
            entries.push(entry(id, &format!("Filler{id}"), None, Some(1000 - id)));
        }
        assert_eq!(
            entries.len(),
            12,
            "10 fillers plus the pin, seat 12 now within the field"
        );
        let order = resolved_order(&entries);
        assert_eq!(order.len(), 12);
        assert_eq!(
            order.last(),
            Some(&1),
            "the field grew into seat 12, so the pin holds it again"
        );
    }

    #[test]
    fn two_pins_past_the_end_keep_their_relative_order() {
        let mut entries = vec![
            pinned(10, "PinnedFirst", 11, None, None),
            pinned(11, "PinnedSecond", 12, None, None),
        ];
        for id in 1..=8 {
            entries.push(entry(id, &format!("Filler{id}"), None, Some(500 - id)));
        }
        let order = resolved_order(&entries);
        assert_eq!(
            &order[8..],
            &[10, 11],
            "both land at the end, in the order they were pinned"
        );
    }

    #[test]
    fn a_pin_on_a_withdrawn_entrant_is_ignored() {
        let mut withdrawn = pinned(1, "Gone", 1, None, Some(1000));
        withdrawn.status = "withdrawn".to_string();
        let entries = vec![withdrawn, entry(2, "Active", None, Some(900))];
        assert_eq!(
            resolved_order(&entries),
            vec![2],
            "a withdrawn entrant holds no seat, pinned or not"
        );
    }

    #[test]
    fn resolved_order_is_always_a_permutation_of_the_field() {
        let entries = vec![
            pinned(1, "A", 3, None, Some(950)),
            pinned(2, "B", 7, None, Some(900)),
            entry(3, "C", None, Some(850)),
            entry(4, "D", None, Some(800)),
            entry(5, "E", None, Some(750)),
        ];
        let order = resolved_order(&entries);
        assert_eq!(order.len(), entries.len());
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn effective_seed_prefers_the_real_seed_and_falls_back_to_the_pin() {
        let closed = TournamentEntry {
            seed: Some(2),
            manual_seed: Some(9),
            ..entry(1, "Closed", None, Some(1000))
        };
        assert_eq!(
            effective_seed(&closed),
            Some(2),
            "the real, closed seed wins over a stale pin"
        );

        let pin_only = pinned(2, "PinOnly", 5, None, Some(900));
        assert_eq!(effective_seed(&pin_only), Some(5));

        let neither = entry(3, "Neither", None, Some(800));
        assert_eq!(effective_seed(&neither), None);
    }

    #[test]
    fn seed_messages_render_in_both_locales() {
        let outcome = SeedOutcome::Pinned {
            display_name: "MarineLorD".to_string(),
            seed: 3,
            displaced: None,
        };
        let zh = outcome.message(Locale::ZhTw);
        let en = outcome.message(Locale::En);
        assert_ne!(zh, en);
        assert!(zh.contains('3'), "{zh}");
        assert!(en.contains("seed 3"), "{en}");
    }

    #[test]
    fn a_pin_that_displaces_someone_names_them_in_both_locales() {
        let outcome = SeedOutcome::Pinned {
            display_name: "MarineLorD".to_string(),
            seed: 3,
            displaced: Some("TheViper".to_string()),
        };
        let zh = outcome.message(Locale::ZhTw);
        let en = outcome.message(Locale::En);
        assert!(zh.contains("TheViper"), "{zh}");
        assert!(en.contains("TheViper") && en.contains("displacing"), "{en}");
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
