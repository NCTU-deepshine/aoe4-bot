# Tournament implementation — commit plan

Ordered chunks for building what [`tournament.md`](./tournament.md) designs. Each numbered entry is intended to
be **one commit**: a single reviewable purpose, building and green on its own.

Design decisions are **not** restated here. Each chunk names the sections that govern it; read those before
writing the code, and if a chunk seems to contradict them, the design doc wins or gets amended first.

## Working rules

- **The gate is `./check.sh --check`** — `cargo fmt --all --check`, `cargo clippy --all-targets -D warnings`,
  `cargo test`. Every commit passes it.
- **No live network in new tests.** `src/ranked.rs` has tests that call aoe4world, but they are
  `#[ignore = "hits the live aoe4world API"]` — opt-in, so CI does not depend on that service. Use the same
  attribute if a live check is ever wanted; otherwise test deserialization against saved payloads (§10).
- **`main` auto-deploys** (Fly, via GitHub Actions). So a half-finished command must not be added to the
  registration lists in `src/main.rs` until its chunk is complete — landing the code is fine, exposing it is not.
  Migrations, being additive, are safe to deploy as they land.
- **One migration file per schema chunk**, never edited after it lands. A rewritten migration diverges from the
  deployed database.
- **Two guilds, hardcoded** (§8.0). No per-guild configuration table and no setup command in this plan.
- Chunks 1–20 and 24 have no external dependencies, and 19 is already a shippable tournament. Chunk 21 is in
  **another repository**; only 22 waits on it, and most of 22 can be built before it lands.
- **Chunk numbers are append-only.** New chunks take the next free number rather than slotting in where they
  belong in the order, because the numbers already cited in shipped code (chunks 6–10's comments in `db.rs`,
  `panel.rs`, `registration.rs`, `mod.rs`, `dispatch.rs`, `commands.rs` — e.g. "consumed by chunk 12") would
  silently break under renumbering. So the number says where a chunk was written down, and only the phase it
  sits in says where it belongs.
- **Chunks 25, 26 and 24 landed out of numerical order**, in that sequence, right after chunk 10 and before the
  rest of Phase D. 24 (localization) retrofitted chunks 7–10, 25 and 26; **every chunk from here on writes its
  user-facing text through `Locale` from the start**, and its shared surfaces bilingual (§8.10).

## Phase A — foundations

Nothing user-visible. All three are prerequisites for everything after.

**1. Versioned migrations, and one test pool**
Add `migrations/` driven by `sqlx::migrate!`, run *after* the existing `schema.sql` execute so the live
database is untouched. `sqlx`'s default features are on, so no dependency change. Replace the three duplicated
`sqlite::memory:` preambles in `src/integration_tests.rs` with a shared `test_pool()` that runs both steps.
Design: §9.
Gate: migrator clean on an empty database and on one that already has `accounts`; `pragma foreign_keys` is on
(assert it — every `references` in §4 is inert otherwise).

**2. Command-error plumbing and explicit serenity features**
Add an `on_error` handler to `FrameworkOptions` (there is none today) and a helper for ephemeral replies. Add
`builder` — and anything else §8's APIs need — to our **own** `serenity` features in `Cargo.toml`: it is
currently enabled only transitively through poise, so §8's `CreateChannel` / `CreateThread` / `CreateButton` /
`EditThread` would vanish if poise's feature set ever changed. Do **not** enable `collector` for our own use
(§8.5 deliberately rejects it).
Design: §8.2, §8.9.

**3. Split the bot across two guilds**
Registration becomes two lists instead of one: every existing command stays home-guild-only, and the
tournament-guild list starts empty. Add the guild ids as config, thread them through `Data` — including the copy
the cron closure builds — add a `guild_only` + right-guild check usable by every command, and put a guild guard on
the message-reaction handler, which has none today and would otherwise start reacting in the tournament guild as
soon as the bot joins.
Design: §8.0.
Gate: the right-guild check as a pure function; the two command lists asserted **disjoint**, so a later chunk
cannot quietly register a tournament command in the home guild.

## Phase B — pure logic

No database, no Discord. These hold the subtle parts, so they are reviewed without any I/O noise around them,
and they can land before the tables exist.

**4. Bracket generation**
`bracket_size`, seed order by reflection, bye placement, round/set construction, advancement links. Pure
functions over plain structs.
Design: §5.
Gate: §10's bracket-math list — pairings for n = 2, 3, 4, 6, 8, 16; byes on top seeds; no two top-4 seeds meeting
before the semi-finals; advancement links forming a single-rooted tree; `best_of` varying per round.

**5. Bracket rendering**
`fn render(sets, width) -> Vec<String>`, plus the per-round list view. Adds `unicode-width`.
Design: §8.6.
Gate: §10's rendering list. Assert the box-drawing joins share a column rather than only diffing golden strings;
CJK alignment; backtick/fence stripping; n = 32 splitting under 2000 chars.

## Phase C — data model

**6. Core schema**
One migration for all nine tables (§4's seven plus §8.8's `tournament_admins` and
`tournament_bracket_messages`), the row types, and the queries the later chunks need. New module `src/tournament/`.
Nothing here reads or writes `accounts` — the tournament side owns `tournament_players` and the separation is the
design (§4 notes).
Design: §4, §8.8.
Gate: foreign keys enforced — an entry for a user with no `tournament_players` row is rejected; both uniqueness
directions on `tournament_players` hold; the `check` constraints reject unknown status values.

## Phase D — running an event

Each chunk from here adds a usable command, registered in the tournament-guild list only. Register it when its
chunk is done, not before. Chunks 25 and 26 belong to this phase but sit at the end of it, carrying the numbers
they were written down with (see Working rules); they land right after chunk 10, before 11.

**7. `/tournament create`, and the admin list**
Channel and category creation including `#…-draft`, the top-level-channel fallback, the `tournaments` and
`tournament_admins` rows, and `/tournament admin add|remove|list`. This is where access control enters the
codebase, `MANAGE_GUILD` bypass included.
Design: §8.1, §8.2.
Gate: the permission decision as a pure function (creator / admin / `MANAGE_GUILD` / nobody); slug validation.

**8. The interaction dispatcher**
One `EventHandler::interaction_create` branch over `"<action>:<entity_id>"` custom_ids, Defer-first for any
handler that makes an HTTP call, unknown and malformed ids ignored rather than panicking, and the throttled
panel-edit helper. No panels yet — this is the shared mechanism for chunks 9, 10, 20 and 22.
Design: §8.5.
Gate: §10's Discord-helper list — every custom_id round-trips to the right action and entity, and a stale or
malformed one is ignored.

**9. Registration, which is also binding**
`/tournament register [aoe4_id]` with the aoe4world autocomplete, `/tournament rebind`, `/withdraw`, the panel,
its buttons, and the ephemeral feedback rules. A first sign-up writes `tournament_players` and the entry in one
transaction; later ones find the player row already there, which is what makes the button work with no argument.
Design: §8.5 (registration panel), §4 (`tournament_players` and the notes).
Gate: §10's registration and player-binding lists — the two writes are atomic, a profile already claimed by
another user is rejected with a readable message, a rebind is refused during a running event, a second
registration is idempotent, withdrawal only before start.

**10. Check-in**
`/tournament open-checkin`, `/tournament checkin`, `/tournament close-checkin`, the panel, and no-show marking.
Design: §8.3, §8.5 (check-in panel).
Gate: §10's check-in list — second check-in idempotent, unregistered rejected, closing marks exactly the
non-checked-in entrants.

**11. Seeding**
ATR and ELO through the existing `fetch_profile` and shared client (§6 "Reuse" — no new HTTP client), the tiered
suggestion, `/tournament seed list|set|refresh`, and the ratings refresh at seeding time. No ratings cache (§4).
Adds a **seeding panel** in `#…-bracket`, posted by `close-checkin` and edited in place afterwards, which is
what makes `seed set`'s shift-down renumbering safe to watch. Seeding at close-checkin is best-effort: the
status has already moved, so an aoe4world outage reports the gap and leaves `seed refresh` to retry. One
migration for the panel's message id. `set_seed_order` nulls every seed before rewriting, or shifting a field
collides with `unique (tournament_id, seed)`.
Design: §6, §8.5.
Gate: §10's seeding list — tiering with only some entrants rated, an override that leaves the suggestion
intact, a reorder that does not trip the unique index, and esports-leaderboard deserialization against a saved
payload including its nullable `profile_id` rows.

**12. `/tournament start`**
Generate the bracket in one transaction from finalized seeds and open every playable set. Consumes chunks 4
and 5, chunk 27 for the gate and each round's `best_of` (from its draft preset, not an option here), and chunk
29 for publication — the preview messages become the real bracket in place, so nothing is posted afresh.
Design: §8.3, §5, §8.6.
Gate: §10's lifecycle list — starting before check-in closes, non-contiguous seeds, registering after start all
rejected.

**13. `/tournament bracket` and `/tournament cancel` — dropped, not built**
Neither half earned its place, so Phase D closes at chunk 12 plus 25–29.

`/tournament bracket` was to refresh or repost the bracket and offer a per-round companion view. But
the bracket is already reconciled on every registration, withdrawal, `seed set`, `start` and
`refresh`, so it is current without being asked; and `#…-bracket` denies `SEND_MESSAGES` to
`@everyone`, so nothing can bury it except the seeding panel sharing the channel. That left a jump
link as the command's only value. The per-round list has a better home than a command nobody knows to
type — the round-opening announcement (chunks 16–18), where every player sees it. So
`render::render_round_list` stays written and unused until then.

`/tournament cancel` was to move any status to `canceled`. Without an un-cancel it ends an event
without ending it, and `/tournament delete` already removes one; keeping a read-only record of an
abandoned event was not worth a terminal state nothing can leave. `canceled` remains in the schema's
`check` constraint — dropping it would mean editing a landed migration — but nothing writes it.

**25. `/tournament reopen-registration`**
The one backward lifecycle edge, for admin mistakes. Reverts `checkin` or `seeding` to `registration`:
`no_show` entries back to `active`, every `checked_in_at` cleared along with `seed`/`suggested_seed` (null
until chunk 11 writes them, but a reopen out of `seeding` is exactly when they would be stale),
`checkin_closes_at` and `checkin_message_id` nulled, and the check-in panel message deleted so a later
`open-checkin` posts a clean one. Needs `set_checkin_message_id` widened to `Option<i64>` (mirroring `set_checkin_closes_at`), an inverse of
`mark_no_shows`, and an unthrottled `panel::refresh_now` mirroring `checkin_panel::close` — restoring no-shows
changes the registration roster, and a phase change deserves a guaranteed edit rather than a throttled one. No
migration: the `check` constraints already permit both target values.
Design: §8.3, §8.4.
Gate: §10's reopening-registration list.

**26. `/tournament delete`**
The inverse of chunk 7, for a mistyped `create` and for teardown between test runs. Refuses unless `confirm`
matches the slug exactly and unless invoked from the announce channel — the only one of the five that survives,
so the reply doesn't vanish with its own channel. Deletes `#…-register|bracket|draft|matches`, then the
`tournaments` row, which cascades to every tournament-scoped table; the announce channel, the category and
`tournament_players` are left alone. Channel deletions are best-effort and logged — one an admin already
removed by hand must not block the database cleanup. Creator-or-`MANAGE_GUILD` tier, so it reuses
`tournament_admin_only`, whose refusal message stops naming the admin list now that two commands
share it.
Design: §8.1, §8.2, §8.4.
Gate: §10's deletion-cascade list.

**27. Tournament setup, and preset-derived match lengths**
`/tournament setup` (entrant cap, start time) and `/tournament preset`, which assigns a draft preset to a round
and everything after it. The preset is where `best_of` comes from: §3.3 already requires the two to match, so
deriving beats validating. Blocks chunk 12 until a preset and a start time exist. The cap defaults to 32 and is
enforced at **registration** — the sign-up that would exceed it is refused, so an over-full field never happens
and no admin "kick" is needed; rejoining after a withdrawal is capped too. Adds `drafttool.rs`, unauthenticated
reads only, so none of Phase E is a prerequisite. One migration.
Design: §3.3, §8.3, §4.
Gate: the preset cascade resolved at every depth; the cap refusing at the boundary, freed by a withdrawal, and
not leaking via rejoin; preset validation against a saved payload; a local wall time stored as the right UTC.

**29. The live bracket preview**
The bracket drawn into `#…-bracket` from the first two entrants, labelled provisional, redrawn as the field
and the seeding change. `bracket_view::preview_rounds` orders by `seeding::suggested_order` and feeds chunk 5's
renderer; chunk 12 then reuses the same messages for the real thing rather than publishing its own. The
awkward part is that the message count follows the bracket size, so a redraw reconciles — edit, post, and
delete the surplus tail via `delete_bracket_messages_from`.
Design: §8.6.
Gate: no draw below two entrants; byes on the top seeds for a non-power-of-two field; the order following
suggested seeding rather than registration; the tail delete removing exactly the surplus and nothing from
another tournament.

### Invited entrants and invite-only events (chunks 30–34)

Four chunks plus one optional, in this order. **30 first** because it fixes a bug that exists today and depends
on nothing; **31 before 32** because an invite cannot insert against a `not null` column; **33 after 32** because
invite-only with no `invite` verb is a tournament nobody can enter; **34 last** because it is the only piece whose
cost is arguably not worth paying, and it is written down so it can be dropped the way 13 and 15 were.

**30. A hand-made seed order survives the rating pass**
`tournaments.seed_source` (`'suggested' | 'manual'`, borrowing `atr_source`'s vocabulary), a pure
`seeding::SeedPolicy`, and `refresh_ratings` taking it: ratings are always refreshed, only the ordering branches.
`KeepManual` feeds `seeding::display_order` back through `set_seed_order(.., false)`, which preserves the
organizers' relative order **and compacts it**, so `start`'s 1..n requirement holds with no separate renumber.
`seed set` writes `'manual'`, `seed refresh` writes `'suggested'` — which makes its "discards any override" doc
comment true — and `clear_checkins` splits so `reopen-registration` stops nulling a manual order.
**Fixes a live bug**: `seed set` has no status gate, so an order set during `registration` is destroyed by
`close-checkin`'s seeding pass and by `reopen-registration`, both silently, invites or no invites.
Design: §6 ("A hand-made order outlives the rating pass"), §8.3.
Gate: the policy mapping as a pure function; `KeepManual` preserving relative order while closing a gap a
no-show left; `Suggest` still discarding an override; the new outcome rendering in both locales.

**31. `aoe4_id` becomes optional**
No new command and nothing user-visible — the schema simply starts permitting an unbound entrant that nothing yet
creates. `Option<i64>` on both row types, guards where the ratings pass assumed a profile (skip the write
entirely for an unbound entrant rather than storing `(None, None, None)`), `snapshot_entry_elo` widened so its two
call sites need no guard of their own, and three fixes the nullability exposes: `register` currently refuses an
unbound player who supplies a profile, `unbind` should answer "not bound" rather than "blocked by entries", and
`rebind` should finally write the display name through `db::set_player_display_name`, which has had no production
caller since it was written.
**This migration is a table rebuild, not an `alter table`, and cannot run in sqlx's transaction** — see §4's
notes for why and for the exact shape. It is the highest-risk commit in this group by a distance.
Design: §4 (the schema block and its notes on nullable `aoe4_id`).
Gate: the migrator against a **populated** database preserves every row and leaves `pragma foreign_key_check`
clean — every existing migrator test starts from empty, so none of them would catch a data-losing rebuild. Also:
many null `aoe4_id`s accepted while a duplicate real one is still rejected; foreign keys enforced again
afterwards; the ratings pass making no HTTP call for an unbound entrant.

**32. `/tournament invite` and `/tournament uninvite`**
`tournament_entries.invited_by`, and the no-show sweep skipping entries that have one — with the reasoning
recorded, because stamping `checked_in_at` instead is the tempting version and it unravels twice.
`db::invite_player_and_entry` mirrors `register_new_player_and_entry`: one transaction, null `aoe4_id`. The
optional seed goes through `seeding::reorder` + `set_seed_order`, never a direct `seed` write, so
`unique (tournament_id, seed)` is never touched. `in_game_name` is required free text — not the profile
autocomplete, whose whole purpose is resolving a profile that here does not exist. `uninvite` is scoped to
invited entries and re-writes the order so the field stays startable. `set_thread::Player` gains `verified`, and
the seat line marks an unverified name: chunk 16's panel presents that name as the one to search for in the lobby
browser, which is the only landed code whose *behaviour*, not just its types, is wrong for an unbound entrant.
Design: §8.3 ("Invited entrants, and an invite-only field"), §8.4, §8.5, §8.7.
Gate: §10 — an invitee survives `close-checkin` without checking in while a self-registered no-show does not; the
check-in counter excludes invitees; an invite past the cap is refused; `uninvite` refuses a self-registered entry;
re-inviting updates the name everywhere it is shown; a seeded invite leaves the field contiguous.

**33. Invite-only registration**
`tournaments.registration_mode`, and one pure three-state type replacing the `open: bool` that the registration
gate and the three panel renderers currently share — so the gate and what the panel advertises cannot disagree.
`RegisterOutcome` gains a variant that explains invite-only rather than reusing "registration is closed", whose
wording sends people looking for a reopen. **Register disables while Withdraw stays live**, the first state where
the two buttons differ. Configured through `/tournament setup`, which already owns per-tournament settings and
already reports them. Fixes a live bug on the way: `panel::post_initial` hardcodes the open state, so
`/tournament refresh` already reposts an OPEN registration panel for a tournament in `checkin`.
Design: §8.3, §8.5, §8.4, §4.
Gate: the three-state resolution as a pure function; a sign-up and the button both refused with the invite-only
wording; withdrawal still permitted; the panel rendering all three states; `/tournament refresh` reposting the
panel in the state the tournament is actually in.

**34. Invite-only skips check-in** *(optional; drop if the wart below outweighs it)*
A pure `checkin::closeable(status, invite_only)`, `close` skipping the sweep when it ran from `registration`, and
`/tournament lock` as a second entry point to the same code rather than a second implementation of it — the
pattern §8.7 already uses for `/set done` and its button. Needs `panel::refresh_now` on that path, since the
refresh that closes the registration panel lives only in `open-checkin` today.
**Accepted wart:** `/tournament refresh` will post a closed, empty check-in panel for an event that never ran
check-in, because panel expectations are derived from status and the alternative skips exactly the case that
needs repair. Recorded in §8.3.
Design: §8.3 (the `registration ──lock──▶ seeding` edge).
Gate: the gate as a pure function — closeable from `checkin` always, from `registration` only when invite-only,
never otherwise; no entry marked `no_show` on that edge; the registration panel closed afterwards.

## Phase E — the draft tool

**14. Draft-tool client**
The authenticated session (cookie store, Auth.js credentials handshake, re-auth on 401), `POST /api/matches`,
and reading a preset's config. Base URL and credentials from env, alongside `DISCORD_TOKEN`.
Design: §3.3.
Gate: preset-config deserialization from a saved payload; the handshake against a stub. No live calls.

**15. Round presets: the tool's own validation — dropped, not built**
It would have run `validatePreset` (`lib/draft/validate.ts`) when a round was configured, so a preset the tool
considers unplayable was caught before two players were waiting on it. The tool offers no way to do that.

`validatePreset` runs in exactly one place — inside `POST /api/matches` — and that call creates a room on
success. There is no validate endpoint, and `app/api/matches/[id]` is GET-only, so every probe would leave a
room nobody can delete. The alternative, porting the rules to Rust, means modelling the whole step/civ/map
config that chunk 27 deliberately left to the tool (§2), reproducing about ten interacting checks that are still
being fixed upstream, and drifting silently from then on — and a port that gets a rule wrong rejects **valid**
presets, which is worse than not checking.

What already covers it: the tool's own editor warns while a preset is being authored, chunk 27 checks the three
properties the bot depends on, and chunk 14 keeps the tool's `issues` in `DraftError::PresetRejected`. So a
malformed preset is reported when it bites rather than prevented beforehand.

**16. Set threads and draft creation**
Thread on `ready`, members added, draft created as the bot's account, pinned panel carrying the room link and
the seat instruction with both players mentioned.
Design: §8.7.
Gate: thread names stay within 100 characters for worst-case names, using chunk 5's width helper.

**17. Draft channel announcement**
One post per set into `#…-draft` the moment the room is created, in the same call — the round, both seeds and
names, and the `/watch/` link in a link button, with no mentions and an empty `allowed_mentions` so a display
name cannot smuggle one. `draft_announce_message_id` is kept as the **handle** chunks 20 and 22 repoint, not as a guard.
**No polling.** `open` is the only place a room is minted and it no-ops on a set that has a thread, so
structural idempotency replaces the seat poll — which removes the second scheduler §7 wanted and the last use of
`GET /api/matches/<id>`. Adds `bracket::round_name_bilingual` for a surface with no reader locale (§8.10).
Anonymous presets are **not** supported, and §8.7 records why half-supporting them does not work.
Design: §8.7 ("Announcing the draft").
Gate: §10 — the watch link and never the room link, no mention syntax, both names escaped, a round name doubled
only where a translation exists, the url in a button; plus the handle round-tripping in the database and being
cleared by a redraft. Also fixes chunk 16's panel, which interpolated player names into markdown and into an
inline code span unescaped.

**18. Set completion and advancement**
The completion transaction: winner, loser eliminated, winner written into the next set's slot, target set flipped
to `ready`, bracket edited, thread archived and locked, next thread created. Driven by reported games; the import
path in chunk 22 reuses all of it.
Design: §7 ("Set completion"), §8.7.
Gate: §10 — a set reaching a majority of its games completes and places the winner in the correct slot;
completion derived from score against `target`, never from a status field.

**19. `/set report` — the manual path**
Organizer override writing `source = 'manual'` rows, plus `/set schedule`.
Design: §3.7, §7 ("Fallback — manual"), §8.4.

> **Chunk 19 is the first shippable milestone.** With it the bot runs a whole tournament in its own guild —
> registration, check-in, seeding, bracket, threads, drafts created on the tool, results entered by hand — and
> needs nothing from the draft tool's API beyond what already exists. Everything after this replaces hand-entry
> with import.

**20. `/set redraft`**
Overwrite the pointer, increment `redraft_count`, clear the sync and announcement state, re-post the panel, and
the three guards.
Design: §8.7 (`/set redraft`), §4.
Gate: §10 — refused on a completed set, voids that set's `draft_import` games while preserving `manual` ones,
re-points the announcement.

**21. [Other repository] the read endpoint**
`GET /api/v1/drafts/:id` in `aoe4_banpick` — nine fields, a thin wrapper over `deriveState()`. Not a commit in
this repo, and the only external gate in this plan. Whether we run it on a branch of our own or offer it upstream
is still open (§12).
Design: §3.2 item 1.

**22. Result import and `/set done`**
The payload type, slot mapping, the upsert into `tournament_games`, `/set done` and its thread button, and the
background poll on its own schedule — the existing cron is twice daily, far too coarse, and its `.unwrap()`
panics the job.
Design: §7, §8.7.
Gate: §10's import list — swapped slots both ways, re-import overwrites `draft_import` and preserves `manual`,
`status = "running"` with a clinching score treated as complete, `setdone` on an unfinished draft changes nothing.
Write the import against saved fixtures so this chunk can be built and tested before chunk 21 exists; only the
live fetch is blocked.

**23. Boot-time panel reconciliation**
Confirm each stored panel message still exists on startup and recreate it if an organizer deleted it.
If chunk 33 has landed, derive the registration panel's state from its three-state resolution rather than
hardcoding the open one — that is the bug chunk 33 fixes in `panel::post_initial`, and a reconciler that reposts
from scratch is the obvious place to reintroduce it.
Design: §8.5.

## Phase F — localization

Numbered before 25 and 26 but **scheduled after them** — all three land right after chunk 10, and every chunk
after 24 must use it for new text from the start.

**24. Localization (zh-TW, English default)**
A `Locale` enum and `from_discord_locale`, resolved per-interaction from `Context::locale()`
(slash commands) / `ComponentInteraction.locale` (buttons) — never `guild_locale`. Retrofits the outcome
messages of chunks 7–10, 25 and 26, `access.rs`'s refusals, `commands.rs`'s own replies, and the shared
`errors.rs`/`guilds.rs` notices (neither is home-guild-only) to take a `Locale`. Both panels go **bilingual**
instead, keeping no `locale` parameter: they are shared messages that re-render on every button press, so a
per-reader language would flip. Collapses the ten identical wrong-channel refusals into
`resolve_tournament_by_channel`, which would otherwise have become ten two-language pairs.
Design: §8.10.
Gate: §10's localization list — `"zh-TW"` and only `"zh-TW"` resolves to `Locale::ZhTw`; `"zh-CN"`, empty, and
an unrecognized code all fall back to `Locale::En`; every retrofitted message renders correctly in both.

## Not in this plan

Swiss, group stage, round robin and double elimination (§1 "Designed for, not built now"); team tournaments;
the catalog endpoint, seat assignment and the completion webhook (§3.2 items 2–4); Discord login on the tool
(§3.6); per-guild configuration beyond the hardcoded ids (§8.0); everything under §11 "Follow-ups".
