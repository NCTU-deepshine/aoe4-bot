//! Rasterizing a bracket SVG (`bracket_svg::svg`) into a PNG, with a bundled
//! font rather than whatever the host happens to have installed —
//! `docs/bracket-image.md`'s whole reason for existing: Discord clients don't
//! agree on how wide a CJK character renders relative to a Latin one, and the
//! only portable fix is to stop asking them.
//!
//! Two faces, because a player's display name can be in any script a
//! competitor picked — Noto Sans covers Latin/Greek/Cyrillic, Noto Sans CJK TC
//! covers Chinese/Japanese/Korean. Both OFL-licensed
//! (`assets/fonts/OFL-*.txt`), loaded once via `include_bytes!` rather than a
//! `COPY` in the Dockerfile's runtime stage, which carries only the binary.

use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

const NOTO_SANS: &[u8] = include_bytes!("../../assets/fonts/NotoSans-Regular.ttf");
const NOTO_SANS_CJK_TC: &[u8] = include_bytes!("../../assets/fonts/NotoSansCJKtc-Regular.otf");

/// Loaded once at first use, not per render — the same one-time-setup shape
/// `drafttool::client()` uses for its HTTP client.
fn fonts() -> &'static Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(NOTO_SANS)));
        db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(NOTO_SANS_CJK_TC)));
        Arc::new(db)
    })
}

/// What went wrong turning an SVG document into a PNG.
#[derive(Debug)]
pub(crate) enum RasterError {
    /// `bracket_svg::svg`'s own output failed to parse — a bug in that
    /// module, since nothing downstream should ever hand this malformed XML.
    InvalidSvg(String),
    /// A zero-size document — an empty bracket, which `bracket_view` should
    /// never reach this with.
    EmptySize,
    Encode(String),
}

impl std::fmt::Display for RasterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSvg(message) => write!(f, "the bracket SVG failed to parse: {message}"),
            Self::EmptySize => write!(f, "the bracket SVG has no size to rasterize"),
            Self::Encode(message) => write!(f, "failed to encode the bracket PNG: {message}"),
        }
    }
}

impl std::error::Error for RasterError {}

/// Rasterizes `svg` (`bracket_svg::svg`'s output) into PNG bytes, using the
/// bundled fonts and no others — `system-fonts` is off (see `Cargo.toml`),
/// since falling back to whatever the host has installed is the bug this
/// exists to fix.
///
/// CPU-bound: a caller on the gateway's executor should run this inside
/// `tokio::task::spawn_blocking` rather than call it directly.
pub(crate) fn rasterize(svg: &str) -> Result<Vec<u8>, RasterError> {
    let options = usvg::Options {
        fontdb: fonts().clone(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).map_err(|err| RasterError::InvalidSvg(err.to_string()))?;

    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).ok_or(RasterError::EmptySize)?;
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());

    pixmap.encode_png().map_err(|err| RasterError::Encode(err.to_string()))
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

    /// A PNG signature is the eight fixed bytes every valid file starts with.
    fn decodes_as_png(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
    }

    #[test]
    fn a_plain_bracket_rasterizes_to_a_png() {
        let rounds = vec![Round {
            name: "Final".to_string(),
            matches: vec![played(
                entrant(1, "MarineLorD"),
                entrant(2, "Beasty"),
                (2, 0),
                Slot::One,
            )],
        }];
        let svg = crate::tournament::bracket_svg::svg(&render::grid(&rounds, DEFAULT_WIDTH));
        let png = rasterize(&svg).expect("a well-formed bracket should rasterize");
        assert!(decodes_as_png(&png), "did not produce a PNG signature");
    }

    #[test]
    fn a_mixed_script_bracket_rasterizes_without_error() {
        // The whole point: a Latin and a CJK name in the same bracket, on the
        // bundled fonts alone — no system font stack involved.
        let rounds = vec![Round {
            name: "Final".to_string(),
            matches: vec![played(
                entrant(1, "測試選手"),
                entrant(2, "MarineLorD"),
                (2, 0),
                Slot::One,
            )],
        }];
        let svg = crate::tournament::bracket_svg::svg(&render::grid(&rounds, DEFAULT_WIDTH));
        let png = rasterize(&svg).expect("a mixed-script bracket should rasterize");
        assert!(decodes_as_png(&png));
    }

    #[test]
    fn the_bundled_database_resolves_both_faces_by_name() {
        // If either of these ever fails, a name in that script renders as
        // tofu instead of failing loudly — assert the fonts are actually
        // there rather than trust the rasterize call alone to notice.
        for family in ["Noto Sans", "Noto Sans CJK TC"] {
            let query = usvg::fontdb::Query {
                families: &[usvg::fontdb::Family::Name(family)],
                ..Default::default()
            };
            let id = fonts()
                .query(&query)
                .unwrap_or_else(|| panic!("{family} did not resolve"));
            let has_data = fonts().with_face_data(id, |data, _index| !data.is_empty());
            assert_eq!(has_data, Some(true), "{family}'s resolved face has no backing data");
        }
    }

    #[test]
    fn an_invalid_document_is_reported_rather_than_panicking() {
        assert!(rasterize("not an svg document").is_err());
    }
}
