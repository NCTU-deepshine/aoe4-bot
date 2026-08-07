//! Single-elimination bracket generation (docs/tournament.md §5).
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

/// Where a set's winner goes. `None` only on the final.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Advancement {
    pub(crate) round: usize,
    pub(crate) position: usize,
    pub(crate) slot: Slot,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Set {
    /// 1-based, top to bottom within the round.
    pub(crate) position: usize,
    /// Seeds, where known. Only round one has them: later rounds are filled in as
    /// results land, and an absent seed in round one is what leaves a bye.
    pub(crate) slot1: Option<u32>,
    pub(crate) slot2: Option<u32>,
    pub(crate) winner_advances_to: Option<Advancement>,
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
    /// One `best_of` per round, no more and no fewer — §4 keeps match length per
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
/// contiguous before generation runs (§8.3) — this returns seeds and the caller maps
/// them back to entrants. `best_of` carries one value per round, outermost first.
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

    let rounds = (1..=round_count)
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
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Advancement, Bracket, BracketError, Slot, build, round_count, seed_order, size};

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

        // Round two is the real field: eight players, then four, then the final.
        assert_eq!(bracket.rounds.len(), 4);
        let set_counts: Vec<usize> = bracket.rounds.iter().map(|round| round.sets.len()).collect();
        assert_eq!(set_counts, vec![8, 4, 2, 1]);
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
        let rounds = bracket.rounds.len();

        // Exactly one root: the final, which advances nowhere.
        let rootless: Vec<&super::Set> = bracket
            .rounds
            .iter()
            .flat_map(|round| &round.sets)
            .filter(|set| set.winner_advances_to.is_none())
            .collect();
        assert_eq!(rootless.len(), 1);
        assert_eq!(rootless[0].position, 1);
        assert_eq!(bracket.rounds[rounds - 1].sets.len(), 1);

        // And every slot of every later set is fed exactly once.
        for round in 2..=rounds {
            let mut fed: Vec<(usize, Slot)> = bracket.rounds[round - 2]
                .sets
                .iter()
                .map(|set| {
                    let target = set.winner_advances_to.expect("only the final has no target");
                    assert_eq!(target.round, round, "a winner may only advance one round");
                    (target.position, target.slot)
                })
                .collect();

            let mut expected: Vec<(usize, Slot)> = (1..=bracket.rounds[round - 1].sets.len())
                .flat_map(|position| [(position, Slot::One), (position, Slot::Two)])
                .collect();

            fed.sort_by_key(|&(position, slot)| (position, slot == Slot::Two));
            expected.sort_by_key(|&(position, slot)| (position, slot == Slot::Two));
            assert_eq!(fed, expected, "round {round} is not fed exactly once per slot");
        }
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
    fn a_single_elimination_bracket_has_one_set_fewer_than_its_size() {
        for entrants in [2, 3, 4, 6, 8, 16] {
            let bracket = bracket_for(entrants);
            let sets: usize = bracket.rounds.iter().map(|round| round.sets.len()).sum();
            assert_eq!(sets, size(entrants) - 1, "for {entrants} entrants");
        }
    }

    #[test]
    fn best_of_is_per_round() {
        // The case §4 exists for: a Bo3 bracket with a Bo5 final.
        let bracket = build(8, &[3, 3, 5]).expect("a valid field");

        let lengths: Vec<u8> = bracket.rounds.iter().map(|round| round.best_of).collect();
        assert_eq!(lengths, vec![3, 3, 5]);
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

        assert_eq!(names(2), vec!["Final"]);
        assert_eq!(names(3), vec!["Semifinal", "Final"]);
        assert_eq!(names(6), vec!["Quarterfinal", "Semifinal", "Final"]);
        assert_eq!(names(16), vec!["Ro16", "Quarterfinal", "Semifinal", "Final"]);
        assert_eq!(names(17), vec!["Ro32", "Ro16", "Quarterfinal", "Semifinal", "Final"]);
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
}
