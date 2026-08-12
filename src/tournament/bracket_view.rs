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
use crate::tournament::panel_check;
use crate::tournament::registration::RegistrationState;
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
///
/// The two branches guard against a real risk of confusing two different kinds
/// of gap with each other:
/// - **Once any real `seed` exists** (close has happened), a gap in the stored
///   seeds is a withdrawal's, and is never reused: seeds are shown as stored,
///   and a latecomer without one continues past the highest rather than
///   filling in behind. Renumbering by position would contradict the panel.
/// - **Before that**, nobody has a real `seed` yet, so the only numbers in use
///   at all are pins (`manual_seed`) — a gap below one is simply not filled
///   in yet, not a vacated slot, and the next unpinned entrant takes the
///   lowest one nobody has claimed.
fn draw_order(entries: &[TournamentEntry]) -> Vec<render::Entrant> {
    let field = seeding::display_order(entries);
    let closed = field.iter().any(|e| e.seed.is_some());

    if closed {
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
    } else {
        let taken: std::collections::HashSet<i64> = field.iter().filter_map(|e| e.manual_seed).collect();
        let mut next_free = 1i64;
        field
            .iter()
            .filter_map(|e| {
                let seed = e.manual_seed.unwrap_or_else(|| {
                    while taken.contains(&next_free) {
                        next_free += 1;
                    }
                    let seed = next_free;
                    next_free += 1;
                    seed
                });
                Some(render::Entrant {
                    seed: u32::try_from(seed).ok()?,
                    name: e.display_name.clone(),
                })
            })
            .collect()
    }
}

/// The seats an invite-only field has not filled yet, as drawable entrants.
///
/// Without them `bracket::build` leaves those positions empty and `render::leaf`
/// draws them `(bye)` — which is right for a seed advancing free and wrong for a
/// seat waiting on an invite. The two mean opposite things and look identical, so
/// the vacancies are named instead of inferred.
///
/// A placeholder takes the lowest number nobody holds, which makes the drawing a
/// list of the seeds still to be filled. Real seeds may already have gaps in them
/// (a withdrawal leaves 1, 2, 4, 5), and this is the same rule read from the
/// other side. A pin ahead of the current field is never offered here either —
/// `draw_order` already gives it its own number before this runs, so it shows
/// up in `taken` like any other claimed seat.
fn pad_with_open_seats(order: &mut Vec<render::Entrant>, target: usize) {
    let taken: Vec<u32> = order.iter().map(|e| e.seed).collect();
    let mut seat = 1;
    while order.len() < target {
        while taken.contains(&seat) {
            seat += 1;
        }
        order.push(render::Entrant {
            seed: seat,
            // Wrapped, not bare: `seed4` alone reads as somebody's name.
            name: format!("<seed{seat}>"),
        });
        seat += 1;
    }
}

/// The provisional bracket implied by the current field.
///
/// `open_seats_to` draws the whole target bracket rather than one sized to who is
/// in it so far: an organizer filling eight seats should be looking at eight, not
/// at a four-bracket that will reshape twice more before it is done. `None` keeps
/// the field's own size, which is what an open registration wants — it genuinely
/// does not know how big it will end up.
///
/// `None` below two entrants, where `bracket::build` correctly refuses. Padding
/// happens first, so a target of its own is enough to draw from an empty field.
pub(crate) fn preview_rounds(entries: &[TournamentEntry], open_seats_to: Option<usize>) -> Option<Vec<render::Round>> {
    let mut order = draw_order(entries);
    if let Some(target) = open_seats_to {
        pad_with_open_seats(&mut order, target);
    }
    if order.len() < MIN_ENTRANTS {
        return None;
    }

    // Sized from the order rather than the target, so a cap lowered under a field
    // that already exists grows the bracket to hold everyone instead of dropping
    // whoever no longer fits.
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

/// Which drawing this is, which is what the heading above it has to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drawing {
    /// Computed from the current field, before `start` writes a real one.
    Preview,
    /// A preview padded out to the target field, so the empty slots are seats
    /// still to be invited rather than byes.
    PreviewWithOpenSeats,
    /// The bracket `start` wrote.
    Real,
}

/// Wraps the drawing with a heading, and says plainly that it is not the draw
/// yet while the tournament has not started. Bilingual: one shared message with
/// many readers.
fn decorate(name: &str, chunks: Vec<String>, drawing: Drawing) -> Vec<String> {
    // `<seed4>` in a bracket still needs the heading to say what it means.
    let open_seats = if drawing == Drawing::PreviewWithOpenSeats {
        "尚未邀請的空位標示為 `<seedN>`。\n\
         Seats still to be invited are shown as `<seedN>`.\n"
    } else {
        ""
    };
    let heading = if drawing == Drawing::Real {
        format!("**{name} — 賽程表 / bracket**\n")
    } else {
        format!(
            "**{name} — 賽程表預覽 / bracket preview**\n\
             這是依目前報名者推算的暫定賽程表，開賽前都可能變動。\n\
             Provisional, based on who has registered so far; it will change until the event starts.\n\
             {open_seats}"
        )
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

    // Open seats only while the organizers can still fill them. Once check-in
    // opens the field is settled, and a vacancy nobody can take would be a lie.
    let open_seats_to = (RegistrationState::of(tournament) == RegistrationState::InviteOnly)
        .then(|| bracket::size(usize::try_from(tournament.entrant_cap).unwrap_or(MIN_ENTRANTS)));

    // Whether this is still a preview is the same question as which drawing we
    // have, so it is answered once rather than read off the status separately.
    let (rounds, drawing) = match persisted_rounds(pool, tournament.id, &entries).await? {
        Some(rounds) => (rounds, Drawing::Real),
        None => match preview_rounds(&entries, open_seats_to) {
            Some(rounds) if open_seats_to.is_some() => (rounds, Drawing::PreviewWithOpenSeats),
            Some(rounds) => (rounds, Drawing::Preview),
            None => return Ok(ReconcileOutcome::TooFewEntrants),
        },
    };
    let chunks = decorate(
        &tournament.name,
        render::render(&rounds, render::DEFAULT_WIDTH),
        drawing,
    );

    let (mut posted, mut edited, mut deleted) = (0, 0, 0);
    let existing = db::list_bracket_messages(pool, tournament.id).await?;
    for (index, chunk) in chunks.iter().enumerate() {
        let ordinal = i64::try_from(index).unwrap();

        // A stored id that no longer resolves is treated as never having been
        // posted rather than a reason to abort the whole redraw.
        let needs_post = match existing.iter().find(|m| m.ordinal == ordinal) {
            Some(message) => {
                let message_id = to_message_id(message.message_id);
                match channel_id
                    .edit_message(&http, message_id, EditMessage::new().content(chunk))
                    .await
                {
                    Ok(_) => {
                        edited += 1;
                        false
                    },
                    Err(err) if panel_check::is_confirmed_missing(&err) => true,
                    Err(err) => return Err(err.into()),
                }
            },
            None => true,
        };

        if needs_post {
            let message = channel_id
                .send_message(&http, CreateMessage::new().content(chunk))
                .await?;
            // Only the message carrying the heading — jumping to it via the
            // pin and scrolling down reaches every chunk after it. Runs on
            // whichever message a fresh post lands on, including a repost
            // after an admin deletes it, so this self-heals.
            if ordinal == 0
                && let Err(err) = message.pin(&http).await
            {
                tracing::error!(
                    "failed to pin the bracket message for tournament {}: {err:?}",
                    tournament.id
                );
            }
            db::upsert_bracket_message(pool, tournament.id, ordinal, to_db_id(message.id)).await?;
            posted += 1;
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
            invited_by: None,
            seed: None,
            suggested_seed: None,
            manual_seed: None,
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
        assert!(preview_rounds(&[], None).is_none());
        assert!(preview_rounds(&field(1), None).is_none());
    }

    #[test]
    fn open_seats_draw_the_bracket_being_filled_rather_than_the_one_filled_so_far() {
        // Three of eight invited. Without padding this is a 4-bracket that will
        // reshape twice more; with it the organizer sees the eight seats they
        // are actually filling.
        let rounds = preview_rounds(&field(3), Some(8)).unwrap();
        assert_eq!(rounds.len(), 3, "an 8-bracket is three rounds");
        assert_eq!(rounds[0].matches.len(), 4);

        let names: Vec<&str> = drawn(&rounds).into_iter().map(|e| e.name.as_str()).collect();
        for seat in ["<seed4>", "<seed5>", "<seed6>", "<seed7>", "<seed8>"] {
            assert!(names.contains(&seat), "{seat} missing from {names:?}");
        }

        // The point of naming them: a padded round one has no empty slot left,
        // so nothing in the drawing reads `(bye)` when it means "not invited yet".
        let drawing = render::render(&rounds, render::DEFAULT_WIDTH).join("\n");
        assert!(!drawing.contains("(bye)"), "{drawing}");
        assert!(drawing.contains("<seed8>"), "{drawing}");
    }

    #[test]
    fn an_open_seat_takes_the_lowest_number_nobody_holds() {
        // Seeds 1, 2 and 5 are taken, so the seats still to fill are 3, 4, 6, 7
        // and 8 — which is exactly the list an organizer has left to invite.
        let entries = vec![
            with_seed(entry(1, "A", Some(2000)), 1),
            with_seed(entry(2, "B", Some(1900)), 2),
            with_seed(entry(3, "C", Some(1800)), 5),
        ];
        let rounds = preview_rounds(&entries, Some(8)).unwrap();
        let mut seeds: Vec<u32> = drawn(&rounds).into_iter().map(|e| e.seed).collect();
        seeds.sort_unstable();
        assert_eq!(seeds, vec![1, 2, 3, 4, 5, 6, 7, 8]);

        let open: Vec<u32> = drawn(&rounds)
            .into_iter()
            .filter(|e| e.name.starts_with('<'))
            .map(|e| e.seed)
            .collect();
        let mut open = open;
        open.sort_unstable();
        assert_eq!(open, vec![3, 4, 6, 7, 8]);
    }

    #[test]
    fn an_invite_only_bracket_is_drawn_before_anyone_is_in_it() {
        // The whole target field, every seat open. This is what an organizer sees
        // the moment they mark an event invite-only, and it fills in from there.
        let rounds = preview_rounds(&[], Some(8)).unwrap();
        assert_eq!(rounds.len(), 3);
        let names: Vec<&str> = drawn(&rounds).into_iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 8);
        assert!(
            names.iter().all(|n| n.starts_with('<') && n.ends_with('>')),
            "{names:?}"
        );
    }

    #[test]
    fn a_full_field_needs_no_open_seats() {
        let rounds = preview_rounds(&field(8), Some(8)).unwrap();
        let names: Vec<&str> = drawn(&rounds).into_iter().map(|e| e.name.as_str()).collect();
        assert!(!names.iter().any(|n| n.starts_with('<')), "{names:?}");
    }

    #[test]
    fn a_field_larger_than_its_cap_grows_the_bracket_rather_than_dropping_anyone() {
        // Reachable only by lowering the cap under a field that already exists.
        // Nobody may vanish from the drawing over it.
        let rounds = preview_rounds(&field(5), Some(4)).unwrap();
        assert_eq!(rounds.len(), 3, "five entrants still need an 8-bracket");
        assert_eq!(drawn(&rounds).len(), 5);
    }

    #[test]
    fn two_entrants_are_a_single_final() {
        let rounds = preview_rounds(&field(2), None).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].name, "Final");
        assert_eq!(rounds[0].matches.len(), 1);
    }

    #[test]
    fn a_field_that_is_not_a_power_of_two_gets_byes_on_the_top_seeds() {
        // 5 entrants play an 8-bracket, so seeds 1, 2 and 3 are unopposed.
        let rounds = preview_rounds(&field(5), None).unwrap();
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
        let rounds = preview_rounds(&entries, None).unwrap();
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
        let rounds = preview_rounds(&entries, None).unwrap();
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
        let rounds = preview_rounds(&entries, None).unwrap();
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
        let rounds = preview_rounds(&entries, None).unwrap();
        let placed: Vec<(u32, &str)> = drawn(&rounds).into_iter().map(|e| (e.seed, e.name.as_str())).collect();

        // Seed 1 stands; the unseeded pair follow it in rating order.
        assert!(placed.contains(&(1, "Weak")), "{placed:?}");
        assert!(placed.contains(&(2, "Strong")), "{placed:?}");
        assert!(placed.contains(&(3, "Middle")), "{placed:?}");
    }

    fn pinned(mut entry: TournamentEntry, seat: i64) -> TournamentEntry {
        entry.manual_seed = Some(seat);
        entry
    }

    #[test]
    fn a_pin_ahead_of_the_field_is_never_offered_as_an_open_seat() {
        // The exact reported bug: pinning someone to seat 4 with only 3 real
        // entrants and no close yet must show them AT seat 4 immediately —
        // not compacted onto seat 3 while seat 4 is still advertised as open.
        let entries = vec![
            entry(1, "KnockKnock", Some(1765)),
            entry(2, "Deepshine", None),
            pinned(entry(3, "Lun", Some(1680)), 4),
        ];
        let rounds = preview_rounds(&entries, Some(4)).unwrap();
        let placed: Vec<(u32, &str)> = drawn(&rounds).into_iter().map(|e| (e.seed, e.name.as_str())).collect();

        assert!(placed.contains(&(4, "Lun")), "{placed:?}");
        assert!(!placed.contains(&(4, "<seed4>")), "{placed:?}");
        // The seats not yet reached fill from the bottom instead: KnockKnock
        // (unpinned, highest rated) takes 1, Deepshine 2, and the one genuinely
        // open seat is 3 — not 4, which Lun already holds.
        assert!(placed.contains(&(1, "KnockKnock")), "{placed:?}");
        assert!(placed.contains(&(2, "Deepshine")), "{placed:?}");
        assert!(placed.contains(&(3, "<seed3>")), "{placed:?}");
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
        let rounds = preview_rounds(&entries, None).unwrap();
        let mut seeds: Vec<u32> = drawn(&rounds).into_iter().map(|e| e.seed).collect();
        seeds.sort_unstable();
        assert_eq!(seeds, vec![1, 2, 5, 6]);
    }

    #[test]
    fn no_scores_before_anything_is_played() {
        let rounds = preview_rounds(&field(4), None).unwrap();
        assert!(rounds.iter().flat_map(|r| &r.matches).all(|m| m.score.is_none()));
    }

    #[test]
    fn withdrawn_entrants_are_not_in_the_draw() {
        let mut entries = field(4);
        entries[3].status = "withdrawn".to_string();
        let rounds = preview_rounds(&entries, None).unwrap();
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
            panel_message_id: None,
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
        let preview = preview_rounds(&entries, None).unwrap();
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
        let chunks = decorate("Relic Cup", vec!["body".to_string()], Drawing::Preview);
        assert!(chunks[0].contains("賽程表預覽"), "{}", chunks[0]);
        assert!(chunks[0].contains("Provisional"), "{}", chunks[0]);

        let chunks = decorate("Relic Cup", vec!["body".to_string()], Drawing::Real);
        assert!(!chunks[0].contains("Provisional"), "{}", chunks[0]);
    }

    #[test]
    fn a_padded_preview_explains_what_seed_n_means() {
        // Without this a reader takes `<seed4>` for somebody's name.
        let padded = decorate("Relic Cup", vec!["body".to_string()], Drawing::PreviewWithOpenSeats);
        assert!(padded[0].contains("<seedN>"), "{}", padded[0]);
        assert!(padded[0].contains("still to be invited"), "{}", padded[0]);
        assert!(padded[0].contains("尚未邀請的空位"), "{}", padded[0]);
        // Still a preview, so it keeps saying so.
        assert!(padded[0].contains("Provisional"), "{}", padded[0]);

        for drawing in [Drawing::Preview, Drawing::Real] {
            let plain = decorate("Relic Cup", vec!["body".to_string()], drawing);
            assert!(!plain[0].contains("<seedN>"), "{drawing:?}: {}", plain[0]);
        }
    }

    #[test]
    fn only_the_first_chunk_carries_the_heading() {
        let chunks = decorate(
            "Relic Cup",
            vec!["one".to_string(), "two".to_string()],
            Drawing::Preview,
        );
        assert!(chunks[0].contains("Relic Cup"));
        assert_eq!(chunks[1], "two", "a continuation chunk is the drawing alone");
    }

    #[test]
    fn a_large_field_splits_into_several_messages() {
        // The split starts at 16, which is what makes the message count
        // vary with the field and `reconcile` necessary.
        let rounds = preview_rounds(&field(16), None).unwrap();
        let chunks = render::render(&rounds, render::DEFAULT_WIDTH);
        assert!(chunks.len() > 1, "16 entrants should not fit one message");
        assert!(chunks.iter().all(|c| c.len() <= 2000));
    }
}
