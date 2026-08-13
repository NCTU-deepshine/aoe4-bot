# Rendering the bracket as an image

A design note for the §12 open question in `tournament.md`: "rendering the bracket as an image instead
of a code block." Scoped to this improvement only — not a chunk in `implementation.md`'s sequence, and
not started.

## 1. The problem this solves

`render.rs`'s ASCII bracket pads every name to a fixed column width in **display cells**, measured
against the Unicode East Asian Width standard (`unicode-width`, tested correct). The bug is not in that
math — it's that Discord clients don't all render a CJK character at exactly 2× a Latin character's
width. Confirmed empirically (a calibrated ruler pasted into Discord): one client measured ~1.95×, and
different clients/fonts have no reason to agree on a ratio at all. When the ratio is off, the connectors
(`┐├┘│`) drift out of their shared columns.

A structural fix that keeps names off the shared grid (numbers in the tree, names in a legend) was
prototyped and rejected as too much of a downgrade. The only fix that's actually portable is controlling
the font: render to a bitmap with a bundled font instead of asking every Discord client's font stack to
agree on a width.

## 2. Scope for this pass

**Covers:** a single-image bracket, for any size that renders as one Discord message today. **Does not
cover:** brackets past that size. `render.rs` already knows the exact threshold — a bracket needs
splitting once its rendered form exceeds 2000 characters, which starts at 16 entrants (2308 chars, three
messages) and reaches ~7 messages at the 32-entrant cap. An image doesn't split into multiple Discord
messages the way text does; tiling multiple images into one continuous drawing is a real design problem
of its own (how does an edit target the right tile as the field grows past a chunk boundary?) and isn't
solved here. **The target event is 8 entrants and never reaches the split threshold**, so this scope
covers the case that actually matters and defers the harder one until there's a concrete reason to need
it.

The existing text renderer stays as the fallback for anything past the single-image threshold, unchanged.

## 3. Font: covers what player names use, not the bot's own locale

Easy mistake to make: the bot's own UI is bilingual zh-TW/English (`Locale`), but that has nothing to do
with what a **player's name** can contain — an aoe4world display name can be in any script a competitor
picked, which in practice means Latin as the majority case plus meaningful CJK, Cyrillic, and other
representation. The font has to cover what shows up in `Entrant.name`, not what language the bot speaks.

That rules out a narrow single-script subset (e.g. Traditional-Chinese-only) as too risky — a Korean or
Russian name would fall back to missing-glyph tofu, which is a worse failure than the width bug this is
meant to fix. The realistic choice is a broad multi-script font (something in the Noto/Source Han family,
or an equivalent single file with wide coverage), sized in the tens of MB even after reasonable
subsetting. That's the real cost center of this whole feature — not the rendering code.

## 4. Approach: reuse the existing layout math, swap the drawing target

`render.rs`'s `grid` function already does the hard part: it knows exactly which row and column every
name, connector, and score belongs in (`span = 1 << depth`, row `index * span * 2 + span - 1`, the
`Line`/`fit` width accumulator). None of that changes. What changes is the output format:

- Instead of writing padded text and box-drawing characters into a `String`, emit an SVG document —
  `<text x=".." y="..">` elements at the same computed coordinates, `<line>`/`<path>` elements in place of
  `┐├┘│`.
- Rasterize the SVG with `resvg` (pure Rust — `usvg` + `tiny-skia` underneath, no system font stack, no
  Dockerfile change beyond adding the crate), pointed at the bundled font via `fontdb` rather than
  letting it fall back to whatever the host has installed.
- Send the result as a `CreateAttachment` (creation) or via `EditMessage::new_attachment` (redraw) instead
  of `content`. Both already exist in the vendored serenity with no feature-flag change needed.

This is deliberately not `cosmic-text`/manual glyph placement — SVG generation turns "lay out glyph runs
by hand" into "write XML at coordinates we already compute," which is the smaller amount of new code for
a layout this constrained (fixed-width columns, no line-wrapping, no bidi).

## 5. What has to change, concretely

- A new function alongside `render::render` that takes the same `&[Round]` and produces one SVG string
  instead of one or more text chunks — used only below the single-image threshold.
- A rasterize step (`resvg`) producing PNG bytes.
- `bracket_view::reconcile`'s post/edit calls switch from `CreateMessage::new().content(chunk)` /
  `EditMessage::new().content(chunk)` to attaching the PNG. The stored-message-id-per-ordinal mechanism
  (`tournament_bracket_messages`) is unaffected below the threshold, since there's exactly one message.
- The font ships as a bundled asset (`include_bytes!` or a `COPY` in the Dockerfile) and is loaded once
  at process start, not per render — the same one-time-setup shape `drafttool.rs`'s `client()` already
  uses for its `OnceLock`.

## 6. What this does *not* address, and should be decided before building

- **The un-throttled redraw fan-out.** `bracket_view::reconcile` is called from 13 sites today (every
  register/withdraw, command and button, plus setup/seed/start/settle) with no rate limit — unlike
  `panel::refresh` and friends, which share `EditThrottle`. A ~1.2KB text edit absorbs that; a render-and
  -reupload on every button press does not. This isn't solved by the image renderer itself, but shipping
  images without addressing it turns a latent inefficiency into a real one.
- **Multi-image tiling** for brackets past the single-image threshold (§2) — a separate design problem,
  not a variant of this one.
- **Font licensing and exact coverage** — needs a concrete pick (candidate: Noto Sans + Noto Sans CJK,
  OFL-licensed) and a subsetting decision before the deploy-size cost is known precisely.

## 7. Verification, once built

- Golden-file comparison against `render.rs`'s existing text-mode tests (`brackets_up_to_eight_fit_one_message`,
  the 4/8-entrant fixtures) — same input data, assert the SVG places every name/connector at the same
  logical grid position the text renderer does.
- A rendered PNG, opened by hand, for at least one bracket with a mixed-script name (Latin + CJK in the
  same bracket) to confirm no tofu and no column drift.
- Confirm `EditMessage::new_attachment` actually replaces rather than appends on a real message, and that
  `reconcile`'s existing `panel_check::is_confirmed_missing` fallback (post if the stored message is
  gone) still works with an attachment-bearing message.
