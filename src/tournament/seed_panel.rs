//! The seeding panel (docs/tournament.md §8.5): a persistent message in
//! `#{slug}-bracket` showing the seeded field, posted when `/tournament
//! close-checkin` computes the first seeding and edited in place as an organizer
//! overrides seeds. `render` is the pure part, golden-string tested here;
//! `post_initial` and `refresh` are the thin Discord/DB glue `commands.rs` uses.
//!
//! No buttons: seeding is admin work done by command, so unlike the registration
//! and check-in panels there is nothing here for a player to press. Bilingual for
//! the same reason those two are (§8.10) — one shared message, many readers.

use crate::Error;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::seeding::seedable;
use serenity::all::{CacheHttp, ChannelId, CreateMessage, EditMessage, MessageId};
use sqlx::SqlitePool;

/// Entrants listed before the table is truncated. A 32-player field would blow
/// past Discord's 2000-character message limit otherwise; the bracket itself
/// (chunk 12) is what shows a large field in full.
const SEED_DISPLAY_CAP: usize = 24;

/// Pure. Sorted by `seed`, which is authoritative — `suggested_seed` is shown
/// alongside only so an organizer can see what they overrode (§6).
pub(crate) fn render(name: &str, entries: &[TournamentEntry]) -> String {
    let mut field = seedable(entries);
    field.sort_by_key(|e| (e.seed.unwrap_or(i64::MAX), e.user_id));

    if field.is_empty() {
        return format!("**{name} — 種子名單 / Seeding**\n\n*尚無已簽到的參賽者。 / No checked-in entrants yet.*");
    }

    let truncated = field.len().saturating_sub(SEED_DISPLAY_CAP);
    let mut rows: Vec<String> = field
        .iter()
        .take(SEED_DISPLAY_CAP)
        .map(|e| {
            let seed = e.seed.map_or_else(|| "—".to_string(), |s| s.to_string());
            // Two columns, never one blended number (§6).
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

/// Re-renders the panel in place. Unthrottled, unlike the two player-facing
/// panels: this only ever fires on an admin command, never on a button press, so
/// there is no burst to coalesce. A no-op if the panel was never posted.
pub(crate) async fn refresh(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> Result<(), Error> {
    let (Some(seed_message_id), Some(bracket_channel_id)) = (tournament.seed_message_id, tournament.bracket_channel_id)
    else {
        return Ok(());
    };

    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let channel_id = ChannelId::new(u64::try_from(bracket_channel_id).unwrap());
    channel_id
        .edit_message(
            http,
            MessageId::new(u64::try_from(seed_message_id).unwrap()),
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
            aoe4_id: user_id * 100,
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
    fn a_missing_rating_renders_as_a_dash_rather_than_a_zero() {
        let content = render("Relic Cup", &[entry(1, "Unrated", Some(1), None, None)]);
        assert!(content.contains("ATR — · ELO —"), "{content}");
    }

    #[test]
    fn carries_both_languages_and_the_scale_disclaimer() {
        // §6 requires the "not comparable" caveat in the output, not just the doc.
        let content = render("Relic Cup", &[entry(1, "A", Some(1), None, None)]);
        assert!(content.contains("種子名單"));
        assert!(content.contains("Seeding"));
        assert!(content.contains("不同的評分標準"));
        assert!(content.contains("different scales"));
    }

    #[test]
    fn credits_the_atr_source() {
        // §6: "credit the source wherever ATR is displayed".
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
        let content = render("Relic Cup", &[]);
        assert!(content.contains("尚無已簽到的參賽者"));
        assert!(content.contains("No checked-in entrants yet"));
    }
}
