//! Rendering a bracket's character grid as an SVG document, for rasterizing
//! into an image instead of a Discord code block.
//!
//! Consumes `render::grid`'s character matrix rather than re-deriving the
//! layout: a name or a connector lands at the same display cell either way,
//! so this cannot drift from the text renderer. Box-drawing connectors become
//! vector strokes — cheap, and it means the bundled font never has to cover
//! them — and everything else becomes text, one `<text>` element per maximal
//! run of non-space, non-connector cells rather than one per character, so a
//! proportional font still reads naturally while landing on the same grid.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Px per display cell — `render::grid`'s own unit, where a narrow character
/// is one cell and a CJK character is two.
const CELL_PX: f64 = 16.0;
const ROW_PX: f64 = 26.0;
const FONT_SIZE_PX: f64 = 18.0;
const MARGIN_PX: f64 = 16.0;
/// Distance from a row's top to its text baseline, chosen to center an
/// 18px face inside a 26px row.
const BASELINE_PX: f64 = 19.0;

/// Matches the dark code block this replaces, so the drawing reads the same
/// in either Discord theme rather than depending on which one the viewer has.
const BACKGROUND: &str = "#2b2d31";
const FOREGROUND: &str = "#dcddde";
const STROKE: &str = "#949ba4";
const STROKE_WIDTH: f64 = 2.0;

/// The font stack a `<text>` element asks for — the bundled faces loaded by
/// `bracket_raster`, by name.
const FONT_FAMILY: &str = "Noto Sans, Noto Sans CJK TC";

/// Every box-drawing character `render::grid` can emit. `place` is exhaustive
/// over this set — anything else is text — so a glyph added to `grid` later
/// and missing here fails the test that checks for exactly that, rather than
/// silently rendering as an unreadable character.
const CONNECTORS: [char; 5] = ['─', '│', '┐', '┘', '├'];

fn is_connector(ch: char) -> bool {
    CONNECTORS.contains(&ch)
}

/// One occupied cell, or run of cells, in the grid — classified for drawing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Placed {
    /// A single box-drawing character, at the display cell it occupies.
    Connector { row: usize, cell: usize, glyph: char },
    /// A maximal run of adjacent non-space, non-connector characters — a name
    /// or a score digit — starting at `cell` and spanning `width` display
    /// cells. One `<text>` element per run rather than per character, so a
    /// proportional font is not chopped into single glyphs.
    Text {
        row: usize,
        cell: usize,
        text: String,
        width: usize,
    },
}

/// Classifies every non-space character in `lines` — `render::grid`'s output
/// — by display cell. Pure and total: any character `grid` can produce is
/// either a known connector or routed to text, so nothing it emits can
/// silently vanish from the drawing.
pub(crate) fn place(lines: &[String]) -> Vec<Placed> {
    let mut placed = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let mut cell = 0;
        let mut run: Option<(usize, String, usize)> = None;
        for ch in line.chars() {
            let width = ch.width().unwrap_or(0);
            if ch == ' ' {
                flush_run(&mut run, row, &mut placed);
            } else if is_connector(ch) {
                flush_run(&mut run, row, &mut placed);
                placed.push(Placed::Connector { row, cell, glyph: ch });
            } else {
                match &mut run {
                    Some((_, text, run_width)) => {
                        text.push(ch);
                        *run_width += width;
                    },
                    None => run = Some((cell, ch.to_string(), width)),
                }
            }
            cell += width;
        }
        flush_run(&mut run, row, &mut placed);
    }
    placed
}

fn flush_run(run: &mut Option<(usize, String, usize)>, row: usize, placed: &mut Vec<Placed>) {
    if let Some((cell, text, width)) = run.take() {
        placed.push(Placed::Text { row, cell, text, width });
    }
}

/// Renders `lines` — `render::grid`'s character matrix — as a self-contained
/// SVG document on an opaque background, sized to the widest row.
pub(crate) fn svg(lines: &[String]) -> String {
    let placed = place(lines);
    let cols = lines.iter().map(|line| line.width()).max().unwrap_or(0);
    let rows = lines.len();
    let width = cols as f64 * CELL_PX + MARGIN_PX * 2.0;
    let height = rows as f64 * ROW_PX + MARGIN_PX * 2.0;

    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\"><rect width=\"{width}\" height=\"{height}\" fill=\"{BACKGROUND}\"/>"
    );
    for p in &placed {
        match p {
            Placed::Connector { row, cell, glyph } => out.push_str(&connector_svg(*row, *cell, *glyph)),
            Placed::Text { row, cell, text, width } => out.push_str(&text_svg(*row, *cell, text, *width)),
        }
    }
    out.push_str("</svg>");
    out
}

/// A cell's edges and center, in document px. Every connector glyph is drawn
/// from the same center, so an adjacent cell's stroke always meets it exactly
/// at the shared edge regardless of which two glyphs are involved.
fn cell_box(row: usize, cell: usize) -> (f64, f64, f64, f64, f64, f64) {
    let left = MARGIN_PX + cell as f64 * CELL_PX;
    let top = MARGIN_PX + row as f64 * ROW_PX;
    let right = left + CELL_PX;
    let bottom = top + ROW_PX;
    (left, top, right, bottom, left + CELL_PX / 2.0, top + ROW_PX / 2.0)
}

fn line_svg(x1: f64, y1: f64, x2: f64, y2: f64) -> String {
    format!(
        "<line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" stroke=\"{STROKE}\" \
         stroke-width=\"{STROKE_WIDTH}\" shape-rendering=\"crispEdges\"/>"
    )
}

/// One box-drawing character as vector strokes rather than a glyph. Named
/// for what it connects, exactly as the Unicode block does: `┐` is down and
/// left, `┘` is up and left, `├` is up, down and right.
fn connector_svg(row: usize, cell: usize, glyph: char) -> String {
    let (left, top, right, bottom, cx, cy) = cell_box(row, cell);
    match glyph {
        '─' => line_svg(left, cy, right, cy),
        '│' => line_svg(cx, top, cx, bottom),
        '┐' => line_svg(left, cy, cx, cy) + &line_svg(cx, cy, cx, bottom),
        '┘' => line_svg(left, cy, cx, cy) + &line_svg(cx, cy, cx, top),
        '├' => line_svg(cx, top, cx, bottom) + &line_svg(cx, cy, right, cy),
        _ => unreachable!("place() only ever classifies CONNECTORS as connectors"),
    }
}

/// A run of characters as one proportional-font `<text>` element, its
/// `textLength` pinned to the display cells it occupies — natural glyph
/// shapes, exact column alignment.
fn text_svg(row: usize, cell: usize, text: &str, width: usize) -> String {
    let (left, top, ..) = cell_box(row, cell);
    let baseline = top + BASELINE_PX;
    let text_length = width as f64 * CELL_PX;
    format!(
        "<text x=\"{left}\" y=\"{baseline}\" font-family=\"{FONT_FAMILY}\" font-size=\"{FONT_SIZE_PX}\" \
         textLength=\"{text_length}\" lengthAdjust=\"spacingAndGlyphs\" fill=\"{FOREGROUND}\">{}</text>",
        escape_xml(text)
    )
}

/// A player's display name is free text, not markup — `render::sanitize`
/// only guards the code-fence path, so `<`, `&` and quotes reach here
/// unescaped and have to be handled at this boundary instead.
fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tournament::bracket::Slot;
    use crate::tournament::render::{self, DEFAULT_WIDTH, Entrant, Match, Round};

    fn entrant(seed: u32, name: &str) -> Entrant {
        Entrant {
            seed,
            name: name.to_string(),
        }
    }

    fn played(slot1: Entrant, slot2: Entrant, score: (u8, u8), winner: Slot) -> Match {
        Match {
            slot1: Some(slot1),
            slot2: Some(slot2),
            score: Some(score),
            winner: Some(winner),
        }
    }

    /// Which display cell `needle` sits in on `line`. Mirrors `render.rs`'s
    /// own `cell_of` test helper, which is private to that module's tests
    /// and so not reachable from here.
    fn cell_of(line: &str, needle: char) -> Option<usize> {
        let mut cells = 0;
        for ch in line.chars() {
            if ch == needle {
                return Some(cells);
            }
            cells += ch.width().unwrap_or(0);
        }
        None
    }

    fn four_player_grid() -> Vec<String> {
        let rounds = vec![
            Round {
                name: "Semifinal".to_string(),
                matches: vec![
                    played(entrant(1, "MarineLorD"), entrant(4, "Beasty"), (2, 1), Slot::One),
                    played(entrant(2, "VortiX"), entrant(3, "Anotand"), (0, 2), Slot::Two),
                ],
            },
            Round {
                name: "Final".to_string(),
                matches: vec![Match {
                    slot1: None,
                    slot2: None,
                    score: None,
                    winner: None,
                }],
            },
        ];
        render::grid(&rounds, DEFAULT_WIDTH)
    }

    #[test]
    fn every_name_lands_at_the_same_cell_the_text_renderer_puts_it() {
        let lines = four_player_grid();
        let placed = place(&lines);

        for (row, line) in lines.iter().enumerate() {
            for name in ["MarineLorD", "Beasty", "VortiX", "Anotand"] {
                if !line.contains(name) {
                    continue;
                }
                let first = name.chars().next().unwrap();
                let expected = cell_of(line, first).unwrap();
                let found = placed.iter().any(|p| {
                    matches!(p, Placed::Text { row: r, cell, text, .. }
                        if *r == row && *cell == expected && text == name)
                });
                assert!(found, "{name} not placed at row {row} cell {expected}: {line}");
            }
        }
    }

    #[test]
    fn every_connector_lands_at_the_same_cell_the_text_renderer_puts_it() {
        let lines = four_player_grid();
        let placed = place(&lines);

        for (row, line) in lines.iter().enumerate() {
            for glyph in CONNECTORS {
                let mut search_from = 0;
                while let Some(rel) = line[search_from..].find(glyph) {
                    let idx = search_from + rel;
                    let expected = line[..idx].width();
                    assert!(
                        placed.contains(&Placed::Connector {
                            row,
                            cell: expected,
                            glyph
                        }),
                        "{glyph} at row {row} cell {expected} missing from {placed:?}: {line}"
                    );
                    search_from = idx + glyph.len_utf8();
                }
            }
        }
    }

    #[test]
    fn every_non_space_character_is_either_a_connector_or_text() {
        // A connector added to `grid()` later must fail this rather than
        // silently disappear from the image.
        let lines = four_player_grid();
        let placed = place(&lines);
        let total_non_space = lines.iter().flat_map(|l| l.chars()).filter(|c| *c != ' ').count();
        let accounted: usize = placed
            .iter()
            .map(|p| match p {
                Placed::Connector { .. } => 1,
                Placed::Text { text, .. } => text.chars().count(),
            })
            .sum();
        assert_eq!(accounted, total_non_space);
    }

    #[test]
    fn a_run_spans_its_full_display_width() {
        // "MarineLorD" is ten narrow characters; the run's declared width
        // has to match, since that is what pins `textLength`.
        let lines = four_player_grid();
        let placed = place(&lines);
        let run = placed
            .iter()
            .find_map(|p| match p {
                Placed::Text { text, width, .. } if text == "MarineLorD" => Some(*width),
                _ => None,
            })
            .expect("MarineLorD should appear as one run");
        assert_eq!(run, 10);
    }

    #[test]
    fn cjk_names_still_land_on_the_grid_column_they_are_drawn_at() {
        let rounds = vec![Round {
            name: "Final".to_string(),
            matches: vec![played(entrant(1, "測試選手"), entrant(2, "Player"), (2, 0), Slot::One)],
        }];
        let lines = render::grid(&rounds, DEFAULT_WIDTH);
        let placed = place(&lines);
        assert!(
            placed
                .iter()
                .any(|p| matches!(p, Placed::Text { text, .. } if text.contains('測'))),
            "{placed:?}"
        );
    }

    #[test]
    fn the_document_is_well_formed_enough_to_open() {
        let doc = svg(&four_player_grid());
        assert!(doc.starts_with("<svg"));
        assert!(doc.ends_with("</svg>"));
        assert_eq!(doc.matches("<svg").count(), 1, "{doc}");
    }

    #[test]
    fn a_name_with_markup_characters_cannot_break_the_document() {
        let rounds = vec![Round {
            name: "Final".to_string(),
            matches: vec![played(
                entrant(1, "<script>&\"'"),
                entrant(2, "Player"),
                (2, 0),
                Slot::One,
            )],
        }];
        let doc = svg(&render::grid(&rounds, DEFAULT_WIDTH));
        assert!(!doc.contains("<script>"), "{doc}");
        assert!(doc.contains("&lt;script&gt;&amp;&quot;&apos;"), "{doc}");
    }
}
