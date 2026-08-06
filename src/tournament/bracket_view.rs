//! The bracket as Discord sees it (docs/tournament.md §8.6): a persistent,
//! multi-message drawing in `#{slug}-bracket`.
//!
//! It exists from the first two entrants, long before `/tournament start`, so
//! the field can watch the draw take shape. Before the event begins it is a
//! **preview** — computed from the current entrants and labelled as provisional —
//! and afterwards the same messages carry the real thing, edited in place.
//!
//! `bracket::build` and `render::render` are pure and cheap, so redrawing costs
//! nothing worth counting. What costs is Discord: a 16-player bracket is several
//! messages, which is what `reconcile` is about.

use crate::Error;
use crate::tournament::db::{self, Tournament, TournamentEntry};
use crate::tournament::seeding;
use crate::tournament::{bracket, render};
use serenity::all::{CacheHttp, ChannelId, CreateMessage, EditMessage, MessageId};
use sqlx::SqlitePool;

/// A bracket needs two sides; below that there is nothing to draw.
const MIN_ENTRANTS: usize = 2;

/// `bracket::build` insists on one `best_of` per round, but nothing rendered
/// depends on it — `render::Round` carries a name and matches, and match length
/// appears nowhere in the drawing. So the preview supplies a filler and needs no
/// draft preset, which is what lets it exist during registration.
const RENDER_ONLY_BEST_OF: u8 = 1;

/// The provisional bracket implied by the current field.
///
/// Ordered by `seeding::suggested_order`, so it is genuinely informative once
/// ELO lands at sign-up (chunk 28) and exact once seeding has run. `None` below
/// two entrants, where `bracket::build` correctly refuses.
pub(crate) fn preview_rounds(entries: &[TournamentEntry]) -> Option<Vec<render::Round>> {
    let order = seeding::suggested_order(entries);
    if order.len() < MIN_ENTRANTS {
        return None;
    }

    // Seed n is the nth entrant in the suggested order.
    let names: Vec<&str> = order
        .iter()
        .filter_map(|user_id| {
            entries
                .iter()
                .find(|e| e.user_id == *user_id)
                .map(|e| e.display_name.as_str())
        })
        .collect();

    let round_count = bracket::round_count(bracket::size(order.len()));
    let built = bracket::build(order.len(), &vec![RENDER_ONLY_BEST_OF; round_count]).ok()?;

    Some(
        built
            .rounds
            .iter()
            .map(|round| render::Round {
                name: round.name.clone(),
                matches: round
                    .sets
                    .iter()
                    .map(|set| render::Match {
                        slot1: entrant(set.slot1, &names),
                        slot2: entrant(set.slot2, &names),
                        // Nothing has been played, so no scores and no winners —
                        // §8.6 wants a blank rather than a zero.
                        score: None,
                        winner: None,
                    })
                    .collect(),
            })
            .collect(),
    )
}

fn entrant(seed: Option<u32>, names: &[&str]) -> Option<render::Entrant> {
    let seed = seed?;
    let name = names.get(seed as usize - 1)?;
    Some(render::Entrant {
        seed,
        name: (*name).to_string(),
    })
}

/// Wraps the drawing with a heading, and says plainly that it is not the draw
/// yet while the tournament has not started. Bilingual: one shared message with
/// many readers (§8.10).
fn decorate(name: &str, chunks: Vec<String>, provisional: bool) -> Vec<String> {
    let heading = if provisional {
        format!(
            "**{name} — 賽程表預覽 / bracket preview**\n\
             這是依目前報名者推算的暫定賽程表，開賽前都可能變動。\n\
             Provisional, based on who has registered so far; it will change until the event starts.\n"
        )
    } else {
        format!("**{name} — 賽程表 / bracket**\n")
    };

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| if index == 0 { format!("{heading}{chunk}") } else { chunk })
        .collect()
}

/// Draws the current bracket into `#{slug}-bracket`, reusing the messages that
/// are already there.
///
/// The message count is **not stable**: it follows the bracket size, which jumps
/// at powers of two, so a field growing from 8 to 9 turns one message into
/// three. Each chunk is therefore edited if a message already holds that
/// ordinal, posted if not, and any surplus tail deleted — otherwise the bottom
/// of a bigger bracket lingers under a smaller one.
pub(crate) async fn reconcile(http: impl CacheHttp, pool: &SqlitePool, tournament: &Tournament) -> Result<(), Error> {
    let Some(bracket_channel_id) = tournament.bracket_channel_id else {
        return Ok(());
    };
    let channel_id = ChannelId::new(u64::try_from(bracket_channel_id).unwrap());

    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    let Some(rounds) = preview_rounds(&entries) else {
        return Ok(());
    };
    let provisional = tournament.status != "running" && tournament.status != "completed";
    let chunks = decorate(
        &tournament.name,
        render::render(&rounds, render::DEFAULT_WIDTH),
        provisional,
    );

    let existing = db::list_bracket_messages(pool, tournament.id).await?;
    for (index, chunk) in chunks.iter().enumerate() {
        let ordinal = i64::try_from(index).unwrap();
        match existing.iter().find(|m| m.ordinal == ordinal) {
            Some(message) => {
                let message_id = MessageId::new(u64::try_from(message.message_id).unwrap());
                channel_id
                    .edit_message(&http, message_id, EditMessage::new().content(chunk))
                    .await?;
            },
            None => {
                let posted = channel_id
                    .send_message(&http, CreateMessage::new().content(chunk))
                    .await?;
                db::upsert_bracket_message(pool, tournament.id, ordinal, i64::try_from(posted.id.get()).unwrap())
                    .await?;
            },
        }
    }

    // Anything past the last chunk belongs to a bracket that no longer exists.
    let surplus = i64::try_from(chunks.len()).unwrap();
    for message in existing.iter().filter(|m| m.ordinal >= surplus) {
        let message_id = MessageId::new(u64::try_from(message.message_id).unwrap());
        // `delete_message` wants `AsRef<Http>` where the others take `CacheHttp`.
        if let Err(err) = channel_id.delete_message(http.http(), message_id).await {
            tracing::error!(
                "failed to delete surplus bracket message {message_id} for tournament {}: {err:?}",
                tournament.id
            );
        }
    }
    db::delete_bracket_messages_from(pool, tournament.id, surplus).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(user_id: i64, display_name: &str, elo: Option<i64>) -> TournamentEntry {
        TournamentEntry {
            tournament_id: 1,
            user_id,
            aoe4_id: user_id * 100,
            seed: None,
            suggested_seed: None,
            display_name: display_name.to_string(),
            elo,
            atr: None,
            atr_source: None,
            status: "active".to_string(),
            registered_at: Utc::now(),
            checked_in_at: None,
        }
    }

    fn field(n: i64) -> Vec<TournamentEntry> {
        // Descending ELO so the suggested order matches the numbering.
        (1..=n).map(|i| entry(i, &format!("P{i}"), Some(2000 - i))).collect()
    }

    #[test]
    fn there_is_nothing_to_draw_below_two_entrants() {
        assert!(preview_rounds(&[]).is_none());
        assert!(preview_rounds(&field(1)).is_none());
    }

    #[test]
    fn two_entrants_are_a_single_final() {
        let rounds = preview_rounds(&field(2)).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].name, "Final");
        assert_eq!(rounds[0].matches.len(), 1);
    }

    #[test]
    fn a_field_that_is_not_a_power_of_two_gets_byes_on_the_top_seeds() {
        // 5 entrants play an 8-bracket, so seeds 1, 2 and 3 are unopposed.
        let rounds = preview_rounds(&field(5)).unwrap();
        assert_eq!(rounds.len(), 3);

        let unopposed: Vec<u32> = rounds[0]
            .matches
            .iter()
            .filter(|m| m.slot1.is_some() != m.slot2.is_some())
            .filter_map(|m| m.slot1.as_ref().or(m.slot2.as_ref()).map(|e| e.seed))
            .collect();
        assert_eq!(unopposed, vec![1, 2, 3]);
    }

    #[test]
    fn the_order_follows_the_suggested_seeding_not_registration() {
        // P3 has the best rating despite registering last, so it takes seed 1.
        let entries = vec![
            entry(1, "Weak", Some(1000)),
            entry(2, "Middle", Some(1500)),
            entry(3, "Strong", Some(2000)),
        ];
        let rounds = preview_rounds(&entries).unwrap();
        let top = rounds[0]
            .matches
            .iter()
            .find_map(|m| m.slot1.as_ref().filter(|e| e.seed == 1))
            .expect("seed 1 should be placed");
        assert_eq!(top.name, "Strong");
    }

    #[test]
    fn no_scores_before_anything_is_played() {
        let rounds = preview_rounds(&field(4)).unwrap();
        assert!(rounds.iter().flat_map(|r| &r.matches).all(|m| m.score.is_none()));
    }

    #[test]
    fn withdrawn_entrants_are_not_in_the_draw() {
        let mut entries = field(4);
        entries[3].status = "withdrawn".to_string();
        let rounds = preview_rounds(&entries).unwrap();
        // Three entrants play a 4-bracket: two rounds, one bye.
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].matches.len(), 2);
    }

    #[test]
    fn the_preview_says_it_is_provisional_and_the_real_bracket_does_not() {
        let chunks = decorate("Relic Cup", vec!["body".to_string()], true);
        assert!(chunks[0].contains("賽程表預覽"), "{}", chunks[0]);
        assert!(chunks[0].contains("Provisional"), "{}", chunks[0]);

        let chunks = decorate("Relic Cup", vec!["body".to_string()], false);
        assert!(!chunks[0].contains("Provisional"), "{}", chunks[0]);
    }

    #[test]
    fn only_the_first_chunk_carries_the_heading() {
        let chunks = decorate("Relic Cup", vec!["one".to_string(), "two".to_string()], true);
        assert!(chunks[0].contains("Relic Cup"));
        assert_eq!(chunks[1], "two", "a continuation chunk is the drawing alone");
    }

    #[test]
    fn a_large_field_splits_into_several_messages() {
        // §8.6: the split starts at 16, which is what makes the message count
        // vary with the field and `reconcile` necessary.
        let rounds = preview_rounds(&field(16)).unwrap();
        let chunks = render::render(&rounds, render::DEFAULT_WIDTH);
        assert!(chunks.len() > 1, "16 entrants should not fit one message");
        assert!(chunks.iter().all(|c| c.len() <= 2000));
    }
}
