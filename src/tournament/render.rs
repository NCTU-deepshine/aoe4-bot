//! Rendering a bracket for Discord.
//!
//! Pure: takes rounds, returns message bodies. No database, no Discord, no clock.
//!
//! Four things make this fiddlier than it looks, all of them invisible in the code
//! and obvious in the output:
//!
//! - markdown does not work inside a code fence, so a winner is shown by name on the
//!   connector line rather than by being bolded;
//! - a backtick in a player's name would end the fence early;
//! - CJK names are double-width, so every column is measured in display cells, never
//!   in `chars().count()`;
//! - a message caps at 2000 characters, so a large bracket is split.

use crate::tournament::bracket::Slot;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Discord's per-message limit. Everything a chunk needs — fence, newlines, the
/// champion line — is counted against it.
const MESSAGE_LIMIT: usize = 2000;

/// Default display width of a name cell.
pub(crate) const DEFAULT_WIDTH: usize = 12;

#[derive(Clone)]
pub(crate) struct Entrant {
    pub(crate) seed: u32,
    pub(crate) name: String,
}

pub(crate) struct Match {
    pub(crate) slot1: Option<Entrant>,
    pub(crate) slot2: Option<Entrant>,
    /// Wins for slot one and slot two.
    pub(crate) score: Option<(u8, u8)>,
    pub(crate) winner: Option<Slot>,
}

impl Match {
    fn entrant(&self, slot: Slot) -> Option<&Entrant> {
        match slot {
            Slot::One => self.slot1.as_ref(),
            Slot::Two => self.slot2.as_ref(),
        }
    }

    /// Each slot's games, or `None` apiece while the match has not started. Kept as a
    /// pair so a caller never has to remember which half of the score is whose.
    fn wins(&self) -> (Option<u8>, Option<u8>) {
        match self.score {
            Some((one, two)) => (Some(one), Some(two)),
            None => (None, None),
        }
    }
}

pub(crate) struct Round {
    pub(crate) name: String,
    pub(crate) matches: Vec<Match>,
}

/// Render a bracket into one or more message bodies, each within Discord's limit.
///
/// Splitting halves the bracket and peels the closing round off as its
/// own message, recursing until each part fits. A half of a bracket is just a smaller
/// bracket, which is why this needs no separate code path.
pub(crate) fn render(rounds: &[Round], width: usize) -> Vec<String> {
    if rounds.is_empty() {
        return Vec::new();
    }

    let body = fenced(&grid(rounds, width), &champion_line(rounds));
    if body.len() <= MESSAGE_LIMIT || rounds.len() == 1 {
        // A single round that still does not fit cannot be split any further. Better
        // an over-long body that Discord rejects loudly than a silent truncation.
        return vec![body];
    }

    let (feeders, closing) = rounds.split_at(rounds.len() - 1);
    let half = |take_upper: bool| -> Vec<Round> {
        feeders
            .iter()
            .map(|round| {
                let middle = round.matches.len() / 2;
                let range = if take_upper {
                    0..middle
                } else {
                    middle..round.matches.len()
                };
                Round {
                    name: round.name.clone(),
                    matches: round.matches[range].iter().map(clone_match).collect(),
                }
            })
            .collect()
    };

    let mut messages = render(&half(true), width);
    messages.extend(render(&half(false), width));
    messages.extend(render(closing, width));
    messages
}

/// A round as a plain list, for phones — a 16-player bracket is already wider than a
/// phone's code block.
///
/// This is *outside* a fence, so names go through the markdown escaper rather than the
/// fence-safety pass.
pub(crate) fn render_round_list(round: &Round) -> String {
    let mut out = format!("**{}**\n", crate::ranked::escape(&round.name));

    let seeded = |entrant: Option<&Entrant>| match entrant {
        Some(entrant) => format!("`{}` {}", entrant.seed, crate::ranked::escape(&entrant.name)),
        None => "`?` —".to_owned(),
    };

    for game in &round.matches {
        let score = match game.score {
            Some((a, b)) => format!("{a} – {b}"),
            None => "vs".to_owned(),
        };
        out.push_str(&format!(
            "{}  {score}  {}\n",
            seeded(game.slot1.as_ref()),
            seeded(game.slot2.as_ref())
        ));
    }
    out
}

fn clone_match(game: &Match) -> Match {
    let entrant = |slot: Option<&Entrant>| {
        slot.map(|entrant| Entrant {
            seed: entrant.seed,
            name: entrant.name.clone(),
        })
    };
    Match {
        slot1: entrant(game.slot1.as_ref()),
        slot2: entrant(game.slot2.as_ref()),
        score: game.score,
        winner: game.winner,
    }
}

fn champion(rounds: &[Round]) -> Option<String> {
    let final_match = rounds.last()?.matches.first()?;
    if rounds.last()?.matches.len() != 1 {
        return None;
    }
    // Raw: this lands outside the fence, so `fenced` escapes it as markdown rather
    // than putting it through the fence-safety pass.
    Some(final_match.entrant(final_match.winner?)?.name.clone())
}

/// The trophy line appended once a champion exists, empty otherwise. Outside
/// the fence, so it goes through the markdown escaper rather than the
/// fence-safety pass — and `pub(crate)` so `bracket_view`'s image path can
/// reuse it as plain message content alongside the drawing.
pub(crate) fn champion_line(rounds: &[Round]) -> String {
    match champion(rounds) {
        Some(name) => format!("\n🏆 **{}**", crate::ranked::escape(&name)),
        None => String::new(),
    }
}

fn fenced(lines: &[String], champion_line: &str) -> String {
    format!("```\n{}\n```{champion_line}", lines.join("\n"))
}

/// The bracket itself, one string per line, trailing spaces trimmed.
///
/// Every column holds the **participants** of the match immediately to its right, each
/// with the games they won in it — so a score sits beside the player who earned it, in
/// the round it was played. The rightmost column is therefore the winner of the final,
/// who has no next match and so no score.
///
/// Leaves sit on even rows, which puts each match's own row at the midpoint between its
/// two participants. That is what keeps a connector's `┐`, `│`, `├` and `┘` in one
/// column.
///
/// `pub(crate)` so `bracket_svg` can place an SVG document over the same
/// character matrix rather than re-deriving this layout.
pub(crate) fn grid(rounds: &[Round], width: usize) -> Vec<String> {
    let leaves = rounds[0].matches.len() * 2;
    let mut lines = vec![Line::default(); leaves * 2 - 1];

    // A cell is a name, a space and one digit of score. No match runs to ten wins.
    let cell = width + 2;

    // Column 0: the entrants, each with their games in their opening match.
    let mut connector = cell + 2;
    for (index, game) in rounds[0].matches.iter().enumerate() {
        let top = index * 4;
        let (one, two) = game.wins();
        lines[top].put(0, &participant(&leaf(game.slot1.as_ref()), one, width));
        lines[top].put(cell + 1, "─┐");
        lines[top + 2].put(0, &participant(&leaf(game.slot2.as_ref()), two, width));
        lines[top + 2].put(cell + 1, "─┘");
    }

    // Then one column per round, holding whoever came out of the round before it.
    for depth in 1..=rounds.len() {
        let feeders = &rounds[depth - 1].matches;
        let span = 1 << depth; // leaves under one match of the feeding round
        let last = depth == rounds.len();
        let content = connector + 3;
        let next = content + cell + 2;

        for (index, feeder) in feeders.iter().enumerate() {
            let row = index * span * 2 + span - 1;

            // Whoever won the feeding match, and how they are doing in the match this
            // column feeds. The last column feeds nothing, so it carries no score.
            let advanced = feeder.winner.and_then(|slot| feeder.entrant(slot));
            let name = advanced.map_or_else(|| "?".to_owned(), |entrant| sanitize(&entrant.name));
            let wins = if last {
                None
            } else {
                // An even feeder index arrives in slot one of its parent.
                rounds[depth].matches.get(index / 2).and_then(|parent| {
                    let (one, two) = parent.wins();
                    if index % 2 == 0 { one } else { two }
                })
            };

            lines[row].put(connector, "├─ ");
            lines[row].put(content, &participant(&name, wins, width));

            if !last {
                lines[row].put(next - 1, "─");
                lines[row].put(next, if index % 2 == 0 { "┐" } else { "┘" });
            }
        }

        if !last {
            // The vertical run between a pair, skipping the row where the next round's
            // own `├` goes.
            for pair in 0..feeders.len() / 2 {
                let top = pair * span * 4 + span - 1;
                let bottom = top + span * 2;
                let midpoint = top + span;
                for (row, line) in lines.iter_mut().enumerate().take(bottom).skip(top + 1) {
                    if row != midpoint {
                        line.put(next, "│");
                    }
                }
            }
            connector = next;
        }
    }

    lines.into_iter().map(|line| line.text.trim_end().to_owned()).collect()
}

/// A name padded to `width`, then that player's games. Blank rather than `0` when the
/// match has not started, so "not begun" stays distinguishable from "0-2 down".
fn participant(name: &str, wins: Option<u8>, width: usize) -> String {
    match wins {
        Some(wins) => format!("{} {wins}", fit(name, width)),
        None => format!("{}  ", fit(name, width)),
    }
}

/// Names only — no seed prefix.
///
/// A seed costs two or three of the twelve cells a name has, which is what pushes an
/// ordinary name into an ellipsis. Seeds are still in the per-round list view, where
/// there is room for them.
fn leaf(entrant: Option<&Entrant>) -> String {
    match entrant {
        Some(entrant) => sanitize(&entrant.name),
        None => "(bye)".to_owned(),
    }
}

/// A line built left to right, tracking how many display cells it occupies so that a
/// wide character cannot shift everything after it.
#[derive(Clone, Default)]
struct Line {
    text: String,
    cells: usize,
}

impl Line {
    fn put(&mut self, column: usize, text: &str) {
        while self.cells < column {
            self.text.push(' ');
            self.cells += 1;
        }
        self.text.push_str(text);
        self.cells += text.width();
    }
}

/// Make a name safe inside a code fence — or an inline code span, which has the same
/// hazard — which is a much smaller job than escaping markdown: only a backtick can
/// end the span early, and only a newline or a control character can break the grid
/// apart. Markdown escapes do not work inside code, so this replaces rather than
/// escapes.
pub(crate) fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '`' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

/// Truncate to `width` display cells, with a single-cell ellipsis, then pad to exactly
/// that many cells. Never splits a character, and counts a CJK character as the two
/// cells it occupies.
pub(crate) fn fit(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut cells = 0;

    if text.width() > width {
        // Leave room for the ellipsis.
        let room = width.saturating_sub(1);
        for c in text.chars() {
            let size = c.width().unwrap_or(0);
            if cells + size > room {
                break;
            }
            out.push(c);
            cells += size;
        }
        out.push('…');
        cells += 1;
    } else {
        out.push_str(text);
        cells = text.width();
    }

    while cells < width {
        out.push(' ');
        cells += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_WIDTH, Entrant, Match, Round, fit, render, render_round_list, sanitize};
    use crate::tournament::bracket::Slot;
    use unicode_width::UnicodeWidthChar;

    fn entrant(seed: u32, name: &str) -> Option<Entrant> {
        Some(Entrant {
            seed,
            name: name.to_owned(),
        })
    }

    fn played(a: (u32, &str), b: (u32, &str), score: (u8, u8)) -> Match {
        let winner = if score.0 > score.1 { Slot::One } else { Slot::Two };
        Match {
            slot1: entrant(a.0, a.1),
            slot2: entrant(b.0, b.1),
            score: Some(score),
            winner: Some(winner),
        }
    }

    fn pending() -> Match {
        Match {
            slot1: None,
            slot2: None,
            score: None,
            winner: None,
        }
    }

    /// A bracket of `entrants` with generated names, nothing played.
    fn unplayed(entrants: usize) -> Vec<Round> {
        let mut rounds = Vec::new();
        let mut count = entrants / 2;
        let mut depth = 0;

        while count >= 1 {
            let matches = (0..count)
                .map(|index| {
                    if depth == 0 {
                        Match {
                            slot1: entrant((index * 2 + 1) as u32, &format!("Player{}", index * 2 + 1)),
                            slot2: entrant((index * 2 + 2) as u32, &format!("Player{}", index * 2 + 2)),
                            score: None,
                            winner: None,
                        }
                    } else {
                        pending()
                    }
                })
                .collect();
            rounds.push(Round {
                name: format!("Round{}", depth + 1),
                matches,
            });
            if count == 1 {
                break;
            }
            count /= 2;
            depth += 1;
        }
        rounds
    }

    /// Which display cell a character sits in, measuring width rather than counting
    /// chars — the whole point of the alignment work.
    fn cell_of(line: &str, needle: char) -> Option<usize> {
        let mut cells = 0;
        for c in line.chars() {
            if c == needle {
                return Some(cells);
            }
            cells += c.width().unwrap_or(0);
        }
        None
    }

    fn body_lines(message: &str) -> Vec<String> {
        message
            .trim_start_matches("```\n")
            .split("```")
            .next()
            .expect("a fence")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_four_player_bracket_looks_like_the_design() {
        let rounds = vec![
            Round {
                name: "Semifinal".to_owned(),
                matches: vec![
                    played((1, "MarineLorD"), (8, "Beasty"), (2, 1)),
                    played((5, "VortiX"), (4, "Anotand"), (0, 2)),
                ],
            },
            Round {
                name: "Final".to_owned(),
                matches: vec![pending()],
            },
        ];

        let messages = render(&rounds, DEFAULT_WIDTH);
        assert_eq!(messages.len(), 1);

        // Both result cells are a fixed 12-cell name field plus the score, so the scores
        // line up vertically no matter how long a name is.
        assert_eq!(
            body_lines(&messages[0]),
            vec![
                "MarineLorD   2 ─┐",
                "                ├─ MarineLorD     ─┐",
                "Beasty       1 ─┘                  │",
                "                                   ├─ ?",
                "VortiX       0 ─┐                  │",
                "                ├─ Anotand        ─┘",
                "Anotand      2 ─┘",
            ]
        );
    }

    #[test]
    fn each_player_carries_their_own_games_in_the_round_they_played() {
        // Both semi-finals and the final decided, so every column has a score to show.
        let rounds = vec![
            Round {
                name: "Semifinal".to_owned(),
                matches: vec![
                    played((1, "MarineLorD"), (8, "Beasty"), (2, 1)),
                    played((5, "VortiX"), (4, "Anotand"), (0, 2)),
                ],
            },
            Round {
                name: "Final".to_owned(),
                matches: vec![played((1, "MarineLorD"), (4, "Anotand"), (1, 2))],
            },
        ];
        let lines = body_lines(&render(&rounds, DEFAULT_WIDTH)[0]);

        // The semi-final scores sit beside the four entrants.
        assert!(lines[0].starts_with("MarineLorD   2"), "{}", lines[0]);
        assert!(lines[2].starts_with("Beasty       1"), "{}", lines[2]);
        assert!(lines[4].starts_with("VortiX       0"), "{}", lines[4]);
        assert!(lines[6].starts_with("Anotand      2"), "{}", lines[6]);

        // The *final's* scores sit beside the two finalists, one column right — 1-2 to
        // Anotand, so the score is on whoever earned it rather than on the match.
        assert!(lines[1].contains("MarineLorD   1"), "{}", lines[1]);
        assert!(lines[5].contains("Anotand      2"), "{}", lines[5]);

        // And the last column is the champion, who has no next match to score in.
        assert!(lines[3].contains("├─ Anotand"), "{}", lines[3]);
        assert!(!lines[3].trim_end().ends_with(char::is_numeric), "{}", lines[3]);
    }

    #[test]
    fn a_match_that_has_not_started_leaves_the_score_blank() {
        let not_started = vec![Round {
            name: "Final".to_owned(),
            matches: vec![Match {
                slot1: entrant(1, "MarineLorD"),
                slot2: entrant(2, "Beasty"),
                score: None,
                winner: None,
            }],
        }];
        let underway = vec![Round {
            name: "Final".to_owned(),
            matches: vec![Match {
                slot1: entrant(1, "MarineLorD"),
                slot2: entrant(2, "Beasty"),
                score: Some((0, 1)),
                winner: None,
            }],
        }];

        // Blank, not zero: "not begun" has to stay distinguishable from "0-1 down".
        let idle = body_lines(&render(&not_started, DEFAULT_WIDTH)[0]);
        assert!(!idle[0].contains(char::is_numeric), "{}", idle[0]);
        assert!(!idle[2].contains(char::is_numeric), "{}", idle[2]);

        let live = body_lines(&render(&underway, DEFAULT_WIDTH)[0]);
        assert!(live[0].starts_with("MarineLorD   0"), "{}", live[0]);
        assert!(live[2].starts_with("Beasty       1"), "{}", live[2]);
    }

    #[test]
    fn every_join_of_one_connector_shares_a_column() {
        let rounds = vec![
            Round {
                name: "Semifinal".to_owned(),
                matches: vec![
                    played((1, "MarineLorD"), (8, "Beasty"), (2, 1)),
                    played((5, "VortiX"), (4, "Anotand"), (0, 2)),
                ],
            },
            Round {
                name: "Final".to_owned(),
                matches: vec![pending()],
            },
        ];
        let lines = body_lines(&render(&rounds, DEFAULT_WIDTH)[0]);

        // The seeded pairs' connectors.
        let first = cell_of(&lines[0], '┐').expect("a top join");
        assert_eq!(cell_of(&lines[1], '├'), Some(first));
        assert_eq!(cell_of(&lines[2], '┘'), Some(first));
        assert_eq!(cell_of(&lines[4], '┐'), Some(first));
        assert_eq!(cell_of(&lines[5], '├'), Some(first));
        assert_eq!(cell_of(&lines[6], '┘'), Some(first));

        // The connector that carries both into the final: ┐ then │, ├, │ then ┘, all in
        // one column. An off-by-one here is invisible in the code and glaring in Discord.
        let second = cell_of(&lines[1], '┐').expect("a second-round join");
        assert!(second > first);
        assert_eq!(cell_of(&lines[2], '│'), Some(second));
        assert_eq!(cell_of(&lines[3], '├'), Some(second));
        assert_eq!(cell_of(&lines[4], '│'), Some(second));
        assert_eq!(cell_of(&lines[5], '┘'), Some(second));
    }

    #[test]
    fn cjk_names_keep_the_columns_aligned() {
        // Traditional Chinese names are double-width, so counting chars would shift
        // every column after them.
        let rounds = vec![
            Round {
                name: "Semifinal".to_owned(),
                matches: vec![
                    played((1, "比那明居天子"), (8, "Beasty"), (2, 0)),
                    played((5, "納可"), (4, "包吞"), (1, 2)),
                ],
            },
            Round {
                name: "Final".to_owned(),
                matches: vec![pending()],
            },
        ];
        let lines = body_lines(&render(&rounds, DEFAULT_WIDTH)[0]);

        let column = cell_of(&lines[0], '┐').expect("a join");
        for (row, needle) in [(1, '├'), (2, '┘'), (4, '┐'), (5, '├'), (6, '┘')] {
            assert_eq!(
                cell_of(&lines[row], needle),
                Some(column),
                "row {row} is out of column:\n{}",
                lines.join("\n")
            );
        }
    }

    #[test]
    fn long_names_truncate_to_the_configured_width() {
        assert_eq!(fit("MarineLorD", 12).width_cells(), 12);
        assert_eq!(fit("AVeryLongPlayerNameIndeed", 12), "AVeryLongPl…");
        assert_eq!(fit("short", 8), "short   ");

        // A wide character is never split in half, so the cell can come out one short
        // of the target and is padded back up.
        let cjk = fit("比那明居天子的名字", 12);
        assert_eq!(cjk.width_cells(), 12);
        assert!(cjk.ends_with('…') || cjk.ends_with(' '));
    }

    #[test]
    fn a_backtick_cannot_escape_the_code_fence() {
        let rounds = vec![Round {
            name: "Final".to_owned(),
            matches: vec![played((1, "ev``il"), (2, "```rust"), (2, 0))],
        }];

        let message = &render(&rounds, DEFAULT_WIDTH)[0];

        // Exactly the opening and closing fences, and no backtick in between.
        assert_eq!(message.matches("```").count(), 2);
        assert!(!body_lines(message).join("\n").contains('`'));
        assert_eq!(sanitize("a`b"), "a'b");
    }

    #[test]
    fn a_newline_in_a_name_cannot_break_the_grid() {
        // Not a hypothetical: a name is player-controlled text, and one stray newline
        // would shift every row below it.
        let rounds = vec![Round {
            name: "Final".to_owned(),
            matches: vec![played((1, "line\nbreak"), (2, "tab\there"), (2, 0))],
        }];
        let lines = body_lines(&render(&rounds, DEFAULT_WIDTH)[0]);

        assert_eq!(lines.len(), 3, "a 2-player bracket is 3 rows:\n{}", lines.join("\n"));
    }

    /// Every match decided, with names at the full cell width — the widest a bracket
    /// of this size can render, which is the only size worth testing against a limit.
    fn fully_played(entrants: usize) -> Vec<Round> {
        let mut rounds = unplayed(entrants);
        for round in &mut rounds {
            for (index, game) in round.matches.iter_mut().enumerate() {
                game.slot1 = entrant((index * 2 + 1) as u32, "MarineLorDXY");
                game.slot2 = entrant((index * 2 + 2) as u32, "AnotandABCDE");
                game.score = Some((2, 1));
                game.winner = Some(Slot::One);
            }
        }
        rounds
    }

    #[test]
    fn brackets_up_to_eight_fit_one_message() {
        for entrants in [4, 8] {
            for rounds in [unplayed(entrants), fully_played(entrants)] {
                let messages = render(&rounds, DEFAULT_WIDTH);
                assert_eq!(messages.len(), 1, "{entrants} entrants should be one message");
            }
        }
    }

    #[test]
    fn larger_brackets_split_into_chunks_that_each_fit() {
        // 16 already needs splitting: fully played it is 2308 characters in one piece.
        // Most of that is the tall vertical runs, where a row exists only to carry a
        // `│` in a far column, so there is no narrowing that would rescue it.
        for entrants in [16, 32] {
            for rounds in [unplayed(entrants), fully_played(entrants)] {
                let messages = render(&rounds, DEFAULT_WIDTH);

                assert!(messages.len() > 1, "{entrants} entrants must split");
                for (index, message) in messages.iter().enumerate() {
                    assert!(
                        message.len() <= 2000,
                        "{entrants} entrants: chunk {index} is {} characters",
                        message.len()
                    );
                    assert_eq!(
                        message.matches("```").count(),
                        2,
                        "{entrants} entrants: chunk {index} is not exactly one fence"
                    );
                }
            }
        }
    }

    #[test]
    fn sixteen_splits_into_the_two_halves_and_the_final() {
        // The shape wanted: upper half, lower half, then the closing round.
        let messages = render(&fully_played(16), DEFAULT_WIDTH);
        assert_eq!(messages.len(), 3);

        let rows = |index: usize| body_lines(&messages[index]).len();
        assert_eq!(rows(0), 15, "an 8-entrant half is 15 rows");
        assert_eq!(rows(1), 15);
        assert_eq!(rows(2), 3, "the final is two entrants and their result");
    }

    #[test]
    fn a_champion_is_named_once_the_final_is_decided() {
        let decided = vec![Round {
            name: "Final".to_owned(),
            matches: vec![played((1, "MarineLorD"), (2, "Beasty"), (2, 1))],
        }];
        assert!(render(&decided, DEFAULT_WIDTH)[0].contains("🏆 **MarineLorD**"));

        let undecided = vec![Round {
            name: "Final".to_owned(),
            matches: vec![pending()],
        }];
        assert!(!render(&undecided, DEFAULT_WIDTH)[0].contains('🏆'));
    }

    #[test]
    fn the_round_list_renders_for_any_round() {
        let round = Round {
            name: "Quarterfinal".to_owned(),
            matches: vec![
                played((1, "MarineLorD"), (8, "Beasty"), (2, 1)),
                Match {
                    slot1: entrant(5, "VortiX"),
                    slot2: None,
                    score: None,
                    winner: None,
                },
            ],
        };

        let listing = render_round_list(&round);
        assert_eq!(
            listing,
            "**Quarterfinal**\n`1` MarineLorD  2 – 1  `8` Beasty\n`5` VortiX  vs  `?` —\n"
        );
    }

    #[test]
    fn the_round_list_escapes_markdown_because_it_is_not_in_a_fence() {
        let round = Round {
            name: "Final".to_owned(),
            matches: vec![played((1, "under_score"), (2, "*starred*"), (2, 0))],
        };

        let listing = render_round_list(&round);
        assert!(listing.contains("under\\_score"));
        assert!(listing.contains("\\*starred\\*"));
    }

    /// Display width of a rendered cell, so the assertions above read as widths.
    trait WidthCells {
        fn width_cells(&self) -> usize;
    }

    impl WidthCells for String {
        fn width_cells(&self) -> usize {
            unicode_width::UnicodeWidthStr::width(self.as_str())
        }
    }
}
