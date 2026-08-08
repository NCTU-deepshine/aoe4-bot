//! The bracket as Discord sees it: a persistent,
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
use crate::db::{to_channel_id, to_db_id, to_message_id};
use crate::tournament::db::{self, Tournament, TournamentEntry, TournamentRound, TournamentSet};
use crate::tournament::seeding;
use crate::tournament::{bracket, render};
use serenity::all::{CacheHttp, CreateMessage, EditMessage};
use sqlx::SqlitePool;

/// A bracket needs two sides; below that there is nothing to draw.
const MIN_ENTRANTS: usize = 2;

/// What `reconcile` did.
///
/// Two of these are ordinary quiet outcomes rather than errors, but a repair
/// command reporting them as success is how a bracket that never appeared got
/// announced as "repaired" — so they are named and returned instead of being
/// folded into `Ok(())`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReconcileOutcome {
    /// The tournament row has no bracket channel, so there is nowhere to draw.
    NoChannel,
    /// Under two active entrants — there is nothing to draw yet.
    TooFewEntrants,
    Drawn {
        posted: usize,
        edited: usize,
        deleted: usize,
    },
}

impl ReconcileOutcome {
    /// Whether the channel's contents actually changed, as opposed to the
    /// drawing already being current.
    pub(crate) fn changed(&self) -> bool {
        matches!(self, Self::Drawn { posted, deleted, .. } if *posted > 0 || *deleted > 0)
    }
}

/// `bracket::build` insists on one `best_of` per round, but nothing rendered
/// depends on it — `render::Round` carries a name and matches, and match length
/// appears nowhere in the drawing. So the preview supplies a filler and needs no
/// draft preset, which is what lets it exist during registration.
const RENDER_ONLY_BEST_OF: u8 = 1;

/// The draw order: one entrant per bracket position, each carrying the seed it
/// should be drawn with.
///
/// Seeds win once they exist — they are what `start` builds from, so an
/// organizer's override has to be visible here — and the rating tiering orders
/// whoever has no seed yet.
///
/// `seeding::display_order` decides who stands where, so the drawing and the
/// seeding panel cannot disagree; this only turns that into drawable entrants.
fn draw_order(entries: &[TournamentEntry]) -> Vec<render::Entrant> {
    let field = seeding::display_order(entries);

    // Seeds are shown as stored — a withdrawal leaves gaps, and renumbering by
    // position would contradict the panel. Latecomers continue past the last
    // one rather than reusing a number that is already taken.
    let mut unseeded = field.iter().filter_map(|e| e.seed).max().unwrap_or(0);
    field
        .iter()
        .filter_map(|e| {
            let seed = e.seed.unwrap_or_else(|| {
                unseeded += 1;
                unseeded
            });
            Some(render::Entrant {
                seed: u32::try_from(seed).ok()?,
                name: e.display_name.clone(),
            })
        })
        .collect()
}

/// The provisional bracket implied by the current field.
///
/// `None` below two entrants, where `bracket::build` correctly refuses.
pub(crate) fn preview_rounds(entries: &[TournamentEntry]) -> Option<Vec<render::Round>> {
    let order = draw_order(entries);
    if order.len() < MIN_ENTRANTS {
        return None;
    }

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
                        slot1: entrant(set.slot1, &order),
                        slot2: entrant(set.slot2, &order),
                        // Nothing has been played, so no scores and no winners —
                        // A blank rather than a zero.
                        score: None,
                        winner: None,
                    })
                    .collect(),
            })
            .collect(),
    )
}

/// `bracket::build` names slots by position in the draw, which is this vector's
/// index — not necessarily the seed the entrant is drawn with.
fn entrant(position: Option<u32>, order: &[render::Entrant]) -> Option<render::Entrant> {
    order.get(position? as usize - 1).cloned()
}

/// The real bracket, read back from the rows `start` wrote — so it shows the
/// seeds actually used and the bye winners already advanced, neither of which a
/// preview recomputed from ratings can know.
///
/// Pure over already-fetched data, like `preview_rounds`. Rounds are drawn in
/// the order given (`db::list_rounds_for_stage` returns them by ordinal, and
/// concatenating stages in order keeps that true); sets are sorted by position.
pub(crate) fn played_rounds(
    rounds: &[TournamentRound],
    sets: &[TournamentSet],
    entries: &[TournamentEntry],
) -> Vec<render::Round> {
    rounds
        .iter()
        .map(|round| {
            let mut round_sets: Vec<&TournamentSet> = sets.iter().filter(|s| s.round_id == round.id).collect();
            round_sets.sort_by_key(|s| s.position);

            render::Round {
                name: round.name.clone(),
                matches: round_sets.iter().map(|set| played_match(set, entries)).collect(),
            }
        })
        .collect()
}

fn played_match(set: &TournamentSet, entries: &[TournamentEntry]) -> render::Match {
    let slot = |user_id: Option<i64>| {
        let user_id = user_id?;
        let entry = entries.iter().find(|e| e.user_id == user_id)?;
        Some(render::Entrant {
            // A started bracket has seeded everyone, so the fallback is unreachable.
            seed: entry.seed.and_then(|s| u32::try_from(s).ok()).unwrap_or_default(),
            name: entry.display_name.clone(),
        })
    };

    let winner = set
        .winner_user_id
        .and_then(|winner| match (set.slot1_user_id, set.slot2_user_id) {
            (Some(one), _) if one == winner => Some(bracket::Slot::One),
            (_, Some(two)) if two == winner => Some(bracket::Slot::Two),
            _ => None,
        });

    // A blank rather than a zero, which also stops a bye — decided
    // without anyone playing — from reading `0-0`.
    let score = (set.slot1_wins + set.slot2_wins > 0).then(|| {
        (
            u8::try_from(set.slot1_wins).unwrap_or(u8::MAX),
            u8::try_from(set.slot2_wins).unwrap_or(u8::MAX),
        )
    });

    render::Match {
        slot1: slot(set.slot1_user_id),
        slot2: slot(set.slot2_user_id),
        score,
        winner,
    }
}

/// Wraps the drawing with a heading, and says plainly that it is not the draw
/// yet while the tournament has not started. Bilingual: one shared message with
/// many readers.
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

/// The drawn bracket once `start` has written one, `None` while it has not —
/// which is what separates the real thing from a preview.
async fn persisted_rounds(
    pool: &SqlitePool,
    tournament_id: i64,
    entries: &[TournamentEntry],
) -> Result<Option<Vec<render::Round>>, Error> {
    let sets = db::list_sets_for_tournament(pool, tournament_id).await?;
    if sets.is_empty() {
        return Ok(None);
    }

    let mut rounds = Vec::new();
    for stage in db::list_stages_for_tournament(pool, tournament_id).await? {
        rounds.extend(db::list_rounds_for_stage(pool, stage.id).await?);
    }

    Ok(Some(played_rounds(&rounds, &sets, entries)))
}

/// Draws the current bracket into `#{slug}-bracket`, reusing the messages that
/// are already there.
///
/// The message count is **not stable**: it follows the bracket size, which jumps
/// at powers of two, so a field growing from 8 to 9 turns one message into
/// three. Each chunk is therefore edited if a message already holds that
/// ordinal, posted if not, and any surplus tail deleted — otherwise the bottom
/// of a bigger bracket lingers under a smaller one.
pub(crate) async fn reconcile(
    http: impl CacheHttp,
    pool: &SqlitePool,
    tournament: &Tournament,
) -> Result<ReconcileOutcome, Error> {
    let Some(bracket_channel_id) = tournament.bracket_channel_id else {
        return Ok(ReconcileOutcome::NoChannel);
    };
    let channel_id = to_channel_id(bracket_channel_id);

    let entries = db::list_entries_for_tournament(pool, tournament.id).await?;
    // Whether this is still a preview is the same question as which drawing we
    // have, so it is answered once rather than read off the status separately.
    let (rounds, provisional) = match persisted_rounds(pool, tournament.id, &entries).await? {
        Some(rounds) => (rounds, false),
        None => match preview_rounds(&entries) {
            Some(rounds) => (rounds, true),
            None => return Ok(ReconcileOutcome::TooFewEntrants),
        },
    };
    let chunks = decorate(
        &tournament.name,
        render::render(&rounds, render::DEFAULT_WIDTH),
        provisional,
    );

    let (mut posted, mut edited, mut deleted) = (0, 0, 0);
    let existing = db::list_bracket_messages(pool, tournament.id).await?;
    for (index, chunk) in chunks.iter().enumerate() {
        let ordinal = i64::try_from(index).unwrap();
        match existing.iter().find(|m| m.ordinal == ordinal) {
            Some(message) => {
                let message_id = to_message_id(message.message_id);
                channel_id
                    .edit_message(&http, message_id, EditMessage::new().content(chunk))
                    .await?;
                edited += 1;
            },
            None => {
                let message = channel_id
                    .send_message(&http, CreateMessage::new().content(chunk))
                    .await?;
                db::upsert_bracket_message(pool, tournament.id, ordinal, to_db_id(message.id)).await?;
                posted += 1;
            },
        }
    }

    // Anything past the last chunk belongs to a bracket that no longer exists.
    let surplus = i64::try_from(chunks.len()).unwrap();
    for message in existing.iter().filter(|m| m.ordinal >= surplus) {
        let message_id = to_message_id(message.message_id);
        // `delete_message` wants `AsRef<Http>` where the others take `CacheHttp`.
        if let Err(err) = channel_id.delete_message(http.http(), message_id).await {
            tracing::error!(
                "failed to delete surplus bracket message {message_id} for tournament {}: {err:?}",
                tournament.id
            );
        } else {
            deleted += 1;
        }
    }
    db::delete_bracket_messages_from(pool, tournament.id, surplus).await?;

    Ok(ReconcileOutcome::Drawn {
        posted,
        edited,
        deleted,
    })
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
    fn only_a_posted_or_deleted_message_counts_as_a_repair() {
        // An edit means the drawing was already there and merely stale. Calling
        // that a repair is how a bracket that never appeared got reported as
        // fixed, so the distinction is the point of the type.
        assert!(
            ReconcileOutcome::Drawn {
                posted: 1,
                edited: 0,
                deleted: 0
            }
            .changed()
        );
        assert!(
            ReconcileOutcome::Drawn {
                posted: 0,
                edited: 0,
                deleted: 2
            }
            .changed()
        );
        assert!(
            !ReconcileOutcome::Drawn {
                posted: 0,
                edited: 3,
                deleted: 0
            }
            .changed()
        );
        assert!(!ReconcileOutcome::NoChannel.changed());
        assert!(!ReconcileOutcome::TooFewEntrants.changed());
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

    fn with_seed(mut entry: TournamentEntry, seed: i64) -> TournamentEntry {
        entry.seed = Some(seed);
        entry
    }

    /// Every entrant drawn in the opening round, in slot order.
    fn drawn(rounds: &[render::Round]) -> Vec<&render::Entrant> {
        rounds[0]
            .matches
            .iter()
            .flat_map(|m| [m.slot1.as_ref(), m.slot2.as_ref()])
            .flatten()
            .collect()
    }

    #[test]
    fn a_seed_override_beats_the_rating_it_contradicts() {
        // The whole point of `/tournament seed set`: the tiering would put
        // Strong first, and the organizer has said otherwise.
        let entries = vec![
            with_seed(entry(1, "Weak", Some(1000)), 1),
            with_seed(entry(2, "Middle", Some(1500)), 2),
            with_seed(entry(3, "Strong", Some(2000)), 3),
        ];
        let rounds = preview_rounds(&entries).unwrap();
        let top = drawn(&rounds)
            .into_iter()
            .find(|e| e.seed == 1)
            .expect("seed 1 should be placed");
        assert_eq!(top.name, "Weak");
    }

    #[test]
    fn seeds_left_with_gaps_keep_their_real_numbers() {
        // A withdrawal after seeding leaves 1, 2, 4, 5. Renumbering those to
        // 1..4 would make the drawing disagree with the seeding panel.
        let entries = vec![
            with_seed(entry(1, "A", Some(2000)), 1),
            with_seed(entry(2, "B", Some(1900)), 2),
            with_seed(entry(3, "C", Some(1800)), 4),
            with_seed(entry(4, "D", Some(1700)), 5),
        ];
        let rounds = preview_rounds(&entries).unwrap();
        let mut seeds: Vec<u32> = drawn(&rounds).into_iter().map(|e| e.seed).collect();
        seeds.sort_unstable();
        assert_eq!(seeds, vec![1, 2, 4, 5]);
    }

    #[test]
    fn a_part_seeded_field_keeps_the_seeds_it_has() {
        // Reopening registration lets a latecomer in unseeded. That must not
        // discard the seeds already assigned — the seeding panel still shows
        // them, and the drawing has to agree.
        let entries = vec![
            with_seed(entry(1, "Weak", Some(1000)), 1),
            entry(2, "Middle", Some(1500)),
            entry(3, "Strong", Some(2000)),
        ];
        let rounds = preview_rounds(&entries).unwrap();
        let placed: Vec<(u32, &str)> = drawn(&rounds).into_iter().map(|e| (e.seed, e.name.as_str())).collect();

        // Seed 1 stands; the unseeded pair follow it in rating order.
        assert!(placed.contains(&(1, "Weak")), "{placed:?}");
        assert!(placed.contains(&(2, "Strong")), "{placed:?}");
        assert!(placed.contains(&(3, "Middle")), "{placed:?}");
    }

    #[test]
    fn latecomers_are_numbered_past_the_last_seed_not_over_it() {
        // Seeds 1, 2 and 5 are taken, so the unseeded entrant becomes 6 —
        // reusing 3 or 4 would put two entrants on the same number.
        let entries = vec![
            with_seed(entry(1, "A", Some(2000)), 1),
            with_seed(entry(2, "B", Some(1900)), 2),
            with_seed(entry(3, "C", Some(1800)), 5),
            entry(4, "Latecomer", Some(1700)),
        ];
        let rounds = preview_rounds(&entries).unwrap();
        let mut seeds: Vec<u32> = drawn(&rounds).into_iter().map(|e| e.seed).collect();
        seeds.sort_unstable();
        assert_eq!(seeds, vec![1, 2, 5, 6]);
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

    fn round(id: i64, ordinal: i64, name: &str) -> TournamentRound {
        TournamentRound {
            id,
            stage_id: 1,
            ordinal,
            name: name.to_string(),
            best_of: 3,
            bracket: None,
            draft_preset_id: None,
            rules: None,
        }
    }

    fn set(id: i64, round_id: i64, position: i64, slot1: Option<i64>, slot2: Option<i64>) -> TournamentSet {
        TournamentSet {
            id,
            tournament_id: 1,
            round_id,
            position,
            slot1_user_id: slot1,
            slot2_user_id: slot2,
            slot1_wins: 0,
            slot2_wins: 0,
            winner_user_id: None,
            status: "pending".to_string(),
            draft_external_id: None,
            draft_synced_at: None,
            draft_announce_message_id: None,
            redraft_count: 0,
            thread_id: None,
            winner_advances_to_set_id: None,
            winner_advances_to_slot: None,
            loser_advances_to_set_id: None,
            loser_advances_to_slot: None,
            scheduled_at: None,
            completed_at: None,
        }
    }

    /// A seeded field, as `start` leaves it.
    fn seeded_field(n: i64) -> Vec<TournamentEntry> {
        (1..=n)
            .map(|i| with_seed(entry(i, &format!("P{i}"), None), i))
            .collect()
    }

    #[test]
    fn the_persisted_bracket_takes_names_and_seeds_from_the_entries() {
        let played = played_rounds(
            &[round(10, 1, "Final")],
            &[set(100, 10, 1, Some(1), Some(2))],
            &seeded_field(2),
        );
        assert_eq!(played.len(), 1);
        assert_eq!(played[0].name, "Final");

        let m = &played[0].matches[0];
        assert_eq!(m.slot1.as_ref().unwrap().name, "P1");
        assert_eq!(m.slot1.as_ref().unwrap().seed, 1);
        assert_eq!(m.slot2.as_ref().unwrap().seed, 2);
    }

    #[test]
    fn sets_are_drawn_by_position_whatever_order_they_arrive_in() {
        let played = played_rounds(
            &[round(10, 1, "Semifinal")],
            &[set(101, 10, 2, Some(2), Some(3)), set(100, 10, 1, Some(1), Some(4))],
            &seeded_field(4),
        );
        let top: Vec<&str> = played[0]
            .matches
            .iter()
            .map(|m| m.slot1.as_ref().unwrap().name.as_str())
            .collect();
        assert_eq!(top, vec!["P1", "P2"]);
    }

    #[test]
    fn an_unplayed_set_shows_no_score() {
        let played = played_rounds(
            &[round(10, 1, "Final")],
            &[set(100, 10, 1, Some(1), Some(2))],
            &seeded_field(2),
        );
        assert!(played[0].matches[0].score.is_none(), "a blank, not a zero");
        assert!(played[0].matches[0].winner.is_none());
    }

    #[test]
    fn a_completed_set_shows_both_counts_and_the_winner() {
        let mut decided = set(100, 10, 1, Some(1), Some(2));
        decided.slot1_wins = 1;
        decided.slot2_wins = 2;
        decided.winner_user_id = Some(2);
        decided.status = "completed".to_string();

        let played = played_rounds(&[round(10, 1, "Final")], &[decided], &seeded_field(2));
        assert_eq!(played[0].matches[0].score, Some((1, 2)));
        assert_eq!(played[0].matches[0].winner, Some(bracket::Slot::Two));
    }

    #[test]
    fn a_bye_advances_its_occupant_without_a_scoreline() {
        let mut bye = set(100, 10, 1, Some(1), None);
        bye.winner_user_id = Some(1);
        bye.status = "bye".to_string();

        let played = played_rounds(&[round(10, 1, "Semifinal")], &[bye], &seeded_field(2));
        let m = &played[0].matches[0];
        assert!(m.slot2.is_none());
        assert!(m.score.is_none(), "nobody played, so 0-0 would be a lie");
        assert_eq!(m.winner, Some(bracket::Slot::One));
    }

    #[test]
    fn starting_does_not_visibly_redraw_the_tree() {
        // The preview and the real bracket of the same field are the same
        // shape, so `/tournament start` only changes the label and the scores.
        let entries = seeded_field(4);
        let preview = preview_rounds(&entries).unwrap();
        let played = played_rounds(
            &[round(10, 1, "Semifinal"), round(11, 2, "Final")],
            &[
                set(100, 10, 1, Some(1), Some(4)),
                set(101, 10, 2, Some(2), Some(3)),
                set(102, 11, 1, None, None),
            ],
            &entries,
        );

        let shape = |rounds: &[render::Round]| rounds.iter().map(|r| r.matches.len()).collect::<Vec<_>>();
        assert_eq!(shape(&preview), shape(&played));
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
        // The split starts at 16, which is what makes the message count
        // vary with the field and `reconcile` necessary.
        let rounds = preview_rounds(&field(16)).unwrap();
        let chunks = render::render(&rounds, render::DEFAULT_WIDTH);
        assert!(chunks.len() > 1, "16 entrants should not fit one message");
        assert!(chunks.iter().all(|c| c.len() <= 2000));
    }
}
