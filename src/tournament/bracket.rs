//! Single-elimination bracket generation.
//!
//! Pure: no database, no Discord. A bracket is a function of the number of entrants
//! and the per-round match lengths, so the parts that are easy to get quietly wrong
//! — the seed order, where byes land, which set feeds which — are testable on their
//! own.

use crate::locale::Locale;
use std::fmt::{Display, Formatter};

/// Which of a set's two slots a winner lands in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Slot {
    One,
    Two,
}

/// Where a set's winner or loser goes next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Advancement {
    pub(crate) round: usize,
    pub(crate) position: usize,
    pub(crate) slot: Slot,
}

/// The 3rd place round's name, shared by generation, localization and every
/// presentational branch that needs to recognize it — one literal so they can't drift.
pub(crate) const THIRD_PLACE: &str = "Third Place";

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Set {
    /// 1-based, top to bottom within the round.
    pub(crate) position: usize,
    /// Seeds, where known. Only round one has them: later rounds are filled in as
    /// results land, and an absent seed in round one is what leaves a bye.
    pub(crate) slot1: Option<u32>,
    pub(crate) slot2: Option<u32>,
    /// `None` only on the final and the 3rd place match — neither sends its winner
    /// anywhere else.
    pub(crate) winner_advances_to: Option<Advancement>,
    /// `Some` only on the two semifinal sets, pointing at the 3rd place match: single
    /// elimination otherwise has nothing for a loser to do.
    pub(crate) loser_advances_to: Option<Advancement>,
}

impl Set {
    /// One entrant and no opponent, so it auto-advances when the bracket opens.
    ///
    /// A set in a later round has neither slot filled yet, which is not a bye — hence
    /// comparing the two rather than counting how many are empty.
    pub(crate) fn is_bye(&self) -> bool {
        self.slot1.is_some() != self.slot2.is_some()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Round {
    /// 1-based; round one is the one entrants are seeded into.
    pub(crate) ordinal: usize,
    pub(crate) name: String,
    pub(crate) best_of: u8,
    pub(crate) sets: Vec<Set>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Bracket {
    pub(crate) rounds: Vec<Round>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BracketError {
    /// A bracket needs two sides.
    TooFewEntrants(usize),
    /// One `best_of` per round, no more and no fewer — match length is stored per
    /// round rather than per tournament, so there is no sensible default to fill in.
    RoundCountMismatch { rounds: usize, best_of: usize },
}

impl Display for BracketError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewEntrants(entrants) => {
                write!(f, "a bracket needs at least 2 entrants, got {entrants}")
            },
            Self::RoundCountMismatch { rounds, best_of } => {
                write!(f, "this bracket has {rounds} rounds but {best_of} best_of values")
            },
        }
    }
}

impl std::error::Error for BracketError {}

/// The bracket a field of `entrants` is played in: the next power of two, so the
/// shortfall becomes byes.
pub(crate) fn size(entrants: usize) -> usize {
    entrants.next_power_of_two()
}

/// `log2(size)`, exact because `size` is a power of two.
pub(crate) fn round_count(size: usize) -> usize {
    size.trailing_zeros() as usize
}

/// Seed order for round one, by reflection: start with `[1, 2]` and, to double from
/// size `s` to `2s`, replace every entry `x` with `[x, 2s + 1 - x]`.
///
/// Size 8 gives `[1, 8, 4, 5, 2, 7, 3, 6]`, so round one is `(1,8) (4,5) (2,7) (3,6)`:
/// every seed meets its mirror, and no two of the top four can meet before the
/// semi-finals.
pub(crate) fn seed_order(size: usize) -> Vec<u32> {
    let mut order = vec![1, 2];
    while order.len() < size {
        let mirror = (order.len() * 2 + 1) as u32;
        order = order.into_iter().flat_map(|seed| [seed, mirror - seed]).collect();
    }
    order
}

/// Build the whole bracket from a finalized field.
///
/// `entrants` is a count, not a list, because seeds are required to be 1..=n and
/// contiguous before generation runs — this returns seeds and the caller maps
/// them back to entrants. `best_of` carries one value per round, outermost first —
/// the appended 3rd place round, when one exists, reuses the semifinal's own value
/// rather than requiring an extra entry.
pub(crate) fn build(entrants: usize, best_of: &[u8]) -> Result<Bracket, BracketError> {
    if entrants < 2 {
        return Err(BracketError::TooFewEntrants(entrants));
    }

    let size = size(entrants);
    let round_count = round_count(size);
    if best_of.len() != round_count {
        return Err(BracketError::RoundCountMismatch {
            rounds: round_count,
            best_of: best_of.len(),
        });
    }

    let order = seed_order(size);
    // A seed above the field size was never filled, which is what leaves its
    // opponent unopposed. Reflection puts those against the top seeds.
    let seated = |seed: u32| (seed as usize <= entrants).then_some(seed);

    let mut rounds: Vec<Round> = (1..=round_count)
        .map(|ordinal| {
            let set_count = size >> ordinal;
            let last = ordinal == round_count;

            let sets = (1..=set_count)
                .map(|position| {
                    let (slot1, slot2) = if ordinal == 1 {
                        let left = (position - 1) * 2;
                        (seated(order[left]), seated(order[left + 1]))
                    } else {
                        (None, None)
                    };

                    Set {
                        position,
                        slot1,
                        slot2,
                        // Position p feeds ceil(p / 2) in the next round, taking slot
                        // one when p is odd and slot two when it is even.
                        winner_advances_to: (!last).then(|| Advancement {
                            round: ordinal + 1,
                            position: position.div_ceil(2),
                            slot: if position % 2 == 1 { Slot::One } else { Slot::Two },
                        }),
                        loser_advances_to: None,
                    }
                })
                .collect();

            Round {
                ordinal,
                name: round_name(set_count, last),
                best_of: best_of[ordinal - 1],
                sets,
            }
        })
        .collect();

    // The 3rd place match: the two semifinal losers play each other, using the
    // semifinal's own best_of. `round_count >= 2` is checked before indexing back
    // into `rounds` — with only one round there is no semifinal to take it from.
    // Skipped when a semifinal set is a bye, since a bye leaves only one loser to
    // fill the match (only possible when round_count == 2, i.e. 3 entrants — for
    // round_count >= 3 every semifinal set always fills both slots).
    if round_count >= 2 {
        let semifinal_ordinal = round_count - 1;
        let semifinal = &mut rounds[semifinal_ordinal - 1];
        if !semifinal.sets[0].is_bye() && !semifinal.sets[1].is_bye() {
            let third_place_ordinal = round_count + 1;
            let semifinal_best_of = semifinal.best_of;
            semifinal.sets[0].loser_advances_to = Some(Advancement {
                round: third_place_ordinal,
                position: 1,
                slot: Slot::One,
            });
            semifinal.sets[1].loser_advances_to = Some(Advancement {
                round: third_place_ordinal,
                position: 1,
                slot: Slot::Two,
            });

            rounds.push(Round {
                ordinal: third_place_ordinal,
                name: THIRD_PLACE.to_owned(),
                best_of: semifinal_best_of,
                sets: vec![Set {
                    position: 1,
                    slot1: None,
                    slot2: None,
                    winner_advances_to: None,
                    loser_advances_to: None,
                }],
            });
        }
    }

    Ok(Bracket { rounds })
}

/// Also used by the setup panel to name a preset's scope, so the two agree.
///
/// Always English: this is what goes into `tournament_rounds.name`, so it stays one
/// canonical value in the database. `localize_round_name` renders it for a reader.
pub(crate) fn round_name(set_count: usize, last: bool) -> String {
    if last {
        return "Final".to_owned();
    }
    match set_count * 2 {
        4 => "Semifinal".to_owned(),
        8 => "Quarterfinal".to_owned(),
        entrants => format!("Ro{entrants}"),
    }
}

/// A round's name as a reader sees it. Only the closing three have Chinese names
/// worth giving them; `RoX` is already language-neutral and stays as it is.
///
/// Maps the stored English name rather than taking a depth, so every surface holding
/// a `tournament_rounds.name` can use it without knowing the bracket's shape.
pub(crate) fn localize_round_name(name: &str, locale: Locale) -> String {
    match (locale, name) {
        (Locale::ZhTw, "Final") => "決賽".to_owned(),
        (Locale::ZhTw, "Semifinal") => "準決賽".to_owned(),
        (Locale::ZhTw, "Quarterfinal") => "八強".to_owned(),
        (Locale::ZhTw, THIRD_PLACE) => "季軍賽".to_owned(),
        _ => name.to_owned(),
    }
}

/// `決賽 / Final`, for a surface with many readers and so no one reader's locale to
/// follow.
///
/// Collapsed to a single name when the two languages coincide: `Ro16 / Ro16` is
/// noise rather than bilingualism.
pub(crate) fn round_name_bilingual(name: &str) -> String {
    let zh = localize_round_name(name, Locale::ZhTw);
    if zh == name { zh } else { format!("{zh} / {name}") }
}

#[cfg(test)]
mod tests {
    use super::{Advancement, Bracket, BracketError, Slot, THIRD_PLACE, build, round_count, seed_order, size};

    /// Round one as seed pairs, `None` for an absent seed.
    fn pairings(bracket: &Bracket) -> Vec<(Option<u32>, Option<u32>)> {
        bracket.rounds[0]
            .sets
            .iter()
            .map(|set| (set.slot1, set.slot2))
            .collect()
    }

    fn bo3(rounds: usize) -> Vec<u8> {
        vec![3; rounds]
    }

    fn bracket_for(entrants: usize) -> Bracket {
        let rounds = round_count(size(entrants));
        build(entrants, &bo3(rounds)).expect("a valid field")
    }

    /// The round in which two round-one seeds could first meet. Two entrants meet
    /// once their positions fall in the same block, and each round halves the blocks.
    fn first_meeting_round(entrants: usize, a: u32, b: u32) -> usize {
        let order = seed_order(size(entrants));
        let index = |seed: u32| order.iter().position(|&s| s == seed).expect("seed is in the bracket");
        let (mut left, mut right) = (index(a), index(b));

        let mut round = 1;
        while left != right {
            left >>= 1;
            right >>= 1;
            round += 1;
        }
        round
    }

    #[test]
    fn bracket_size_rounds_up_to_a_power_of_two() {
        assert_eq!(size(2), 2);
        assert_eq!(size(3), 4);
        assert_eq!(size(6), 8);
        assert_eq!(size(16), 16);
        assert_eq!(size(17), 32);
    }

    #[test]
    fn round_count_is_log2_of_the_size() {
        assert_eq!(round_count(2), 1);
        assert_eq!(round_count(4), 2);
        assert_eq!(round_count(8), 3);
        assert_eq!(round_count(16), 4);
    }

    #[test]
    fn seed_order_reflects() {
        assert_eq!(seed_order(2), vec![1, 2]);
        assert_eq!(seed_order(4), vec![1, 4, 2, 3]);
        assert_eq!(seed_order(8), vec![1, 8, 4, 5, 2, 7, 3, 6]);
        assert_eq!(
            seed_order(16),
            vec![1, 16, 8, 9, 4, 13, 5, 12, 2, 15, 7, 10, 3, 14, 6, 11]
        );
    }

    #[test]
    fn full_fields_pair_every_seed_with_its_mirror() {
        assert_eq!(pairings(&bracket_for(2)), vec![(Some(1), Some(2))]);
        assert_eq!(pairings(&bracket_for(4)), vec![(Some(1), Some(4)), (Some(2), Some(3))]);
        assert_eq!(
            pairings(&bracket_for(8)),
            vec![
                (Some(1), Some(8)),
                (Some(4), Some(5)),
                (Some(2), Some(7)),
                (Some(3), Some(6)),
            ]
        );

        // Every seed appears exactly once, and every pair sums to size + 1.
        let sixteen = bracket_for(16);
        let mut seen: Vec<u32> = pairings(&sixteen)
            .iter()
            .flat_map(|&(a, b)| [a.unwrap(), b.unwrap()])
            .collect();
        for (a, b) in pairings(&sixteen) {
            assert_eq!(a.unwrap() + b.unwrap(), 17);
        }
        seen.sort_unstable();
        assert_eq!(seen, (1..=16).collect::<Vec<u32>>());
    }

    #[test]
    fn a_short_field_gives_byes_to_the_top_seeds() {
        // 3 in a bracket of 4: seed 4 is missing, so seed 1 is unopposed.
        assert_eq!(pairings(&bracket_for(3)), vec![(Some(1), None), (Some(2), Some(3))]);

        // 6 in a bracket of 8: seeds 7 and 8 are missing, so 1 and 2 are unopposed.
        let six = bracket_for(6);
        assert_eq!(
            pairings(&six),
            vec![(Some(1), None), (Some(4), Some(5)), (Some(2), None), (Some(3), Some(6)),]
        );

        let byes: Vec<usize> = six.rounds[0]
            .sets
            .iter()
            .filter(|set| set.is_bye())
            .map(|set| set.position)
            .collect();
        assert_eq!(byes, vec![1, 3], "only the sets missing an opponent are byes");

        // A contested set is not a bye, and neither is an empty later-round set.
        assert!(!six.rounds[0].sets[1].is_bye());
        assert!(!six.rounds[1].sets[0].is_bye());
    }

    #[test]
    fn one_over_a_power_of_two_is_a_single_play_in() {
        // 9 entrants in a bracket of 16. Seven seeds are missing, so seven of the eight
        // round-one sets are byes and the only contested one is between the two lowest
        // seeds — whose winner then meets seed 1. That is what a play-in is, and it
        // falls out of reflection rather than being a special case.
        let bracket = bracket_for(9);
        let first = &bracket.rounds[0];

        assert_eq!(
            pairings(&bracket),
            vec![
                (Some(1), None),
                (Some(8), Some(9)),
                (Some(4), None),
                (Some(5), None),
                (Some(2), None),
                (Some(7), None),
                (Some(3), None),
                (Some(6), None),
            ]
        );

        let contested: Vec<usize> = first
            .sets
            .iter()
            .filter(|set| !set.is_bye())
            .map(|set| set.position)
            .collect();
        assert_eq!(contested, vec![2], "only the 8-versus-9 set is played");
        assert_eq!(first.sets.iter().filter(|set| set.is_bye()).count(), 7);

        // Seed 1's bye and the play-in feed the two slots of one round-two set, so the
        // top seed's first opponent is whoever survives it.
        assert_eq!(
            first.sets[0].winner_advances_to,
            Some(Advancement {
                round: 2,
                position: 1,
                slot: Slot::One
            })
        );
        assert_eq!(
            first.sets[1].winner_advances_to,
            Some(Advancement {
                round: 2,
                position: 1,
                slot: Slot::Two
            })
        );

        // Round two is the real field: eight players, then four, then the final —
        // plus the 3rd place match, since round two's semifinal has no bye.
        assert_eq!(bracket.rounds.len(), 5);
        let set_counts: Vec<usize> = bracket.rounds.iter().map(|round| round.sets.len()).collect();
        assert_eq!(set_counts, vec![8, 4, 2, 1, 1]);
    }

    #[test]
    fn no_two_of_the_top_four_seeds_meet_before_the_semi_finals() {
        for entrants in [8, 16, 32] {
            let semi_final = round_count(size(entrants)) - 1;
            for a in 1..=4 {
                for b in (a + 1)..=4 {
                    let round = first_meeting_round(entrants, a, b);
                    assert!(
                        round >= semi_final,
                        "seeds {a} and {b} could meet in round {round} of a {entrants}-player bracket, \
                         before the semi-final in round {semi_final}"
                    );
                }
            }
        }
    }

    #[test]
    fn advancement_links_form_one_rooted_tree() {
        let bracket = bracket_for(16);
        // The 3rd place match sits last and is fed by losers, not winners, so it is
        // excluded from the winner-advancement tree checked below and verified
        // separately.
        let tree_rounds = &bracket.rounds[..bracket.rounds.len() - 1];
        let rounds = tree_rounds.len();

        // Exactly one root in the winner tree: the final, which advances nowhere.
        let rootless: Vec<&super::Set> = tree_rounds
            .iter()
            .flat_map(|round| &round.sets)
            .filter(|set| set.winner_advances_to.is_none())
            .collect();
        assert_eq!(rootless.len(), 1);
        assert_eq!(rootless[0].position, 1);
        assert_eq!(tree_rounds[rounds - 1].sets.len(), 1);

        // And every slot of every later set is fed exactly once.
        for round in 2..=rounds {
            let mut fed: Vec<(usize, Slot)> = tree_rounds[round - 2]
                .sets
                .iter()
                .map(|set| {
                    let target = set.winner_advances_to.expect("only the final has no winner target");
                    assert_eq!(target.round, round, "a winner may only advance one round");
                    (target.position, target.slot)
                })
                .collect();

            let mut expected: Vec<(usize, Slot)> = (1..=tree_rounds[round - 1].sets.len())
                .flat_map(|position| [(position, Slot::One), (position, Slot::Two)])
                .collect();

            fed.sort_by_key(|&(position, slot)| (position, slot == Slot::Two));
            expected.sort_by_key(|&(position, slot)| (position, slot == Slot::Two));
            assert_eq!(fed, expected, "round {round} is not fed exactly once per slot");
        }

        // The 3rd place match is the tree's other rootless set — fed by losers
        // rather than winners — and both semifinal losers feed it, into slots one
        // and two respectively.
        let third_place = bracket.rounds.last().expect("a 16-entrant field has one");
        assert_eq!(third_place.name, super::THIRD_PLACE);
        assert!(third_place.sets[0].winner_advances_to.is_none());

        let semifinal = &tree_rounds[rounds - 2];
        assert_eq!(
            semifinal.sets[0].loser_advances_to,
            Some(Advancement {
                round: rounds + 1,
                position: 1,
                slot: Slot::One
            })
        );
        assert_eq!(
            semifinal.sets[1].loser_advances_to,
            Some(Advancement {
                round: rounds + 1,
                position: 1,
                slot: Slot::Two
            })
        );
    }

    #[test]
    fn the_first_two_sets_feed_the_slots_of_the_first_set_above_them() {
        let sets = &bracket_for(8).rounds[0].sets;

        assert_eq!(
            sets[0].winner_advances_to,
            Some(Advancement {
                round: 2,
                position: 1,
                slot: Slot::One
            })
        );
        assert_eq!(
            sets[1].winner_advances_to,
            Some(Advancement {
                round: 2,
                position: 1,
                slot: Slot::Two
            })
        );
        assert_eq!(
            sets[2].winner_advances_to,
            Some(Advancement {
                round: 2,
                position: 2,
                slot: Slot::One
            })
        );
    }

    #[test]
    fn a_single_elimination_bracket_has_one_set_fewer_than_its_size_unless_a_third_place_match_exists() {
        // A 3rd place match adds the set back. Absent for 2 (no semifinal to take a
        // loser from) and 3 (the semifinal itself is a bye); present everywhere else.
        for (entrants, has_third_place) in [(2, false), (3, false), (4, true), (6, true), (8, true), (16, true)] {
            let bracket = bracket_for(entrants);
            let sets: usize = bracket.rounds.iter().map(|round| round.sets.len()).sum();
            let expected = if has_third_place {
                size(entrants)
            } else {
                size(entrants) - 1
            };
            assert_eq!(sets, expected, "for {entrants} entrants");
        }
    }

    #[test]
    fn best_of_is_per_round() {
        // The case per-round `best_of` exists for: a Bo3 bracket with a Bo5 final.
        // The 3rd place match takes the semifinal's own value (3) rather than an
        // extra entry in the input slice.
        let bracket = build(8, &[3, 3, 5]).expect("a valid field");

        let lengths: Vec<u8> = bracket.rounds.iter().map(|round| round.best_of).collect();
        assert_eq!(lengths, vec![3, 3, 5, 3]);
    }

    #[test]
    fn rounds_are_named_from_the_end() {
        let names = |entrants: usize| -> Vec<String> {
            bracket_for(entrants)
                .rounds
                .into_iter()
                .map(|round| round.name)
                .collect()
        };

        // No semifinal to take a loser from.
        assert_eq!(names(2), vec!["Final"]);
        // The semifinal itself is a bye, so there's only one loser — no match for them.
        assert_eq!(names(3), vec!["Semifinal", "Final"]);
        assert_eq!(names(6), vec!["Quarterfinal", "Semifinal", "Final", "Third Place"]);
        assert_eq!(
            names(16),
            vec!["Ro16", "Quarterfinal", "Semifinal", "Final", "Third Place"]
        );
        assert_eq!(
            names(17),
            vec!["Ro32", "Ro16", "Quarterfinal", "Semifinal", "Final", "Third Place"]
        );
    }

    #[test]
    fn the_closing_rounds_have_chinese_names_and_the_rest_keep_ro_x() {
        use super::localize_round_name;
        use crate::locale::Locale;

        assert_eq!(localize_round_name("Final", Locale::ZhTw), "決賽");
        assert_eq!(localize_round_name("Semifinal", Locale::ZhTw), "準決賽");
        assert_eq!(localize_round_name("Quarterfinal", Locale::ZhTw), "八強");
        assert_eq!(localize_round_name("Ro16", Locale::ZhTw), "Ro16", "already neutral");
        assert_eq!(localize_round_name("Final", Locale::En), "Final");
    }

    #[test]
    fn a_shared_surface_names_a_round_in_both_languages() {
        use super::round_name_bilingual;

        assert_eq!(round_name_bilingual("Final"), "決賽 / Final");
        assert_eq!(round_name_bilingual("Semifinal"), "準決賽 / Semifinal");
        assert_eq!(round_name_bilingual("Quarterfinal"), "八強 / Quarterfinal");
        // Collapsed, not doubled: `Ro16 / Ro16` is noise, not bilingualism.
        assert_eq!(round_name_bilingual("Ro16"), "Ro16");
        assert_eq!(round_name_bilingual("Ro32"), "Ro32");
    }

    #[test]
    fn every_name_the_bracket_produces_is_translated_or_deliberately_not() {
        // The drift guard: renaming a round in `round_name` without teaching
        // `localize_round_name` about it would otherwise silently ship English into
        // a Chinese line, which is exactly what this pair exists to prevent.
        use super::localize_round_name;
        use crate::locale::Locale;

        for entrants in 2..=64 {
            for round in bracket_for(entrants).rounds {
                let zh = localize_round_name(&round.name, Locale::ZhTw);
                assert!(
                    !zh.is_ascii() || round.name.starts_with("Ro"),
                    "{} is neither translated nor a RoX form",
                    round.name
                );
            }
        }
    }

    #[test]
    fn a_field_too_small_to_play_is_rejected() {
        assert_eq!(build(0, &[]), Err(BracketError::TooFewEntrants(0)));
        assert_eq!(build(1, &[3]), Err(BracketError::TooFewEntrants(1)));
    }

    #[test]
    fn a_best_of_per_round_is_required() {
        // 8 entrants is three rounds; two lengths cannot describe it.
        assert_eq!(
            build(8, &[3, 3]),
            Err(BracketError::RoundCountMismatch { rounds: 3, best_of: 2 })
        );
        assert_eq!(
            build(8, &[]),
            Err(BracketError::RoundCountMismatch { rounds: 3, best_of: 0 })
        );
    }

    // The 3rd place match.

    #[test]
    fn a_third_place_match_is_absent_with_no_semifinal_or_a_bye_one() {
        // 2 entrants: only a final, no semifinal to take a loser from.
        assert!(!bracket_for(2).rounds.iter().any(|round| round.name == THIRD_PLACE));
        // 3 entrants: the semifinal is itself a bye, leaving only one loser.
        let three = bracket_for(3);
        assert!(!three.rounds.iter().any(|round| round.name == THIRD_PLACE));
        assert!(three.rounds[0].sets.iter().any(|set| set.is_bye()));
    }

    #[test]
    fn a_third_place_match_exists_whenever_the_semifinal_has_two_real_losers() {
        for entrants in [4, 6, 8, 16] {
            let bracket = bracket_for(entrants);
            let third_place = bracket
                .rounds
                .iter()
                .find(|round| round.name == THIRD_PLACE)
                .unwrap_or_else(|| panic!("expected a 3rd place match for {entrants} entrants"));
            assert_eq!(third_place.sets.len(), 1);
            assert_eq!(third_place.sets[0].position, 1);
            assert!(third_place.sets[0].slot1.is_none());
            assert!(third_place.sets[0].slot2.is_none());
            assert!(third_place.sets[0].winner_advances_to.is_none());

            // Its best_of is the semifinal's own, not a new configuration surface.
            let semifinal = bracket.rounds[bracket.rounds.len() - 3].clone();
            assert_eq!(third_place.best_of, semifinal.best_of, "for {entrants} entrants");

            // Both semifinal losers feed it, into slots one and two respectively.
            let third_place_ordinal = bracket.rounds.len();
            assert_eq!(
                semifinal.sets[0].loser_advances_to,
                Some(Advancement {
                    round: third_place_ordinal,
                    position: 1,
                    slot: Slot::One
                }),
                "for {entrants} entrants"
            );
            assert_eq!(
                semifinal.sets[1].loser_advances_to,
                Some(Advancement {
                    round: third_place_ordinal,
                    position: 1,
                    slot: Slot::Two
                }),
                "for {entrants} entrants"
            );
        }
    }
}
