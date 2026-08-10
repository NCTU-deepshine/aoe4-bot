//! The seeding panel: a persistent message in
//! `#{slug}-bracket` showing the seeded field, posted when `/tournament
//! close-checkin` computes the first seeding and edited in place as an organizer
//! overrides seeds. `render` is the pure part, golden-string tested here;
//! `post_initial` and `refresh` are the thin Discord/DB glue `commands.rs` uses.
//!
//! No buttons: seeding is admin work done by command, so unlike the registration
//! and check-in panels there is nothing here for a player to press. Bilingual for
//! the same reason those two are — one shared message, many readers.

use crate::Error;
use crate::db::{to_channel_id, to_message_id};
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::seeding::display_order;
use crate::tournament::throttle::EditThrottle;
use serenity::all::{CacheHttp, ChannelId, CreateMessage, EditMessage, MessageId};
use sqlx::SqlitePool;
use std::time::Instant;

/// Entrants listed before the table is truncated. A 32-player field would blow
/// past Discord's 2000-character message limit otherwise; the bracket itself
/// is what shows a large field in full.
const SEED_DISPLAY_CAP: usize = 24;

/// Pure. Ordered by `seeding::display_order`, the same key the bracket drawing
/// uses — `seed` is authoritative, and `suggested_seed` is shown alongside only
/// so an organizer can see what they overrode.
pub(crate) fn render(name: &str, entries: &[TournamentEntry]) -> String {
    let field = display_order(entries);

    if field.is_empty() {
        // Phase-neutral: this panel exists from the moment a tournament does, so
        // it is what an organizer sees before check-in is a thing that has
        // happened, let alone one anyone has missed.
        return format!("**{name} — 種子名單 / Seeding**\n\n*尚無參賽者。 / No entrants yet.*");
    }

    let truncated = field.len().saturating_sub(SEED_DISPLAY_CAP);
    let mut rows: Vec<String> = field
        .iter()
        .take(SEED_DISPLAY_CAP)
        .map(|e| {
            let seed = e.seed.map_or_else(|| "—".to_string(), |s| s.to_string());
            // Two columns, never one blended number.
            let atr = e.atr.map_or_else(|| "—".to_string(), |a| format!("{a:.0}"));
            let elo = e.elo.map_or_else(|| "—".to_string(), |e| e.to_string());
            format!("`{seed:>3}` {} · ATR {atr} · ELO {elo}", e.display_name)
        })
        .collect();
    if truncated > 0 {
        rows.push(format!("…等 {truncated} 人 / and {truncated} more"));
    }

    format!(
        "**{name} — 種子名單 / Seeding**\n\
         ATR 與 ELO 是不同的評分標準，排序只是預設建議，並非表示兩者可以直接比較。\n\
         ATR and ELO are different scales — this order is a default, not a claim they are comparable.\n\
         ATR 資料來源 / ATR data by Andrey \"ISanych\" (@isanych_aoe), via aoe4world.\n\n\
         {}",
        rows.join("\n")
    )
}

/// Posts the panel to `#{slug}-bracket`, returning its id for `seed_message_id`.
pub(crate) async fn post_initial(
    http: impl CacheHttp,
    pool: &SqlitePool,
    channel_id: ChannelId,
    tournament_id: i64,
    name: &str,
) -> Result<MessageId, Error> {
    let entries = db::list_entries_for_tournament(pool, tournament_id).await?;
    let message = channel_id
        .send_message(http, CreateMessage::new().content(render(name, &entries)))
        .await?;
    Ok(message.id)
}

/// Re-renders the panel in place, coalescing a burst into one edit. A no-op if
/// the panel was never posted.
///
/// Throttled because the panel now follows the field from the first entrant, so
/// the Register and Withdraw buttons re-render it — the same reason
/// `panel::refresh` is, and they share the one `EditThrottle`, which is keyed by
/// message id.
pub(crate) async fn refresh(
    http: impl CacheHttp,
    pool: &SqlitePool,
    throttle: &EditThrottle,
    tournament: &Tournament,
) -> Result<(), Error> {
    let Some(seed_message_id) = tournament.seed_message_id else {
        return Ok(());
    };
    if !throttle.try_begin_edit(to_message_id(seed_message_id), Instant::now()) {
        return Ok(());
    }
    refresh_now(http, pool, tournament).await
}

/// The unconditional edit, for an admin command rather than a button press: a
/// phase change or a reordering fires once and deserves a guaranteed edit rather
/// than one the throttle may coalesce away.
pub(crate) async fn refresh_now(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> Result<(), Error> {
    let (Some(seed_message_id), Some(bracket_channel_id)) = (tournament.seed_message_id, tournament.bracket_channel_id)
    else {
        return Ok(());
    };

    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let channel_id = to_channel_id(bracket_channel_id);
    channel_id
        .edit_message(
            http,
            to_message_id(seed_message_id),
            EditMessage::new().content(render(&tournament.name, &entries)),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(
        user_id: i64,
        display_name: &str,
        seed: Option<i64>,
        atr: Option<f64>,
        elo: Option<i64>,
    ) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: Some(user_id * 100),
            invited_by: None,
            seed,
            suggested_seed: seed,
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
    fn lists_the_field_in_seed_order_with_both_ratings() {
        let entries = vec![
            entry(1, "Second", Some(2), None, Some(1400)),
            entry(2, "First", Some(1), Some(2292.531382), None),
        ];
        let content = render("Relic Cup", &entries);
        let first = content.find("First").unwrap();
        let second = content.find("Second").unwrap();
        assert!(first < second, "seed 1 should be listed first:\n{content}");
        // ATR is rounded for display but each rating keeps its own column.
        assert!(content.contains("ATR 2293"), "{content}");
        assert!(content.contains("ELO 1400"), "{content}");
    }

    #[test]
    fn unseeded_entrants_are_listed_after_the_seeded_ones_by_rating() {
        // Registration order used to decide this, which put a stronger latecomer
        // below a weaker one and disagreed with the bracket drawing.
        let entries = vec![
            entry(1, "Seeded", Some(1), None, Some(1000)),
            entry(2, "Weaker", None, None, Some(1200)),
            entry(3, "Stronger", None, None, Some(1800)),
        ];
        let content = render("Relic Cup", &entries);
        let seeded = content.find("Seeded").unwrap();
        let stronger = content.find("Stronger").unwrap();
        let weaker = content.find("Weaker").unwrap();
        assert!(seeded < stronger, "the seed outranks both:\n{content}");
        assert!(stronger < weaker, "unseeded go by rating, not id:\n{content}");
    }

    #[test]
    fn a_missing_rating_renders_as_a_dash_rather_than_a_zero() {
        let content = render("Relic Cup", &[entry(1, "Unrated", Some(1), None, None)]);
        assert!(content.contains("ATR — · ELO —"), "{content}");
    }

    #[test]
    fn carries_both_languages_and_the_scale_disclaimer() {
        // The "not comparable" caveat belongs in the output, where players read it.
        let content = render("Relic Cup", &[entry(1, "A", Some(1), None, None)]);
        assert!(content.contains("種子名單"));
        assert!(content.contains("Seeding"));
        assert!(content.contains("不同的評分標準"));
        assert!(content.contains("different scales"));
    }

    #[test]
    fn credits_the_atr_source() {
        // The source is credited wherever ATR is displayed.
        let content = render("Relic Cup", &[entry(1, "A", Some(1), Some(1500.0), None)]);
        assert!(content.contains("ISanych"), "{content}");
    }

    #[test]
    fn excludes_no_shows() {
        let mut no_show = entry(2, "NoShow", None, Some(2200.0), None);
        no_show.status = "no_show".to_string();
        let content = render("Relic Cup", &[entry(1, "Active", Some(1), None, None), no_show]);
        assert!(!content.contains("NoShow"), "{content}");
    }

    #[test]
    fn truncates_a_large_field_and_says_how_many_are_hidden() {
        let entries: Vec<TournamentEntry> = (1..=32)
            .map(|i| entry(i, &format!("Player{i}"), Some(i), None, Some(1000 + i)))
            .collect();
        let content = render("Relic Cup", &entries);
        assert!(content.contains("…等 8 人 / and 8 more"), "{content}");
        assert!(content.len() < 2000, "must fit Discord's limit, was {}", content.len());
    }

    #[test]
    fn renders_a_placeholder_for_an_empty_field() {
        // Phase-neutral wording: this is what a brand-new tournament's panel says,
        // long before check-in is a step anyone could have missed.
        let content = render("Relic Cup", &[]);
        assert!(content.contains("尚無參賽者"), "{content}");
        assert!(content.contains("No entrants yet"), "{content}");
        assert!(!content.contains("checked-in"), "{content}");
    }

    #[test]
    fn a_field_nobody_has_seeded_still_lists_everyone() {
        // The registration-phase case the panel could not previously be in: no
        // seeds anywhere, so every row shows a dash and the order is the tiering.
        let entries = vec![
            entry(1, "Weaker", None, None, Some(1200)),
            entry(2, "Stronger", None, None, Some(1800)),
        ];
        let content = render("Relic Cup", &entries);
        assert!(content.contains("Stronger"), "{content}");
        assert!(content.contains("Weaker"), "{content}");
        assert!(
            content.find("Stronger").unwrap() < content.find("Weaker").unwrap(),
            "unseeded go by rating:\n{content}"
        );
        assert!(content.contains("`  —`"), "an unseeded row shows a dash:\n{content}");
    }
}
