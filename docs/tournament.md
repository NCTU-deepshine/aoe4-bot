# Tournament orchestration — design

Status: **design only.** Nothing in this document is implemented yet. It exists so that implementation is
mechanical, and so the API requirements in [§3](#3-the-external-draft-tool) can be handed to the draft tool's
author as-is.

## 1. Scope

The bot today maps Discord users to aoe4world profiles (`accounts`) and reminds people who stop laddering
(`reminders`). This design adds **tournament management**: create an event, register entrants, seed them from
ratings, generate a bracket, link each match to a draft on the external ban/pick tool, and record results.

**In scope now**

- Single elimination, fully pre-generated bracket with advancement links.
- Per-round rules (a Bo3 bracket with a Bo5 final is configuration, not code).
- Entrants seeded from ATR and ELO, with organizer override.
- Results imported from the draft tool, with a manual override path.
- Discord orchestration (§8): channel and thread layout, admin roles, a check-in gate, button-driven
  registration and check-in, bracket publication, and a private thread per set.

**Designed for, not built now**

- Swiss, group stage, round robin, double elimination.

**Out of scope**

- Team (2v2+) tournaments.

### Decisions

| Topic | Decision |
|---|---|
| Bracket | Single elimination, fully pre-generated with advancement links |
| Round rules | Per-round, not per-tournament |
| Draft tool | External; the bot stores a pointer and queries draft content on the fly |
| Planning assumption | **The draft-tool APIs in §3.2 will exist by ship time** |
| Snipe | Removes a civ the opponent already picked |
| Bans | Per-player |
| Map order | Loser of game N picks the map for game N+1 — enforced by the tool |
| Civ list | Not stored; always the full list |
| Map list | Sourced from the draft tool |
| Ratings | ATR (external tournament Elo) and ELO (`rm_1v1_elo`) |
| Seeding | Bot suggests; organizer finalizes and may override any seed |
| Registration | Requires a bound account; own channel with a button panel and explicit feedback (§8.5) |
| Per-game record | Map, both civs, winner |
| Check-in | Required, and must close before start — the bracket is built from checked-in players only (§8.3) |
| Channels | `/tournament create` uses the invoking channel for announcements and creates the rest in its category (§8.1) |
| Admins | Creator is the first admin and may add others (§8.2) |
| Set spaces | One private thread per set, players + admins added (§8.7) |
| Bracket view | ASCII bracket in a code block, edited in place (§8.6) |
| Result trigger | `/set done` or a thread button, plus a background poll (§7) |

## 2. Division of responsibility

The guiding principle: **do not duplicate state the draft tool owns.** Every rule the tool already enforces is
a rule we would eventually enforce differently, and then disagree with it.

| Owned by the draft tool | Owned by the bot |
|---|---|
| Map pool and per-player map bans | Tournament identity, stages, rounds, rules |
| Civ bans, offers, picks, snipes | Entrants, ratings, seeds, bracket topology |
| Step order (defined by the preset) | Which two players a set is between, and where the winner goes |
| Per-game map selection, game results | A queryable projection of results for standings and stats |
| Whether a civ may be replayed | Discord orchestration and announcements |

Consequences, each of which removes a table or column someone would otherwise reach for:

- No civ table — the list is fixed and already hardcoded in `src/aoe4world.rs`.
- No event-level map pool — the draft preset defines the pool per draft.
- No `map_bans_per_player`, no `allow_civ_repeat` — preset settings.
- No draft-action table — steps are fetched on demand, never mirrored.
- No map-order validation — `MAP_SELECT` is the tool's business.

`tournament_games` is the one place the bot keeps a copy of the tool's output. It is a **projection**, not a
second source of truth: populated by import, refreshable, and used so standings and civ/map statistics are
SQL-queryable without a network call.

## 3. The external draft tool

`https://aoe4banpick-production.up.railway.app` — "Tournament drafting for Age of Empires IV". Drafts are
identified by a 24-hex id and exposed at two URLs: `/match/<id>` (participant room) and `/watch/<id>`
(spectator). Organizers will usually have the `/watch/` link.

### 3.1 Verified behavior

Established by reading the client bundles and probing the live service.

**Step types** — use these names verbatim:

```
MAP_BAN   MAP_PICK   MAP_SELECT
CIV_BAN   CIV_OFFER  CIV_PICK   CIV_SNIPE_OPPONENT
GAME_RESULT
```

Item states are `banned`, `picked`, `drafted`. Draft status values include `LOBBY`, `PAUSED`, `SYNC_CONFIRM`.

`GAME_RESULT` and `MAP_SELECT` being step types is the important detail: **the tool drives the whole set**, not
just a pre-match draft. Per-game results and the loser's next-map pick happen inside it. Our "loser picks the
next map" rule is already implemented there.

**Presets are user-defined** and carry both the flow and the match length — e.g. `Standard Bo3 — map BP, hand
draft, simultaneous offer & snipe`. No fixed phase order may be hardcoded anywhere in the bot.

**Identifiers are kebab-case.** 23 civs, all treated as regular civs:

```
abbasid-dynasty  ayyubids       byzantines           chinese
delhi-sultanate  english        french               golden-horde
holy-roman-empire  house-of-lancaster  japanese      jeanne-darc
jin-dynasty      knights-templar  macedonian-dynasty  malians
mongols          order-of-the-dragon  ottomans       rus
sengoku-daimyo   tughlaq-dynasty  zhu-xis-legacy
```

The tool marks some of these as variants of a parent civ (`ayyubids` of `abbasid-dynasty`, `zhu-xis-legacy` of
`chinese`, and so on). **We ignore that relationship** — a variant is just another civ id. Do not model,
store, or group by it.

11 maps in the current default pool:

```
baldland  coastal-cliffs  frisian-marshes  front-range  holy-island  kawasan
mountain-clearing  pigeons-view  prairie  rockies  socotra
```

> **Key-format mismatch.** The tool uses kebab-case; aoe4world and our civ table at `src/aoe4world.rs:35-64`
> use snake_case (`abbasid-dynasty` vs `abbasid_dynasty`). Replacing `-` with `_` covered every case checked,
> but do not rely on it — build an explicit mapping table and test it against both vocabularies.

**Current API surface.** There is one undocumented endpoint, `GET /api/matches/<id>`, which returns `200` for
both `/match/` and `/watch/` ids (one id space). It carries lobby metadata only:

```json
{"id":"…","status":"running","shareCode":"ec8801f3","hostId":"…",
 "hasPlayer1":true,"hasPlayer2":true,"isHost":false,"viewer":null}
```

Every content sub-resource returns 404 (`/state`, `/steps`, `/actions`, `/events`, `/draft`, `/snapshot`,
`/summary`, `/result`, `/players`). Draft content arrives over **Ably** realtime, authenticated through
`/api/socket-token` (401 unauthenticated), and both pages hydrate client-side, so nothing is available
server-side either.

**Do not build against `/api/matches/<id>`.** It is undocumented, unversioned, and carries no draft content.

### 3.2 Required APIs

Items 1 and 2 are the hard dependency for shipping. Items 3 and 4 turn recording into orchestration. Paths are
proposals.

#### Item 1 — spectator draft read (required)

`GET /api/v1/drafts/:id` — unauthenticated (the watch link is already public), read-only, supporting
`ETag`/`If-None-Match` so the bot can poll cheaply.

```jsonc
{
  "id": "6a70814a15132eb72a04e531",
  "shareCode": "fe6200bf",
  "status": "lobby|running|paused|completed|abandoned",
  "updatedAt": "2026-08-03T10:15:00Z",
  "completedAt": null,
  "preset": { "name": "Standard Bo3", "bestOf": 3,
              "summary": "map BP, hand draft, simultaneous offer & snipe" },
  "players": [
    { "slot": 1, "name": "MarineLorD", "externalRef": "1102458" },
    { "slot": 2, "name": "Dragevann", "externalRef": "4071458" }
  ],
  "mapPool": ["prairie", "socotra", "kawasan"],
  "games": [
    { "number": 1, "map": "prairie",
      "civBySlot": { "1": "english", "2": "rus" }, "winnerSlot": 1 }
  ],
  "result": { "winnerSlot": null, "scoreBySlot": { "1": 1, "2": 0 } },
  "steps": [
    { "ordinal": 1,  "type": "MAP_BAN", "actorSlot": 1, "targetSlot": 2,
      "value": "holy-island", "executedAt": "…" },
    { "ordinal": 7,  "type": "CIV_PICK", "actorSlot": 2, "value": "rus" },
    { "ordinal": 9,  "type": "CIV_SNIPE_OPPONENT", "actorSlot": 1, "targetSlot": 2,
      "value": "rus", "targetsOrdinal": 7 },
    { "ordinal": 12, "type": "MAP_SELECT", "actorSlot": 2, "value": "socotra", "gameNumber": 2 },
    { "ordinal": 13, "type": "GAME_RESULT", "gameNumber": 1, "winnerSlot": 1 }
  ]
}
```

Why each part matters:

- **`games` and `result` are what the bot consumes.** `steps` is the audit trail, rendered on request. Having
  both means the bot never reimplements preset rules to derive a score.
- **`externalRef` per player is the most valuable field in the payload.** When the bot creates the draft
  (item 3) it sets each ref to our `aoe4_id`, which makes slot→entrant mapping exact and eliminates name
  matching entirely. Echoing back a caller-supplied string is cheap and removes our largest source of
  ambiguity.
- **`targetsOrdinal`** on `CIV_SNIPE_OPPONENT` makes the snipe→pick relationship explicit rather than inferred
  from ordering.
- **`status`, `updatedAt` and `ETag`** are what make polling acceptable rather than wasteful.

#### Item 2 — catalog read (required)

`GET /api/v1/catalog`

```jsonc
{ "civs": [ { "id": "ayyubids", "name": "Ayyubids" } ],
  "maps": [ { "id": "prairie", "name": "Prairie" } ] }
```

A `variantOf` field, if the tool exposes one, is ignored — see §3.1.

Removes the need to hardcode 23 civs and a seasonal map pool, and keeps ids authoritative when a DLC civ or map
rotation lands. Cache locally with a long TTL.

#### Item 3 — draft creation (desirable)

`POST /api/v1/drafts`, API-key authenticated.

```jsonc
// request
{ "presetName": "Standard Bo3",
  "players": [ { "slot": 1, "name": "MarineLorD", "externalRef": "1102458" },
               { "slot": 2, "name": "Dragevann", "externalRef": "4071458" } ],
  "externalMatchRef": "tournament:relic-cup:set:42" }

// response
{ "id": "…", "watchUrl": "…", "joinUrls": { "1": "…", "2": "…" } }
```

This is what lets `/set draft` hand each player a private join link in Discord, instead of an organizer
building the draft by hand and pasting a URL back. `externalMatchRef` lets a draft be reconciled to a set even
if our pointer is lost.

#### Item 4 — completion webhook (optional)

`POST` to a configured URL on status change, with item 1's body as the payload.

**Caveat:** the bot has no HTTP listener. `fly.toml` declares an `[http_service]` on port 8080 but nothing
binds it, so a webhook means standing up a server and a public route. Polling item 1 from the existing
`tokio-cron-scheduler` job (`src/main.rs:503`) plus an on-demand refresh command is the cheaper first cut.
Treat the webhook as a later optimization.

### 3.3 Fallback

Even with the above shipped, the bot needs an organizer override for sets played outside the tool, abandoned
drafts, and API outages: a report command writing the same `tournament_games` rows with
`source = 'manual'`. This is a fallback, not the primary path — see [§7](#7-result-flow).

## 4. Data model

Six tables plus one rating cache. Conventions follow the existing schema: lowercase SQL, `integer primary key
autoincrement`, Discord snowflakes and aoe4 ids as `bigint`, timestamps written with `datetime('now')`.

```sql
-- 1. the event. deliberately thin: rules live per-round, not here.
create table if not exists tournaments (
  id integer primary key autoincrement,
  slug text not null unique,
  name text not null,
  status text not null default 'registration'
    check (status in ('registration','checkin','seeding','running','completed','canceled')),
  draft_base_url text,
  -- discord wiring; see §8.1. announce_channel_id is the channel /tournament create ran in.
  announce_channel_id bigint,
  category_id bigint,
  register_channel_id bigint,
  register_message_id bigint,
  bracket_channel_id bigint,
  matches_channel_id bigint,
  checkin_message_id bigint,
  checkin_closes_at timestamp,
  created_by bigint not null,               -- authority over the admin list, see §8.2
  created_at timestamp not null default (datetime('now')),
  started_at timestamp,
  completed_at timestamp
);

-- 2. stages: the extension point for swiss / group / double-elim.
--    single elimination today = exactly one stage with format 'single_elim'.
create table if not exists tournament_stages (
  id integer primary key autoincrement,
  tournament_id integer not null references tournaments(id) on delete cascade,
  ordinal integer not null,
  name text not null,                       -- 'Main Bracket', 'Group Stage', 'Playoffs'
  format text not null default 'single_elim'
    check (format in ('single_elim','double_elim','swiss','group','round_robin')),
  config text,                              -- json: format-specific knobs
  status text not null default 'pending'
    check (status in ('pending','running','completed')),
  unique (tournament_id, ordinal)
);

-- 3. rounds: where per-round rules live.
create table if not exists tournament_rounds (
  id integer primary key autoincrement,
  stage_id integer not null references tournament_stages(id) on delete cascade,
  ordinal integer not null,
  name text not null,                       -- 'Ro16', 'Quarterfinal', 'Final', 'Swiss R1'
  best_of integer not null,
  bracket text                              -- double elimination, later
    check (bracket in ('winners','losers','grand_final')),
  draft_preset text,                        -- the tool's preset name for this round
  rules text,                               -- json: other per-round overrides
  unique (stage_id, ordinal, bracket)
);

-- 4. entrants. bound accounts only (accounts.aoe4_id is unique, so it is a valid fk target).
create table if not exists tournament_entries (
  tournament_id integer not null references tournaments(id) on delete cascade,
  aoe4_id bigint not null references accounts(aoe4_id),
  seed integer,                             -- FINAL seed; the organizer may override
  suggested_seed integer,                   -- what the bot computed, kept for audit
  display_name text not null,               -- snapshot; aoe4world names change
  elo integer,                              -- snapshot of rm_1v1_elo.rating
  atr real,                                 -- snapshot of esports tournament elo
  atr_source text check (atr_source in ('esports','manual')),
  status text not null default 'active'
    check (status in ('active','eliminated','withdrawn','no_show')),
  registered_at timestamp not null default (datetime('now')),
  checked_in_at timestamp,                  -- null = did not check in; see §8.3
  primary key (tournament_id, aoe4_id),
  unique (tournament_id, seed)
);

-- 4b. cache of aoe4world's esports (tournament elo / ATR) leaderboard, keyed by
--     profile_id, which is our aoe4_id. no fk: we may cache pros who never bound an account.
create table if not exists esports_ratings (
  aoe4_id bigint primary key,
  name text not null,
  rating real not null,
  rank integer,
  active_rank integer,
  is_active integer,
  country text,
  liquipedia_name text,
  fetched_at timestamp not null default (datetime('now'))
);

-- 5. a set = one Bo_N meeting between two entrants.
--    loser_advances_to_* is unused by single elimination but present so
--    double elimination needs no migration.
create table if not exists tournament_sets (
  id integer primary key autoincrement,
  tournament_id integer not null references tournaments(id) on delete cascade,
  round_id integer not null references tournament_rounds(id) on delete cascade,
  position integer not null,                -- index within the round, 1-based, top to bottom
  slot1_aoe4_id bigint references accounts(aoe4_id),   -- null until fed by a previous round
  slot2_aoe4_id bigint references accounts(aoe4_id),
  slot1_wins integer not null default 0,
  slot2_wins integer not null default 0,
  winner_aoe4_id bigint references accounts(aoe4_id),
  status text not null default 'pending'
    check (status in ('pending','ready','drafting','in_progress','completed','bye','walkover')),
  draft_external_id text,                   -- the tool's 24-hex id; content fetched on demand
  draft_url text,
  draft_slots_swapped integer not null default 0,   -- 1 if the tool's slot 1 is our slot 2
  draft_synced_at timestamp,
  thread_id bigint,                         -- the set's private thread; see §8.7
  winner_advances_to_set_id integer references tournament_sets(id),
  winner_advances_to_slot integer check (winner_advances_to_slot in (1,2)),
  loser_advances_to_set_id integer references tournament_sets(id),
  loser_advances_to_slot integer check (loser_advances_to_slot in (1,2)),
  scheduled_at timestamp,
  completed_at timestamp,
  unique (round_id, position)
);

-- 6. games: a projection of the draft's GAME_RESULT steps, or a manual override.
create table if not exists tournament_games (
  id integer primary key autoincrement,
  set_id integer not null references tournament_sets(id) on delete cascade,
  game_number integer not null,
  map text,                                 -- draft-tool map id, kebab-case
  slot1_civ text,                           -- draft-tool civ id, kebab-case
  slot2_civ text,
  winner_aoe4_id bigint references accounts(aoe4_id),
  status text not null default 'pending'
    check (status in ('pending','in_progress','completed','void')),
  source text not null default 'draft_import'
    check (source in ('draft_import','manual')),
  reported_by bigint,                       -- discord user_id, when source = 'manual'
  reported_at timestamp,
  unique (set_id, game_number)
);
```

### Notes

- **`slot1`/`slot2`, not `p1`/`p2`.** Slots exist before players do, and `winner_advances_to_slot` /
  `loser_advances_to_slot` use the same numbering.
- **`tournaments` has no `best_of`.** A Bo5 final is the last round's `best_of`. A Swiss stage with Bo1 early
  rounds and Bo3 late rounds is expressible with no schema change. This is the whole reason rounds are a table.
- **`stage.config` and `round.rules` are JSON escape hatches** so a new format's knobs don't each require a
  migration. Concepts shared across formats (`best_of`, `bracket`) stay real columns.
- **`draft_slots_swapped`** covers an organizer creating the draft by hand with player order not matching our
  slots. It becomes unnecessary once the bot creates drafts and sets `externalRef`, but one integer is cheap
  insurance against silently attributing a win to the wrong player.
- **No `map_picked_by` column.** `MAP_SELECT` records who picked, in the draft.
- **Foreign keys.** sqlx enables `pragma foreign_keys` by default on `SqliteConnectOptions`. Assert this in a
  test — every `references` above is inert if it ever changes.

## 5. Bracket generation

Pure functions, no database access, so this is unit-testable in isolation.

1. `bracket_size(n) = n.next_power_of_two()`; round count is `log2(size)`.
2. Seed slot order by reflection: start with `[1, 2]`; to double from size `s` to `2s`, map each entry `x` to
   `[x, 2s + 1 - x]`. Size 8 gives `[1,8,5,4,3,6,7,2]`, so round 1 is `(1,8) (5,4) (3,6) (7,2)` — top seeds
   meet bottom seeds and cannot meet each other before the final.
3. Seeds beyond `n` are absent, leaving a one-player set with `status = 'bye'` that auto-advances at start.
4. Create one `tournament_rounds` row per round, then all sets, then link advancement: `position` p in round r
   feeds `ceil(p/2)` in round r+1, into slot 1 if p is odd and slot 2 if even.
5. Input is the **finalized** `seed` values. Generation is gated on tournament status `seeded`, so it never
   computes an order itself and an organizer override is respected by construction.

## 6. Ratings and seeding

Two numbers, both from aoe4world, both snapshotted onto the entry at seeding time.

| Purpose | Endpoint |
|---|---|
| ELO | `GET /api/v0/players/:profile_id` → `modes.rm_1v1_elo.rating` |
| ATR | `GET /api/v0/esports/leaderboards/1?profile_ids=<a,b,c>` |

**ATR comes from the aoe4world esports leaderboard, not from the source spreadsheet.** The community sheet
([AoE4 Esport Tournament Ranking](https://docs.google.com/spreadsheets/d/12CKvt3uO1NWBL3DsBN0adcynPUcuOIpCkobvgtymJq8/edit?gid=1327598362),
`ATR Rank` tab, `TR` column) is the upstream, but it identifies players by name only — no ids on any tab — and
its CSV export has a two-row header with spreadsheet scratch columns. aoe4world mirrors it every 24 hours keyed
by `profile_id`, which is already our `aoe4_id`, with identical values (MarineLorD `2292.531382` in both). The
sheet is maintained by Andrey "ISanych" (`@isanych_aoe`); credit the source wherever ATR is displayed.

The `profile_ids` filter means ATR for an entire field is **one request**, regardless of size. A full sync is
`?page=N` with `per_page: 50` over `total_count: 347`, and is only worth doing behind an explicit refresh
command or the existing cron — never per command.

Most guild entrants will have no ATR at all; the leaderboard is 347 professional players. That is expected, not
an error.

### Suggested seeding

ATR (roughly 1000–2292, tournament-derived) and ELO are **different scales** and must never be blended into a
single sort key. The default order is tiered:

1. entrants with an `atr`, by `atr` descending;
2. then entrants without, by `elo` descending;
3. ties broken by `display_name`, for deterministic tests.

This writes `suggested_seed` and copies it to `seed`. The organizer then reviews a table showing both numbers
per entrant and may reassign any seed; only `seed` is authoritative. The tiering is a defensible default for a
mixed professional/guild field, **not** a claim that ATR and ELO are comparable — say so in the command output
as well as here.

### Reuse

`src/ranked.rs:181-185` and `:221-225` are already two copies of the same player fetch. Extract a shared
`fetch_profile` rather than adding a third call site. Note `src/ranked.rs:188-189` applies `?` to both modes,
so a player with no ELO entry is dropped entirely; seeding must tolerate `None`, which is why every rating
column is nullable.

## 7. Result flow

**Primary path — import.** A set holds `draft_external_id`. On a poll tick or an on-demand refresh, fetch
item 1, map the draft's slots onto ours (via `externalRef`, or `draft_slots_swapped` for hand-made drafts),
and upsert `games` into `tournament_games` with `source = 'draft_import'`. Stamp `draft_synced_at`.

**Two triggers, one code path:**

1. **On demand** — `/set done`, or the `✅ Set complete` button on the set's thread panel (§8.7). Syncs, imports,
   posts the result in the thread and the bracket channel, advances the winner, then archives and locks the
   thread and creates the next set's thread if it has become `ready`.
2. **Background poll** — on the existing `tokio-cron-scheduler` job (`src/main.rs:503`), over sets in
   `drafting`/`in_progress` that have a `draft_external_id`, so a forgotten report never stalls the bracket.
   The cron closure builds its own `Http` and `Data` (`src/main.rs:506-520`); the same shape works here, though
   its `.unwrap()`s panic the job on failure and should be handled instead.

Because the draft tool is authoritative, an on-demand sync of an unfinished draft is a no-op that reports
"still in progress" — which is why the button needs no confirmation step and no winner-only restriction.

**Fallback — manual.** A report command writes the same rows with `source = 'manual'` and
`reported_by`/`reported_at` set.

**Re-import is safe and idempotent**: it overwrites `source = 'draft_import'` rows and never touches
`source = 'manual'` ones, so an organizer correction survives every subsequent sync.

**Set completion**, in one transaction:

- A set completes at `ceil(best_of / 2)` wins.
- Set `winner_aoe4_id`, `completed_at`, `status = 'completed'`; mark the loser's entry `eliminated`.
- Write the winner into `winner_advances_to_set_id` at `winner_advances_to_slot`.
- Flip that target set from `pending` to `ready` once both its slots are filled.

**Prefer the draft's `result.scoreBySlot` over recomputing from games.** If the two disagree, flag it for an
organizer rather than silently choosing one — a mismatch means either our import or their state machine is
wrong, and both are worth knowing about.

Civ and map values must be valid catalog ids (item 2). **Legality within the draft is the tool's business**, so
the bot does not check whether a civ was available or a map was banned.

## 8. Discord orchestration

How an organizer actually runs an event. Every Discord API named here was checked against the vendored
serenity 0.12.5 source, not assumed.

| Need | API |
|---|---|
| Channel in a category, with per-member overwrites | `CreateChannel::new(name).kind().category().permissions(vec![PermissionOverwrite{allow, deny, kind}])` |
| Private thread | `GuildChannel::create_thread` + `CreateThread::new(name).kind(ChannelType::PrivateThread).invitable(false)` |
| Add/remove thread members | `Http::add_thread_channel_member` / `remove_thread_channel_member` |
| Archive a finished set | `EditThread::new().archived(true).locked(true)` |
| Buttons | `CreateButton` / `CreateActionRow` (`builder/create_components.rs`) |
| Persistent button handling | `EventHandler::interaction_create` |
| Auto-archive | `AutoArchiveDuration::OneWeek` |

**Platform limits that shape the design:** 50 channels per category, 500 per guild, 1000 active threads per
guild, 100-char thread names, 2000-char messages.

**Bot permissions required:** Manage Channels, Manage Threads, Create Private Threads, Send Messages, Send
Messages in Threads, Read Message History.

### 8.1 Channels and threads

`/tournament create name:"Relic Cup" slug:relic-cup`, run in `#relic-cup`:

```
category: Relic Cup                 <- the invoking channel's parent_id
  #relic-cup                        <- announcement channel = the invoking channel
  #relic-cup-register               <- created; player action panels live here
  #relic-cup-bracket                <- created; read-only to @everyone
  #relic-cup-matches                <- created; read-only parent for set threads
       ├ 🧵 R1M1 · MarineLorD vs Beasty   <- private thread per set
       └ 🧵 SF1 · MarineLorD vs Anotand
```

- The category comes from the invoking channel's `parent_id`. If that channel is top-level, create the siblings
  uncategorized and say so in the reply rather than failing.
- Names are slug-prefixed so several tournaments can share one category.
- `#…-bracket` and `#…-matches` deny `SEND_MESSAGES` to `@everyone` — they are output surfaces.
- **Split of concerns:** `#…-register` holds the *interactive* panels and is where players act; the announcement
  channel holds narration (check-in is open, set results, bracket updates) and links to the panels. This keeps a
  busy panel channel from burying announcements.
- **Set threads are created lazily**, when a set reaches `ready`: all of round 1 at start, later rounds as
  results land. A 32-player bracket has 31 sets, so creating them up front would be wasteful and would clutter
  the thread list; this keeps threads near the active frontier.
- On completion a thread is **archived and locked** — it stops counting against the active-thread cap while
  staying readable.

### 8.2 Admins

New table `tournament_admins` (§8.7); the creator is inserted at create time, and `tournaments.created_by`
remains the authority over the admin list.

- **Creator only:** `/tournament admin add|remove`.
- **Any admin:** open/close check-in, seed, start, cancel, draft, manual report, schedule.
- **Anyone:** register, withdraw, check in, view bracket.
- **Guild `MANAGE_GUILD` bypasses the admin check.** This is a policy choice, not a technical necessity: without
  it, a tournament whose creator has left the server is unrecoverable. The cost is that any server admin can act
  on any tournament.

This feature introduces **the first access control in the codebase**. There is no use of
`required_permissions`, `guild_only`, `owners_only` or `ephemeral` anywhere in `src/` today, and
`FrameworkOptions` (`src/main.rs:476`) has no `on_error` handler. All of it arrives here. Worth noting the
existing `/refresh`, which deletes every message in the rank channel, is currently callable by anyone.

### 8.3 Lifecycle

```
registration ──/tournament open-checkin──▶ checkin
checkin ──/tournament close-checkin──▶ seeding
    │  entries without checked_in_at → status 'no_show'
    │  suggested seeding runs over checked-in entrants only
seeding ──/tournament start──▶ running
    │  requires seeds 1..n contiguous; generates the bracket;
    │  creates round-1 threads; posts the bracket
running ──(final set completes)──▶ completed
any ──/tournament cancel──▶ canceled
```

`tournaments.status` is `registration | checkin | seeding | running | completed | canceled`.

**Check-in gates the bracket**: the field is whoever checked in, not whoever registered, so no-shows never
occupy a slot.

### 8.4 Commands

Discord allows only two levels of nesting, and **a command cannot be both a group and a leaf** — a player's
`/tournament checkin` cannot coexist with `/tournament checkin open`. Hence the flat admin verbs.

| Command | Who | Effect |
|---|---|---|
| `/tournament create name slug` | Manage Guild | Creates channels; registers creator as admin |
| `/tournament admin add\|remove\|list` | creator | Manage the admin list |
| `/tournament register` | anyone | Requires a bound account · also a button |
| `/tournament withdraw` | anyone | Before start only · also a button |
| `/tournament open-checkin [minutes]` | admin | Posts the check-in panel |
| `/tournament checkin` | anyone | Self check-in · also a button |
| `/tournament close-checkin` | admin | Marks no-shows, runs suggested seeding |
| `/tournament seed list\|set` | admin | Review and override seeds |
| `/tournament start` | admin | Generates the bracket, opens round 1 |
| `/tournament bracket [round]` | anyone | Reposts/refreshes the bracket |
| `/tournament cancel` | admin | Cancels the event |
| `/set draft` | admin | (Re)creates the draft and posts links |
| `/set done` | either player, or admin | Syncs the draft, imports, advances · also a button |
| `/set report` | admin | Manual override (`source='manual'`) |
| `/set schedule` | admin | Sets `scheduled_at` |

`/set *` resolves the set from the **current thread id**, so nobody types a set id. Outside a set thread they
take an explicit argument.

Follow the `subcommands(...) + subcommand_required` pattern from `bind` (`src/main.rs:44-47`) and register in the
single `commands: vec![…]` at `src/main.rs:477`. Note that file's quirk — subcommands are *also* pushed as
top-level commands — and don't replicate it here.

### 8.5 Player panels

Both panels are persistent messages in `#…-register`, edited in place.

**Why buttons rather than emoji reactions.** The repo only ever *adds* reactions (`message.react`,
`src/main.rs:356-372`); there is no `reaction_add` handler, so emoji input needs new event handling either way.
Buttons then win on every axis that matters: they can be **disabled** when a phase closes, they give
**ephemeral per-user feedback** — a reaction cannot tell one user "you aren't bound yet" without spamming the
channel — and nobody can un-press one ambiguously. Slash commands remain as fallbacks.

#### Registration panel

```
📝 **Relic Cup — registration is OPEN**
Bo3 single elimination · check-in required before start

**Registered (12)**
MarineLorD · Puppypaw · Wam01 · Anotand · …

[ 📝 Register ]  [ ❌ Withdraw ]
```

Registration must give unmistakable feedback:

1. **An ephemeral confirmation naming what was registered**, so the player can see it resolved the right
   profile — `✅ Registered as MarineLorD (ATR 2292, ELO 1180). You are entrant #12.` Silence, or a bare "ok",
   is what makes people press twice.
2. **The roster updates in the panel**, so signups are publicly visible without an announcement per player.
3. **Failures are equally explicit and ephemeral**: not bound (`run /bind id <your aoe4 id> first`),
   registration closed, already registered, or the aoe4world lookup failed.

Registering fetches the aoe4world profile anyway — it validates the binding and supplies `display_name` — which
is where the numbers in that confirmation come from. Snapshot ratings at registration and **refresh them at
seeding**, so a stale signup-time number never decides a seed.

#### Check-in panel

```
📋 **Relic Cup — check-in is OPEN**
Closes in 30 minutes.
  [ ✅ Check In ]     12/16 checked in
(or use /tournament checkin)
```

Same feedback rules: newly checked in, already checked in, not registered, check-in closed.

#### Shared panel mechanics

- **Persistent `custom_id`s, one dispatcher.** Convention `"<action>:<entity_id>"` — `register:<tid>`,
  `withdraw:<tid>`, `checkin:<tid>`, `setdone:<set_id>` — matched in a single `EventHandler::interaction_create`
  branch. **Deliberately not `ComponentInteractionCollector`**: a collector dies with the process, and these
  panels must survive a deploy. Encoding the id in the `custom_id` makes every button stateless.
- **Acknowledge within 3 seconds.** Discord fails an interaction not acked in 3s, and `register` (aoe4world
  lookup) and `setdone` (draft fetch) both make HTTP calls. Respond `CreateInteractionResponse::Defer(...)`
  ephemerally *first*, then edit the response. Getting this wrong shows "This interaction failed" even when the
  work succeeded.
- **Unknown or malformed `custom_id`s must be ignored, not panic.** Buttons from an older deploy will be
  pressed.
- **Presses are idempotent** — a second press reports the existing state.
- **Edits must be throttled.** Editing the roster or counter per press is one API call per press against a
  per-channel edit rate limit; coalesce to at most one edit every few seconds, plus a final edit when the phase
  closes.
- **Disable components on phase change** — closing a phase edits the panel to `CreateButton::disabled(true)`, so
  buttons visibly stop working instead of failing on press.
- Panel message ids live in the DB, so a **boot-time reconciliation** should confirm each still exists and
  recreate it if an organizer deleted it.

### 8.6 Bracket publication

An ASCII bracket in a code block, posted in `#…-bracket` and edited in place as results land.

```
1·MarineLorD ─┐
              ├─ MarineLorD 2-1 ─┐
8·Beasty     ─┘                  │
                                 ├─ ?
5·VortiX     ─┐                  │
              ├─ Anotand    2-0 ─┘
4·Anotand    ─┘
```

Four constraints, all easy to miss and all visible in production if missed:

1. **No markdown inside a code block.** Winners cannot be bolded — mark them in ASCII (a `>` prefix or trailing
   `*`) and put the score on the connector line.
2. **Backticks in a player name break the fence.** `ranked::escape()` (`src/ranked.rs:150-160`) is the wrong
   tool here: it escapes markdown *outside* code blocks. Inside a fence the only hazards are backticks and the
   fence sequence, so strip or replace them.
3. **CJK names break monospace alignment.** This guild is Traditional Chinese speaking, and CJK characters are
   double-width, so `str::chars().count()` returns the wrong column width and every row after a CJK name
   misaligns. Column math must use East Asian display width — add `unicode-width` (tiny, no transitive deps)
   rather than counting chars.
4. **The 2000-char message limit.** At a 12-column name width: 8 players ≈ 600 chars, 16 ≈ 1700 (one message,
   near the edge), 32 ≈ 4500 — **must split**. Render as top half / bottom half plus a final message for the
   closing rounds and champion, storing each message id in `tournament_bracket_messages` so all chunks are
   edited in place.

Names truncate to a fixed display width (default 12) with a single-cell ellipsis, seeds prefixed as
`1·MarineLorD`.

Mobile is this format's known weakness — a 16-player bracket is already wider than a phone's code block. So
`/tournament bracket round:<n>` should also offer a plain per-round list as a companion view:

```
**Quarterfinals**
`1` MarineLorD  2 – 1  Beasty  `8`
`5` VortiX      0 – 2  Anotand `4`
```

The renderer is a **pure function** — `fn render(sets: &[Set], width: usize) -> Vec<String>` — so it is testable
with golden strings and no Discord involved.

### 8.7 Set threads

When a set reaches `ready`:

1. Create a private thread on the matches channel named `R1M1 · MarineLorD vs Beasty`, truncated to Discord's 100-char
   limit (budget ~30 display-width chars per name, using the same width helper as the bracket).
2. Add both players and every current admin (`Http::add_thread_channel_member`).
3. Create the draft (§3.2 item 3) with each player's `externalRef` set to their `aoe4_id`; store
   `draft_external_id`, `draft_url`, `thread_id`.
4. Post a pinned control panel, and DM **each player their own join link** — falling back to an ephemeral reply
   if their DMs are closed.

> Step 4 matters: the two join URLs are **seats, not spectator links**. Posting both in a shared thread lets
> either player open the other's seat. The thread panel carries the watch link only.

DM sending already has a pattern at `src/main.rs:308-317`.

```
⚔️ **Round 1 · Match 1 — Bo3**
1·MarineLorD  vs  8·Beasty
Your join link has been sent to you by DM.

[ 🔗 Watch draft ]  [ ✅ Set complete ]
```

- `🔗 Watch draft` is a **link button** (`CreateButton::new_link(url)`) — no `custom_id`, no interaction, and it
  renders as a real button rather than a bare URL.
- `✅ Set complete` carries `custom_id = "setdone:<set_id>"` and runs exactly what `/set done` runs: one code
  path, two entry points. It must `Defer` first.
- **Safe to press early.** It triggers a *sync*, and the draft tool is authoritative — an unfinished draft
  reports "still in progress, nothing imported" and changes nothing. Hence no confirmation dialog and no
  winner-only restriction; either player or an admin may press.
- After a successful import the panel is edited to disable the button and show the final score, so the thread
  reads as closed before being archived and locked.

### 8.8 Schema additions

```sql
create table if not exists tournament_admins (
  tournament_id integer not null references tournaments(id) on delete cascade,
  user_id bigint not null,                  -- discord user_id
  added_by bigint not null,
  added_at timestamp not null default (datetime('now')),
  primary key (tournament_id, user_id)
);

create table if not exists tournament_bracket_messages (
  tournament_id integer not null references tournaments(id) on delete cascade,
  ordinal integer not null,                 -- which chunk of a split bracket
  message_id bigint not null,
  primary key (tournament_id, ordinal)
);
```

Columns added to §4's tables:

- `tournaments`: `category_id`, `register_channel_id`, `register_message_id`, `bracket_channel_id`,
  `matches_channel_id`, `checkin_message_id`, `checkin_closes_at`. Both panels live in the register channel, so
  one channel id covers them.
- `tournament_entries`: `checked_in_at timestamp`.
- `tournament_sets`: `thread_id bigint`.

### 8.9 New infrastructure this introduces

All first-of-its-kind in this codebase, so the cost is visible up front:

1. Permission gating — `required_permissions`, `guild_only`, `ephemeral`, and an `on_error` handler.
2. Message components and `interaction_create` handling, with one dispatcher over `"<action>:<id>"` custom_ids
   and deferred ephemeral responses wherever a handler makes an HTTP call.
3. Channel and thread creation, permission overwrites, thread membership.
4. Rate-limit-aware message editing (registration roster, check-in counter, bracket chunks).
5. `unicode-width` as a dependency, for CJK-safe column alignment.
6. Boot-time reconciliation of long-lived panel messages.

## 9. Delivery notes

**Schema delivery needs work before any of the above lands.** Today `schema.sql` is `include_str!`'d and
executed as one batch at `src/main.rs:470`, and it is entirely `create table if not exists`. Two problems:

1. **`schema.sql` line 12 has no trailing semicolon.** Appending tables to it silently breaks the whole batch.
2. **There is no versioned migration mechanism**, so an `alter table` has nowhere to live — and a seven-table
   feature will need alters.

Add a `migrations/` directory driven by `sqlx::migrate!`. This needs no dependency change: sqlx's default
features are not disabled in `Cargo.toml`, so `migrate` is already available. Run the migrator *after* the
existing `schema.sql` execute so the live database on the Fly volume (`bot_data` → `/data/bot.db`, single
machine) is unaffected; `sqlx::migrate!` maintains its own `_sqlx_migrations` table.

Note `src/integration_tests.rs:34` reads `schema.sql` by relative path. Tests will need the same two-step
setup, so add a shared `test_pool()` helper there rather than repeating the preamble in every test.

## 10. Test plan

No network in tests. The existing aoe4world tests at `src/ranked.rs:256-303` already make live calls and fail
offline; do not add more. Draft-API and esports-leaderboard deserialization are tested against saved sample
payloads.

**Bracket math (no database):** pairings for n = 2, 3, 4, 6, 8, 16; byes placed on top seeds; no two top-4 seeds
meeting before the semi-finals; advancement links forming a single-rooted tree; `best_of` varying per round
within one bracket.

**Bracket rendering (no database, golden strings):** n = 4, 8, 16, 32 with ASCII names; **CJK names keep their
columns aligned**; over-long names truncate to the configured display width; n = 32 splits into multiple chunks,
each under 2000 characters; a name containing backticks or a fence sequence cannot escape the code block; the
per-round list view renders for any round.

Assert the **box-drawing joins share a column** — that each `┐`, `├`, `│` and `┘` belonging to one connector is
at the same index — rather than only comparing against a golden string. An off-by-one here is invisible when
reading the code and obvious to everyone looking at the output; the example in §8.6 shipped wrong at first for
exactly this reason.

**Discord helpers (no network):** thread names stay within 100 characters for worst-case player names; every
`custom_id` round-trips to the right action and entity id, and an unknown or malformed one is ignored rather
than panicking (buttons from an older deploy will be pressed).

**Database, on `sqlite::memory:` following `src/integration_tests.rs`:**

- the migrator runs clean on an empty database and on one that already has `accounts`/`reminders`;
- `pragma foreign_keys` is on, and an entry referencing an unbound `aoe4_id` is rejected;
- seeding tiers correctly when only some entrants have an `atr`, and an organizer's `seed` override survives
  bracket generation;
- draft import maps slots correctly with `draft_slots_swapped` both 0 and 1;
- re-import overwrites `draft_import` rows and preserves `manual` ones;
- a set reaching `ceil(best_of/2)` wins completes, eliminates the loser, and places the winner in the correct
  slot of the next set;
- a `result.scoreBySlot` disagreeing with the imported games is flagged rather than silently resolved;
- **lifecycle transitions**: every illegal move is rejected — starting before check-in closes, checking in on a
  `running` tournament, registering after start, starting with non-contiguous seeds;
- **check-in**: a second check-in is idempotent; an unregistered user is rejected; closing marks exactly the
  non-checked-in entrants `no_show` and seeds only the rest;
- **registration**: an unbound user is rejected with the bind hint; a second registration is idempotent;
  withdrawal works only before start;
- **`setdone` on an unfinished draft** imports nothing and leaves the set untouched.

## 11. Follow-ups

Tracked separately; not part of this design.

- **aoe4world API compliance.** Their terms require a User-Agent identifying the application and carrying
  contact information, and forbid browser or spoofed agents. The bot currently sends **no User-Agent at all**
  from three bare `reqwest::get()` calls (`src/main.rs:130-135`, `src/ranked.rs:181-185`, `:221-225`). The fix
  is one shared `reqwest::Client` reached through a process-wide accessor in `src/aoe4world.rs`, rather than a
  field on `Data` — `reqwest::Client` is internally pooled and cheap to clone, and a singleton avoids threading
  it through `try_create_ranked_without_account`, the cron closure that builds its own `Data`
  (`src/main.rs:512`), and the existing tests.
- **`Secrets.toml`.** A leftover from the removed Shuttle setup, containing a plaintext `DISCORD_TOKEN` that no
  code reads (deployment uses GitHub Actions secrets and Fly). It is **untracked and gitignored — it was never
  committed**, so there is no repository exposure and nothing to purge from history; the only issue is a stale
  credential sitting on disk. Delete the file.
- **Result cross-checking.** `GET /api/v0/players/:profile_id/games?opponent_profile_id=X` returns games
  between two players with map, civs and winner, and supports `since=`/`updated_since=` for cheap incremental
  polling. This could verify the draft tool's results independently. Once migrations exist, adding
  `aoe4world_game_id` is a one-line `alter table`.
- **Autocomplete.** Registration autocomplete (`src/main.rs:111-135`) calls `players/search` on every
  keystroke with no caching; `GET /api/v0/players/autocomplete` is purpose-built for it.

## 12. Open questions

- **Who takes the §3.2 API requirements to the tool's author?** Items 1 and 2 are the hard dependency for
  shipping.
- **Who may file a manual result override** (`/set report`) — both players, the winner only, or organizers — and
  does it need opponent confirmation? Distinct from `/set done`, which is open to either player because the
  draft tool is authoritative; a manual override bypasses that authority, so it may warrant a tighter rule.
- **Should the bot enforce an event-level map pool?** Currently no: the draft preset owns the pool. Adding a
  `tournament_maps` table would give organizers a pool the bot checks, at the cost of a second source of truth.
- **Civ/map key mapping** between the draft tool's kebab-case and aoe4world's snake_case needs to be built and
  verified rather than assumed (§3.1).
- **Registration roster contents** (§8.5) — names only, or names with ATR/ELO? Ratings make the field's strength
  visible during signup but turn registration into a public leaderboard, which some players dislike. The
  ephemeral confirmation shows a registrant their own numbers either way.
- **`MANAGE_GUILD` bypassing the admin list** (§8.2) — proposed for recoverability when a creator leaves the
  server, at the cost of letting any server admin act on any tournament.
- **Read-only bracket channel** (§8.1) — proposed, though some organizers like a chat-along bracket channel.
- **Check-in reminders** — should the bot DM registered players when check-in opens, or shortly before it
  closes? Cheap to add on the existing cron; not requested.
- **Scheduling** — `/set schedule` stores `scheduled_at`, but nothing acts on it: no reminders, no timezone
  handling.
