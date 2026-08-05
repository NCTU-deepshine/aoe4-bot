# Tournament orchestration — design

Status: **design only.** Nothing in this document is implemented yet. It exists so that implementation is
mechanical.

[§3](#3-the-external-draft-tool) is written against the draft tool's **source**, which is public and MIT:
`https://github.com/MaxLiu1016/aoe4_banpick`, cloned at `~/workspace/aoe4_banpick`. Claims there name the file
they come from, as read at commit `2bc2962` (2026-08-04); they are deliberately not pinned to line numbers,
which rot. Two caveats: the deployed instance may lag the repo, so re-verify before implementing; and the repo's
own `PLAN.md` is stale (it specifies Postgres, the app runs MongoDB) — read the code, not the plan.

## 1. Scope

The bot today maps Discord users to aoe4world profiles (`accounts`) and publishes a ranked board. That is the
whole of it — `schema.sql` has one table. This design adds **tournament management**: create an event, register entrants, seed them from
ratings, generate a bracket, link each match to a draft on the external ban/pick tool, and record results.

**In scope now**

- Single elimination, fully pre-generated bracket with advancement links.
- Per-round rules (a Bo3 bracket with a Bo5 final is configuration, not code).
- Entrants seeded from ATR and ELO, with organizer override.
- Results imported from the draft tool, with a manual override path.
- Discord orchestration (§8): channel and thread layout, admin roles, a check-in gate, button-driven
  registration and check-in, bracket publication, a private thread per set, and a public draft channel.

**Designed for, not built now**

- Swiss, group stage, round robin, double elimination.

**Out of scope**

- Team (2v2+) tournaments.

### Decisions

Positions we took, each of which could have gone the other way.

What the draft tool imposes is **not** listed here, because it was never ours to decide: the offer-and-snipe
mechanic, how a civ ban is scoped, the civ and map vocabularies, that a seat cannot be reserved in advance, and
that every entrant needs an account on the tool. Those are in §3, where they are described as constraints to work
within. Mixing them into this table invites someone to revisit a position nobody ever held.

| Topic | Decision |
|---|---|
| Bracket | Single elimination, fully pre-generated with advancement links |
| Round rules | Per-round, not per-tournament |
| Draft tool | External; the bot stores a pointer and queries draft content on the fly |
| Planning assumption | **The read API in §3.2 item 1 will exist by ship time — it is small enough to write ourselves** |
| Draft creation | The bot creates each draft itself, from an account of its own (§3.3) |
| Seats | Higher seed takes Player 1, by instruction — seats cannot be reserved, so compliance is assumed (§8.7) |
| Redraft | `/set redraft` in the set thread, either player or an admin — the remedy for a mis-seated draft |
| Draft channel | A public channel carrying the spectator link once both seats are claimed (§8.1) |
| Map order | The loser of game N picks from **the whole pool** — a requirement on the round's preset, not something we enforce (§3.3) |
| Ratings | ATR (external tournament Elo) and ELO (`rm_1v1_elo`) |
| Seeding | Bot suggests; organizer finalizes and may override any seed |
| Player identity | The tournament side keeps its own player list — one main aoe4 profile per Discord user, bound at sign-up (§4) |
| Registration | Own channel with a button panel and explicit feedback; first sign-up also binds (§8.5) |
| Per-game record | Map, both civs, winner |
| Check-in | Required, and must close before start — the bracket is built from checked-in players only (§8.3) |
| Guild scope | Tournament features live in their own guild, the existing features stay in theirs; ids hardcoded for now (§8.0) |
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
| Its own user accounts, and which one holds each seat | Which entrant was *told* to take each seat |

Consequences, each of which removes a table or column someone would otherwise reach for:

- No civ table — the list is fixed and already hardcoded in `src/aoe4world.rs`.
- No event-level map pool — the draft preset defines the pool per draft.
- No `map_bans_per_player`, no `allow_civ_repeat` — preset settings.
- No draft-action table — steps are fetched on demand, never mirrored.
- No map-order validation — `MAP_SELECT` is the tool's business. The one thing we check is the *preset*, once,
  when a round is configured (§3.3): no `MAP_PICK` steps, so the loser picks from the whole pool.
- No attempt to derive who is in which seat. The tool knows only its own user ids, and exposes only *whether*
  both seats are filled — so the bot instructs players where to sit (§8.7) instead of reconciling afterwards,
  and a mistake is fixed by redrafting rather than by a mapping table.

**Rules versus ruleset.** That table divides *state and enforcement*, not decisions. Every rule inside a draft is
the tool's to run and the only thing that enforces it: map order, civ ban scope, whether a civ may be replayed,
when a set is decided. What the bot owns is **which ruleset a round uses** — one preset id on the round (§4) and
the checks in §3.3. So "the loser picks from the whole pool" is our rule and the tool's mechanism: asserted once
by refusing a preset that cannot express it, and never again per pick.

Worth naming because an earlier draft of this document called that rule "enforced by the tool", which is the
category error to avoid. The tool enforces whatever the preset says; the preset is ours to choose. The same holds
for `resultMode: "vote"` and for match length, which is why both are preset checks rather than columns.

`tournament_games` is the one place the bot keeps a copy of the tool's output. It is a **projection**, not a
second source of truth: populated by import, refreshable, and used so standings and civ/map statistics are
SQL-queryable without a network call.

## 3. The external draft tool

`https://aoe4banpick-production.up.railway.app` — "Tournament drafting for Age of Empires IV". Drafts are
identified by a 24-hex id and exposed at two URLs: `/match/<id>` (the room, where seats are claimed) and
`/watch/<id>` (spectator). The distinction matters throughout: the room link is a way in, the watch link is not.

### 3.1 Verified against source

An earlier draft of this section was written by reading the client bundles and probing the live service; several
of its conclusions were wrong, and are corrected here against the source.

**Step types** — the enum at `lib/draft/schema.ts`, verbatim:

```
MAP_BAN   MAP_PICK   MAP_SELECT
CIV_BAN   CIV_OFFER  CIV_PICK   CIV_SNIPE_OPPONENT
SYNC_CONFIRM   GAME_RESULT
```

`SYNC_CONFIRM` is a **step type**, not a status — a gate where both players must press confirm before the draft
proceeds. Entry states are `available | banned | picked | drafted` (`lib/draft/engine.ts`).

**Statuses are `lobby | running | paused | finished`** (`lib/models/Match.ts`) — lowercase, and there is
no `completed` or `abandoned`.

> **Nothing ever writes `finished`.** `deriveState()` computes it per request, but the only values ever written
> to the stored `status` are `running` and `paused` (`lib/socket/matchHandlers.ts`). A decided series reads
> `running` in the database forever. **Completion must be derived from the score, never from a status field.** The
> tool works the threshold out itself (`lib/draft/engine.ts`); item 1 reports the answer as `finished`.

`GAME_RESULT` and `MAP_SELECT` being step types is the important detail: **the tool drives the whole set**, not
just a pre-match draft. Per-game results and the loser's next-map pick happen inside it.

**Presets are user-defined** and carry both the flow and the match length. No fixed phase order may be
hardcoded anywhere in the bot. The default preset (`lib/draft/defaultPreset.ts`) is a useful reference for what
a real one looks like: map bans, each player picking maps into their **own** pool, opponent-scoped civ bans, a
drafted "hand" of `bestOf + 1` civs, then per game a map selection, a simultaneous hidden offer of 2, a
simultaneous counter-snipe of 1, and a result.

Two rules the earlier draft got wrong:

- **Snipe.** `CIV_SNIPE_OPPONENT` removes civs from the opponent's *hidden offer for that one game*; the civ each
  player fields is what survives their own offer (`lib/draft/engine.ts`). It does not remove a civ the opponent
  previously picked.
- **Map order.** "Loser picks the next map" is a preset choice (`actor: "LOSER"`), not a tool-wide rule, and
  *what* they may pick from has three cases (`lib/draft/engine.ts`):
  - `mapScope: "own"`, the default — only maps **that player** picked, filtered on `by`. Not the opponent's.
  - `mapScope: "shared"` — every map in state `picked`, with no `by` filter, so the *union* of both players'
    picks. The schema comment calls this "the maps BOTH players picked", which reads like an intersection; the
    code is a union, and the loser may take a map the opponent picked.
  - **no `MAP_PICK` steps at all** — nothing is ever `picked`, so `mapsByP1.length ? mapsByP1 : neutralMaps`
    falls through to every un-banned map. This is the case we want (§3.3).

  Already-played maps are then filtered out, but not unconditionally: `notPlayedMaps.length > 0 ? notPlayedMaps :
  mapSelectBase` permits a repeat rather than deadlocking on an exhausted pool. Unreachable with a 30-map pool;
  quite reachable in `own` mode, where a player might hold three maps in a Bo5. Game 1 is a separate step, drawn
  at random by the server when its actor is `HOST_DRAW`.

**Civ bans are scoped.** `banScope: "pool" | "opponent"` — a pool ban removes the civ globally, while an
opponent ban blocks only the banner's opponent and leaves the civ's global state untouched
(`lib/draft/engine.ts`). The default preset uses `opponent`.

**Timers auto-fill.** Steps carry `timeLimitSec`; when one expires the server picks a random legal target for
whoever owed input (`lib/socket/matchHandlers.ts`). A draft can therefore advance — or finish — with no
human acting, which is worth remembering when reasoning about a stalled set.

**Identifiers are kebab-case.** 23 civs exist (`data/civs.ts`), but the **default competitive pool is the 12
base civs** — `BASE_CIVS` excludes every variant (`data/civs.ts`), and `buildDefaultConfig` uses it. The
tool does record `variantOf` (`ayyubids` of `abbasid-dynasty`, `zhu-xis-legacy` of `chinese`, and so on);
**we ignore that relationship** — a variant is just another civ id. Do not model, store, or group by it.

The default map pool is **30 maps** (`data/maps.ts`). Neither list is a fixed vocabulary for us: what
matters per set is the **round's preset**, whose `config.civs` and `config.maps` are authoritative.

> **Key-format mismatch.** The tool uses kebab-case; aoe4world and our civ table at `src/aoe4world.rs`
> use snake_case (`abbasid-dynasty` vs `abbasid_dynasty`). Replacing `-` with `_` covered every case checked,
> but do not rely on it — build an explicit mapping table and test it against both vocabularies.

**How draft content actually moves.** Not Ably: the tool runs its **own Socket.IO server** on a custom Node
server (`server.ts`), one room per match named `match:<id>` (`lib/socket/events.ts`). Clients receive
full authoritative state snapshots as `match:state`, redacted per recipient so in-flight simultaneous picks and
`anonymous` seat names stay hidden (`lib/socket/matchHandlers.ts`). Identity comes from an HMAC ticket signed with
`AUTH_SECRET` and minted by `/api/socket-token` (401 unauthenticated) — never from the socket payload
(`lib/socket/ticket.ts`, `lib/socket/matchHandlers.ts`).

**HTTP API surface.** `GET /api/matches/<id>` returns `200` for both `/match/` and `/watch/` ids (one id space)
and carries lobby metadata only (`app/api/matches/[id]/route.ts`):

```json
{"id":"…","status":"running","shareCode":"ec8801f3","hostId":"…",
 "hasPlayer1":true,"hasPlayer2":true,"isHost":false,"viewer":null}
```

Every content sub-resource returns 404 (`/state`, `/steps`, `/actions`, `/events`, `/draft`, `/snapshot`,
`/summary`, `/result`, `/players`), and both pages hydrate client-side, so there is no server-rendered state to
scrape either.

**Do not build against `/api/matches/<id>` for draft content.** It is undocumented, unversioned, carries none,
and its `status` cannot show completion (see above). It has **one sanctioned use**: `hasPlayer1` / `hasPlayer2`
are the only unauthenticated signal that both seats are claimed, which §8.7 needs to announce a draft. Item 1
below should absorb that signal so even this use goes away.

**Preset config is already readable, unauthenticated.** `GET /api/presets/:id` returns the full `config` —
civs, maps, steps, `options.bestOf` — for any public preset, and `GET /api/presets?scope=public` lists them
(`app/api/presets/[id]/route.ts`, `app/api/presets/route.ts`). So the bot can know a round's pool and
flow before a single draft is created, without the catalog endpoint in item 2.

### 3.2 API asks

**Item 1 is the only hard dependency for shipping.** Draft *creation* is not on this list any more — it works
today with the tool's existing endpoint and an account of our own (§3.3). Items 2 to 4 are improvements. Paths
are proposals.

#### Item 1 — spectator draft read (required)

`GET /api/v1/drafts/:id` — unauthenticated (the watch link is already public), read-only, supporting
`ETag`/`If-None-Match` so the bot can poll cheaply.

This is a thin wrapper over what the tool already computes: `deriveState()` plus the `Match` document is
essentially this payload (`lib/draft/engine.ts`, `lib/socket/matchHandlers.ts` builds almost exactly
it for the socket). **It is small enough that we can write it ourselves** rather than wait — see §12 on whether
and when to offer it upstream.

Every field below has a named consumer in this document. Nothing is included on the grounds that it might be
useful later.

```jsonc
{
  "id": "6a70814a15132eb72a04e531",
  "status": "lobby|running|paused|finished",
  "finished": false,                  // derived, never a stored status — see §3.1
  "updatedAt": "2026-08-03T10:15:00Z",
  "seats": [ { "slot": 1, "claimed": true }, { "slot": 2, "claimed": false } ],
  "games": [
    { "number": 1, "map": "prairie",
      "civBySlot": { "1": "english", "2": "rus" }, "winnerSlot": 1 }
  ],
  "score": { "1": 1, "2": 0 }
}
```

- **`games`** is `tournament_games` (§4), field for field. This is the payload's reason to exist.
- **`score`** is the tool's own count, which is what makes §7's cross-check meaningful — comparing our tally
  against theirs. Deriving it from `games` ourselves would just compare our tally against itself.
- **`finished` must be explicit**, because the stored `status` never says so (§3.1).
- **`status`** is what lets `/set done` answer "still in the lobby" or "paused" instead of a blank "not yet".
- **`seats[].claimed`** is the gate for §8.7's announcement, and removes the one sanctioned use of
  `/api/matches/<id>`.
- **`updatedAt` and `ETag`** make polling cheap rather than wasteful.

**Deliberately not asked for.** Each of these was in an earlier draft; each was cut because nothing consumes it:

- **`steps[]`, the full action log** — §2 already commits to never mirroring draft actions, and rendering a
  draft's audit trail is not in scope (§1). This was the largest part of the payload and the least used.
- **Per-game offer and snipe lists** — only meaningful for that audit trail. (For the record, the duel is a set
  of hidden offers countered by a set of snipes, resolved to a survivor. An earlier draft asked for a
  `targetsOrdinal` pointer from a snipe to a pick, which misdescribes the mechanic; do not reintroduce it.)
- **`preset` metadata** (`name`, `bestOf`, `target`, `resultMode`, `anonymous`) — the bot picked that preset and
  validated it when the round was configured (§3.3), so it already knows all of it. A disagreement belongs at
  configuration time, not on every poll.
- **`mapPool`** — preset data, and each game already carries the map that was played.
- **`shareCode`** — nothing uses it.
- **`seats[].name`** — the bot displays *our* entrant names. Tool names are player-editable and hidden under
  `anonymous`, so they are worse than what we already hold.

**The one field worth adding next, and only next: `seats[].accountId`.** It costs the tool nothing — the socket
payload already sends each seat's account id to every spectator, and `anonymous` strips only the name, never the
id (`lib/socket/matchHandlers.ts`) — and it is the only thing that would make seat mapping *discoverable* rather
than asserted. With a one-time binding of each entrant to their tool account, a claimed seat resolves to an
entrant and §8.7's seat instruction becomes a fallback. It is excluded above because nothing in the design
consumes it today, and the binding step it needs is exactly what §3.6 avoids.

A caller-supplied `externalRef` is *not* the answer to that problem and is not asked for: the bot can only attach
a ref to a seat it may reserve, and if it can reserve seats it already knows the mapping. The ref would confirm
what we asserted; `accountId` reports what happened.

#### Item 2 — catalog read (desirable)

`GET /api/v1/catalog` — the civ and map vocabulary as `{id, name}` pairs; any `variantOf` is ignored (§3.1).

Not required, because a public preset's `config` already carries both, unauthenticated (§3.1), which covers every
round we run. A real catalog would still be better — one stable global list rather than a per-preset subset, and
it stays authoritative when a DLC civ or a map rotation lands. Cache with a long TTL.

#### Item 3 — seat assignment (desirable)

Creation needs nothing new. What is missing is the ability to say **who** may take each seat: seats are claimed
first-come by whichever logged-in account clicks an empty one, and the invite URL is the same `/match/<id>` for
both players (`components/match/MatchRoom.tsx`). There is no roster and no per-seat token.

The ask is a **per-seat join token** — a link that *is* a seat — plus a roster at creation naming who each seat is
for. That would let §8.7 hand each player their own link instead of one shared link plus an instruction, and
retire `/set redraft` as the remedy for sitting in the wrong chair. An `externalMatchRef` echoed back would also
let a draft be reconciled to a set if our pointer is ever lost.

#### Item 4 — completion webhook (optional)

`POST` item 1's body to a configured URL on status change. The bot has no HTTP listener — `fly.toml` declares an
`[http_service]` on port 8080 but nothing binds it — so this means standing up a server and a public route.
Polling item 1 from the existing `tokio-cron-scheduler` job (`src/main.rs`) plus an on-demand refresh is the
cheaper first cut, and probably the permanent one.

### 3.3 The bot's own account on the tool

The bot creates every draft itself, from a normal account it owns. Registration is a plain
`POST /api/register` with a username and password — open today, because `INVITE_ONLY_REGISTRATION` is `false`
(`lib/features.ts`), but that is a flag someone can flip.

**Authentication is the awkward part.** `POST /api/matches` requires a session cookie
(`app/api/matches/route.ts`), and the only provider is Auth.js Credentials with a JWT session
(`auth.ts`). So the bot drives a browser flow headlessly: `GET /api/auth/csrf`, then
`POST /api/auth/callback/credentials` carrying the CSRF double-submit cookie, keeping the resulting
`authjs.session-token` in a `reqwest` cookie store and re-authenticating on the first 401. This works, but it is
a browser handshake performed by a non-browser client and can break on an Auth.js upgrade — which is the real
argument for an API-key-authenticated create endpoint later. It is not a blocker.

**The bot needs no presets of its own.** That endpoint accepts any preset that is public or owned by the caller,
so a round points at an organizer's **public** preset by id. That matters because the tool caps a user at two
presets: the cap constrains organizers — a Bo3 bracket with a Bo5 final needs two and is exactly at it — not us.

**Creating a draft makes the bot its host.** Consequences, all from the socket layer
(`lib/socket/matchHandlers.ts`):

- **Rounds must use `resultMode: "vote"`.** In `"host"` mode only the host may call a result, which would make
  every game wait on the bot. Validate this when a round is configured.
- **No bot action is needed to start a draft.** Once both seats are claimed and both players ready, the tool
  flips it to `running` by itself.
- **Host powers are latent, not used.** The host may override a game result and force-start. Both are out of
  scope here; `/set report` (§7) stays a bot-side manual override rather than a call into the tool.

**Validate the round's preset when it is configured, not when a set opens.** `validatePreset`
(`lib/draft/validate.ts`) is a hard block at creation and returns `400` with an `issues` list. Discovering that a
preset cannot produce a Bo5 at the moment two players are waiting is the wrong time.

Two of our own rules are properties of the preset rather than of the bot, and both are checkable from the preset's
config, which is readable unauthenticated (§3.1):

- **`resultMode` must be `"vote"`**, or every game waits on the bot to call it.
- **`options.bestOf` must be odd, and must match the round's `best_of`.** Odd keeps "more than half" unambiguous
  (§7); matching keeps the tool and the bracket from disagreeing about how long a set is.
- **No `MAP_PICK` steps**, which is what makes the loser's pick range over the whole pool. With map picks present
  the tool restricts each player to the maps *they* picked, and no `mapScope` value widens that back out — the
  full pool is reached only by the empty-pool fallback. Map bans are unaffected and still work.

None of these is enforced at draft time: §2 leaves map and civ legality to the tool. These are checks on the preset an
organizer chose, made once, so a mis-built preset is caught before a set opens rather than after.

### 3.4 Players need accounts too

A seat can only be claimed by a logged-in account, so **every entrant must register on the draft tool** before
their first set. That is an event-running task, not a bot feature: check-in is the natural place to remind
people (§8.3).

Two properties of tool identity to keep in mind, both enforced in `lib/socket/matchHandlers.ts`:

- **Display names are player-editable**, to 32 characters, at any time. Never match entrants by name.
- **`anonymous` presets hide seat names** from anyone who is neither a player nor the host. A public
  announcement that names both players defeats it — see §8.7.

### 3.5 Fallback read path: the socket

If item 1 does not exist, the draft is still readable **today**: an unauthenticated Socket.IO client that emits
`match:join` with only a `matchId` gets `role = "spectator"` and receives the same full state snapshot everyone
else does (`lib/socket/matchHandlers.ts`); `scripts/test-socket.ts` in that repo demonstrates exactly this. Only
in-flight simultaneous picks and `anonymous` names are redacted.

The cost is a `rust-socketio` dependency and a persistent connection inside a bot whose other network work is
cron-shaped request/response. Recorded as a fallback so nobody thinks we are blocked; item 1 remains the plan.

### 3.6 Discord as a login provider — the better answer to identity

Every awkward part of §3.4 and §8.7 comes from the same root: the tool's accounts and ours are unrelated, so
the bot cannot tell which entrant is in which seat. Discord login would dissolve that rather than work around
it — the tool would hold each player's Discord id, which is already the key of `tournament_players` (§4).

It looks cheap from the source. The tool is Auth.js v5 with a JWT session and Credentials as its only provider
(`auth.ts`), yet it already depends on `@auth/mongodb-adapter` with `lib/mongodb.ts`
staged to feed it and never wires it in. Adding `next-auth/providers/discord` means: keep the JWT strategy; skip
the adapter, which would stand up a second user store beside the Mongoose `User` that `Match.player1Id`
references; and upsert that `User` in a `signIn`/`jwt` callback keyed on a new `discordId`. `passwordHash` would
become optional (`lib/models/User.ts`) and `username` needs collision suffixing. Nothing in the engine, socket
or seat layers changes, because identity already flows through as an opaque `token.uid`.

For us it would be worth more than item 3: exact slot→entrant mapping with no seat instruction, no redraft
remedy, and no separate registration step for players. Limits, stated plainly: it is not a read path, so item 1
still gates shipping; linking Discord to an existing password account has no reliable auto-match, since email is
optional and unused for login; and it is a change in someone else's repository. Recorded as the preferred
direction — no upstream action is committed here (§12).

### 3.7 Manual fallback

Even with everything above, the bot needs an organizer override for sets played outside the tool, drafts
abandoned mid-way, and API outages: a report command writing the same `tournament_games` rows with
`source = 'manual'`. This is a fallback, not the primary path — see [§7](#7-result-flow).

## 4. Data model

Seven tables. Conventions follow the existing schema: lowercase SQL, `integer primary key
autoincrement`, Discord snowflakes and aoe4 ids as `bigint`, timestamps written with `datetime('now')`.

Note what is *not* here: no column holds an entrant's **draft-tool** identity — distinct from `tournament_players`,
which holds their **aoe4world** profile. There is nothing worth storing yet —
tool display names are player-editable, and the read API exposes no seat identity today (§3.4) — so the design
instructs players where to sit and corrects mistakes by redrafting, rather than keeping a mapping it cannot
trust. If item 1 ever returns `seats[].accountId`, one nullable column on `tournament_entries` is what turns that
into an exact mapping; it is deliberately not added on spec.

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
  draft_channel_id bigint,                  -- public; spectator links, see §8.1
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
  best_of integer not null check (best_of % 2 = 1),   -- odd only; see §7 on completion
  bracket text                              -- double elimination, later
    check (bracket in ('winners','losers','grand_final')),
  draft_preset_id text,                     -- the tool's preset ObjectId; must be a PUBLIC preset (§3.3)
  rules text,                               -- json: other per-round overrides
  unique (stage_id, ordinal, bracket)
);

-- 4. tournament players: one MAIN aoe4 profile per discord user, and one user per
--    profile. deliberately separate from `accounts`, which is the ranked board's
--    table and allows a user several profiles — see the notes. bound at sign-up
--    (§8.5) and reused by every later tournament.
create table if not exists tournament_players (
  user_id bigint primary key,               -- discord user; one main profile each
  aoe4_id bigint not null unique,           -- and one user per profile
  display_name text not null,               -- player-editable; seeded from aoe4world at first sign-up
  bound_at timestamp not null default (datetime('now')),
  updated_at timestamp
);

-- 5. entrants. keyed by discord user: everything the bot does with an entrant is a
--    discord action (mentions, thread membership, buttons). the profile is resolved
--    through tournament_players, and snapshotted here so a later rebind cannot
--    rewrite history.
create table if not exists tournament_entries (
  tournament_id integer not null references tournaments(id) on delete cascade,
  user_id bigint not null references tournament_players(user_id),
  aoe4_id bigint not null,                  -- snapshot, not a fk; see above
  seed integer,                             -- FINAL seed; the organizer may override
  suggested_seed integer,                   -- what the bot computed, kept for audit
  display_name text not null,               -- copy of tournament_players.display_name, kept in sync; see notes
  elo integer,                              -- snapshot of rm_1v1_elo.rating
  atr real,                                 -- snapshot of esports tournament elo
  atr_source text check (atr_source in ('esports','manual')),
  status text not null default 'active'
    check (status in ('active','eliminated','withdrawn','no_show')),
  registered_at timestamp not null default (datetime('now')),
  checked_in_at timestamp,                  -- null = did not check in; see §8.3
  primary key (tournament_id, user_id),
  unique (tournament_id, seed)
);

-- 6. a set = one Bo_N meeting between two entrants.
--    loser_advances_to_* is unused by single elimination but present so
--    double elimination needs no migration.
create table if not exists tournament_sets (
  id integer primary key autoincrement,
  tournament_id integer not null references tournaments(id) on delete cascade,
  round_id integer not null references tournament_rounds(id) on delete cascade,
  position integer not null,                -- index within the round, 1-based, top to bottom
  slot1_user_id bigint references tournament_players(user_id),  -- null until fed by a previous round
  slot2_user_id bigint references tournament_players(user_id),
  slot1_wins integer not null default 0,
  slot2_wins integer not null default 0,
  winner_user_id bigint references tournament_players(user_id),
  status text not null default 'pending'
    check (status in ('pending','ready','drafting','in_progress','completed','bye','walkover')),
  draft_external_id text,                   -- the tool's 24-hex id; overwritten by a redraft (§8.7).
                                              -- the room link is derived (§3), never stored: see notes
  draft_synced_at timestamp,
  draft_announce_message_id bigint,         -- post in the draft channel; null = not announced yet (§8.7)
  redraft_count integer not null default 0, -- guards /set redraft; also a signal something went wrong
  thread_id bigint,                         -- the set's private thread; see §8.7
  winner_advances_to_set_id integer references tournament_sets(id),
  winner_advances_to_slot integer check (winner_advances_to_slot in (1,2)),
  loser_advances_to_set_id integer references tournament_sets(id),
  loser_advances_to_slot integer check (loser_advances_to_slot in (1,2)),
  scheduled_at timestamp,
  completed_at timestamp,
  unique (round_id, position)
);

-- 7. games: a projection of the draft's GAME_RESULT steps, or a manual override.
create table if not exists tournament_games (
  id integer primary key autoincrement,
  set_id integer not null references tournament_sets(id) on delete cascade,
  game_number integer not null,
  map text,                                 -- draft-tool map id, kebab-case
  slot1_civ text,                           -- draft-tool civ id, kebab-case
  slot2_civ text,
  winner_user_id bigint references tournament_players(user_id),
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

- **`tournament_players` is deliberately not `accounts`.** The ranked board's `accounts` table lets one Discord
  user bind **several** aoe4 profiles, which is right for a board that lists all of them and wrong for a bracket
  that needs exactly one. Rather than constrain or reinterpret a table another feature depends on, the tournament
  side keeps its own list with the constraint it actually needs: `user_id` as the primary key (one main profile
  per user) and `aoe4_id` unique (one user per profile, so two people cannot both claim a profile). The two
  tables are independent — no foreign key, no sync, no shared writes — which is also what keeps the two guilds'
  command sets disjoint (§8.0).
- **Binding happens at sign-up, once.** A first registration takes the profile as an argument and writes
  `tournament_players`; every later tournament finds the row already there and needs nothing (§8.5). There is no
  separate bind step to forget.
- **Entrants are keyed by `user_id`, with `aoe4_id` snapshotted alongside.** Every action the bot performs on an
  entrant is a Discord action — mention them, add them to a thread, check a button press — so the Discord id is
  the natural key, and sets and games reference it for the same reason. The profile is snapshotted onto the entry
  so that changing a main later cannot silently re-attribute finished games. A rebind is refused while the user
  has an entry in a `running` tournament.
- **`display_name` is player-editable, and deliberately not frozen like the rest of the snapshot.** Unlike
  `aoe4_id`/`elo`/`atr`, a name carries no game-result attribution, so there is nothing to protect by freezing it.
  Setting it is its own action, independent of `/tournament rebind` — changing which profile is bound and
  changing how a name displays are unrelated — and it writes through to every entry in a tournament that has not
  yet completed or been canceled, so brackets and threads always show the current name. A finished tournament
  keeps whatever name was in effect when it ended.
- **`slot1`/`slot2`, not `p1`/`p2`.** Slots exist before players do, and `winner_advances_to_slot` /
  `loser_advances_to_slot` use the same numbering.
- **`tournaments` has no `best_of`.** A Bo5 final is the last round's `best_of`. A Swiss stage with Bo1 early
  rounds and Bo3 late rounds is expressible with no schema change. This is the whole reason rounds are a table.
- **`stage.config` and `round.rules` are JSON escape hatches** so a new format's knobs don't each require a
  migration. Concepts shared across formats (`best_of`, `bracket`) stay real columns.
- **There is no column for a wrong seat.** Seats are claimed first-come and the bot cannot assign them (§3.2
  item 3), so a player sitting in the other seat is possible in principle. For now this is assumed not to
  happen — the panel's seat instruction (§8.7) is trusted rather than defended against — and `/set redraft` is
  the only remedy if it ever does. A correction bit (flip the mapping without redrafting) is one column to add
  later if this assumption stops holding.
- **There is no ratings cache table.** An earlier draft had one mirroring the esports leaderboard — name, rank,
  active rank, active flag, country, Liquipedia name — of which the design only ever read `rating`. It is gone
  entirely, not just trimmed: ATR for a whole field is **one request** (§6), the durable record is the `atr`
  snapshot on `tournament_entries`, and a cache would buy a table, a sync command and staleness reasoning for no
  benefit at this scale. If rate limits ever bite, it comes back as three columns — `aoe4_id`, `rating`,
  `fetched_at` — and nothing else.
- **There is no `draft_url` column.** The tool exposes one fixed base URL with a documented path scheme (§3):
  the room is `<draft_base_url>/match/<draft_external_id>`. Storing a second column that must be kept in step
  with `draft_external_id` on every redraft buys nothing a one-line format doesn't already give for free.
- **`draft_preset_id` holds an id, not a name.** Preset names are neither unique nor stable — any user can name
  a preset anything — and `POST /api/matches` takes an ObjectId anyway.
- **A redraft overwrites the pointer; there is no draft-history table.** The bot only ever syncs the current
  draft, so a superseded id is dead weight, and it is recoverable from the tool's own history anyway — the bot
  hosts every draft it creates, and `GET /api/matches` lists a caller's hosted matches newest-first
  (`app/api/matches/route.ts`). What the bot keeps instead is `redraft_count`, because the guard in §8.7
  needs it; the readable trail is the notice posted in the set thread, which is where a dispute gets argued.
  Superseded rooms cannot be deleted on the tool's side — there is no DELETE on `/api/matches` — so they are
  simply orphaned.
- **No `map_picked_by` column.** `MAP_SELECT` records who picked, in the draft.
- **Foreign keys.** sqlx enables `pragma foreign_keys` by default on `SqliteConnectOptions`. Assert this in a
  test — every `references` above is inert if it ever changes.

## 5. Bracket generation

Pure functions, no database access, so this is unit-testable in isolation.

1. `bracket_size(n) = n.next_power_of_two()`; round count is `log2(size)`.
2. Seed slot order by reflection: start with `[1, 2]`; to double from size `s` to `2s`, map each entry `x` to
   `[x, 2s + 1 - x]`. Size 8 gives `[1,8,4,5,2,7,3,6]`, so round 1 is `(1,8) (4,5) (2,7) (3,6)` — every seed
   meets its mirror, no two of the top four can meet before the semi-finals, and 1 and 2 not before the final.
3. Seeds beyond `n` are absent, leaving a one-player set with `status = 'bye'` that auto-advances at start.
   Reflection puts those gaps against the top seeds, which is where a bye belongs.
4. Create one `tournament_rounds` row per round, then all sets, then link advancement: `position` p in round r
   feeds `ceil(p/2)` in round r+1, into slot 1 if p is odd and slot 2 if even.
5. Round names come from the end, not the start: the last round is `Final`, then `Semifinal` and
   `Quarterfinal`, and anything earlier is `Ro{players in that round}` — `Ro16`, `Ro32`.
6. Input is the **finalized** `seed` values. Generation is gated on tournament status `seeded`, so it never
   computes an order itself and an organizer override is respected by construction. It takes a *count*, not a
   list — seeds are required to be 1..=n and contiguous by then (§8.3) — and returns seeds for the caller to map
   back to entrants.

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

The `profile_ids` filter means ATR for an entire field is **one request**, regardless of size — which is why
there is no ratings cache (§4): fetch at seeding, snapshot onto the entry, done. A full leaderboard sync is
possible (`?page=N`, `per_page: 50`, `total_count: 347`) but nothing here needs one.

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

Both pieces already exist: `fetch_profile` (`src/aoe4world.rs`) and a shared `reqwest::Client` behind a
`OnceLock` accessor that sends the required User-Agent (`src/aoe4world.rs`). Seeding and the ATR lookup go
through them; do not add a bare `reqwest::get`.

Note that both call sites in `src/ranked.rs` apply `?` to both rating modes, so a player with no ELO entry is
dropped entirely; seeding must tolerate `None`, which is why every rating column is nullable.

## 7. Result flow

**Primary path — import.** A set holds `draft_external_id` — the *current* draft, since `/set redraft` can
replace it. On a poll tick or an on-demand refresh, fetch item 1, map the draft's slots onto ours (slot 1 is the
higher seed, by instruction — §8.7, §4 notes), and upsert `games` into `tournament_games` with
`source = 'draft_import'`. Stamp `draft_synced_at`. Never sync a superseded draft.

> **Completion is derived, not reported.** The tool never stores `finished` (§3.1), so a decided series still
> reads `status = "running"`. Treat a set as complete when item 1 says `finished`, or when one side has won more
> than half the games — never on status alone. A payload with `status = "running"` and a score of 2–0 in a Bo3
> **is** a finished draft.

**Two triggers, one code path:**

1. **On demand** — `/set done`, or the `✅ Set complete` button on the set's thread panel (§8.7). Syncs, imports,
   posts the result in the thread and the bracket channel, advances the winner, then archives and locks the
   thread and creates the next set's thread if it has become `ready`.
2. **Background poll** — on the existing `tokio-cron-scheduler` job (`src/main.rs`), over sets in
   `drafting`/`in_progress` that have a `draft_external_id`, so a forgotten report never stalls the bracket.
   The cron closure builds its own `Http` and `Data` (`src/main.rs`); the same shape works here, though
   the `.unwrap()` at `src/main.rs` panics the job on failure and should be handled instead. Note the job
   runs twice a day (`0 0 0,12 * * *`) — far too coarse for draft polling, so this needs its own schedule.

Because the draft tool is authoritative, an on-demand sync of an unfinished draft is a no-op that reports
"still in progress" — which is why the button needs no confirmation step and no winner-only restriction.

**Three things about how the tool decides a result**, all of which affect what a sync sees:

- **Vote or host mode.** With `resultMode: "vote"` both players must click the same winner before it commits;
  the host may override (`lib/socket/matchHandlers.ts`). Tournament rounds use vote mode (§3.3).
- **A game can be decided while the series is held.** After each result both players must acknowledge before the
  next game's clock starts (`lib/socket/matchHandlers.ts`), so a set can legitimately sit with a committed game and
  no further progress.
- **Timers auto-fill.** An expired step is filled with a random legal choice (§3.1), so a draft may advance or
  even finish with neither player reporting anything. The background poll is what catches that.

**Fallback — manual.** A report command writes the same rows with `source = 'manual'` and
`reported_by`/`reported_at` set.

**Re-import is safe and idempotent**: it overwrites `source = 'draft_import'` rows and never touches
`source = 'manual'` ones, so an organizer correction survives every subsequent sync.

**Set completion**, in one transaction:

- A set completes when one side has won more than half the games. `best_of` is odd — an even one is rejected when
  the round is configured — so there is no rounding to argue about and no threshold of our own to keep in step
  with the tool's.
- Set `winner_user_id`, `completed_at`, `status = 'completed'`; mark the loser's entry `eliminated`.
- Write the winner into `winner_advances_to_set_id` at `winner_advances_to_slot`.
- Flip that target set from `pending` to `ready` once both its slots are filled.

**Prefer the draft's `score` over recomputing from games.** If the two disagree, flag it for an organizer rather
than silently choosing one — a mismatch means either our import or their state machine is wrong, and both are
worth knowing about.

Civ and map values must be ids the round's preset actually contains (§3.1). **Legality within the draft is the
tool's business**, so the bot does not check whether a civ was available or a map was banned.

**A redraft resets the set's draft state**, not its results: `draft_external_id` is overwritten (which moves the
derived room link along with it, §4 notes), `draft_synced_at` and `draft_announce_message_id` clear, the
seat-claim watch restarts, and that set's `source = 'draft_import'` games are voided while `manual` ones survive.
See §8.7.

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

**Bot permissions required:** View Channels, Manage Channels, Manage Threads, Create Private Threads, Send
Messages, Send Messages in Threads, Read Message History. As an invite: scopes `bot` and
`applications.commands` — without the latter the commands never appear — and `permissions=360777321488`.

Deliberately absent: **Add Reactions**, which belongs to the home guild and is guarded off here (§8.0), and
**Manage Roles**. §8.1 only ever creates channels *with* their overwrites, which Manage Channels covers; editing
the overwrites of a channel that already exists is what would need Manage Roles.

### 8.0 Two guilds, one bot

Tournaments may run in a **different guild** from the one the bot serves today, and the two feature sets must not
leak into each other: no `/tournament` or `/set` commands in the home guild, and none of the ranked-board,
`/查分` or reaction behavior in the tournament guild.

- **Slash commands are registered per guild**, from two lists rather than one. `register_in_guild` already takes a
  command slice, so this is a split of the existing single list, not a new mechanism.
- **The two lists are fully disjoint.** Nothing is shared, including `bind`: the tournament side keeps its own
  player list and binds at sign-up (§4, §8.5), so the home guild's `/bind` is not needed here and the ranked
  board's `accounts` table is not touched. That independence is the point — one guild's feature set can change
  without consulting the other's.
- **Registration is not a security boundary.** Stale registrations linger and commands can be invoked from
  unexpected contexts, so every command is `guild_only` and additionally checks it is in the guild it belongs to.
- **The message-reaction handler needs a guild guard.** It currently filters on user ids and keywords with no
  guild condition at all, so it would start reacting in the tournament guild the moment the bot joins.
- **`MANAGE_GUILD` (§8.2) is inherently per-guild**, which is what we want: tournament-guild moderators get the
  bypass there and nothing in the home guild.

**Guild ids are hardcoded, or read from env with the home guild as the fallback** — no per-guild configuration
table and no setup command. This is deliberate for now: there are two guilds, both known, and `commands.rs`
already hardcodes a channel id for `/查分`, so this follows existing practice rather than introducing a pattern.
The bot must be invited to the tournament guild with the permissions listed above.

### 8.1 Channels and threads

`/tournament create name:"Relic Cup" slug:relic-cup`, run in `#relic-cup`:

```
category: Relic Cup                 <- the invoking channel's parent_id
  #relic-cup                        <- announcement channel = the invoking channel
  #relic-cup-register               <- created; player action panels live here
  #relic-cup-bracket                <- created; read-only to @everyone
  #relic-cup-draft                  <- created; read-only to @everyone; spectator links
  #relic-cup-matches                <- created; read-only parent for set threads
       ├ 🧵 R1M1 · MarineLorD vs Beasty   <- private thread per set
       └ 🧵 SF1 · MarineLorD vs Anotand
```

- The category comes from the invoking channel's `parent_id`. If that channel is top-level, create the siblings
  uncategorized and say so in the reply rather than failing.
- Names are slug-prefixed so several tournaments can share one category.
- `#…-bracket`, `#…-draft` and `#…-matches` deny `SEND_MESSAGES` to `@everyone` — they are output surfaces.
- **`#…-draft` is the spectator surface.** Set threads are private, so nothing in them is watchable by the
  server; this channel carries one post per set, published when both seats are claimed, with the `/watch/` link
  (§8.7). Five channels per tournament is still far inside the 50-per-category limit.
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
`FrameworkOptions` (`src/main.rs`) has no `on_error` handler. All of it arrives here. Worth noting the
existing `/refresh` (`src/commands.rs`), which deletes every message in the rank channel, is currently
callable by anyone.

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
| `/tournament register [aoe4_id]` | anyone | `aoe4_id` on a first sign-up only, autocompleted · also a button |
| `/tournament rebind aoe4_id` | anyone | Change your main profile; refused during a running event |
| `/tournament withdraw` | anyone | Before start only · also a button |
| `/tournament open-checkin [minutes]` | admin | Posts the check-in panel |
| `/tournament checkin` | anyone | Self check-in · also a button |
| `/tournament close-checkin` | admin | Marks no-shows, runs suggested seeding |
| `/tournament seed list\|set` | admin | Review and override seeds |
| `/tournament start` | admin | Generates the bracket, opens round 1 |
| `/tournament bracket [round]` | anyone | Reposts/refreshes the bracket |
| `/tournament cancel` | admin | Cancels the event |
| `/set draft` | admin | Creates the draft if a set somehow has none, and reposts the links |
| `/set redraft` | either player, or admin | Abandons the current draft and creates a fresh one · also a button |
| `/set done` | either player, or admin | Syncs the draft, imports, advances · also a button |
| `/set report` | admin | Manual override (`source='manual'`) |
| `/set schedule` | admin | Sets `scheduled_at` |

`/set *` resolves the set from the **current thread id**, so nobody types a set id. Outside a set thread they
take an explicit argument.

Follow the `subcommands(...) + subcommand_required` pattern from `bind` (`src/commands.rs`) and register in
the single `commands: vec![…]` at `src/main.rs`. Note that list's quirk — `bind`'s subcommands `id` and
`name` are *also* pushed as top-level commands (`src/main.rs`) — and don't replicate it here.

### 8.5 Player panels

Both panels are persistent messages in `#…-register`, edited in place.

**Why buttons rather than emoji reactions.** The repo only ever *adds* reactions (`message.react`,
`src/emperor.rs`); there is no `reaction_add` handler, so emoji input needs new event handling either way.
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

**Signing up is also how a player binds.** There is no separate bind step (§4):

- **First time ever** — `/tournament register aoe4_id:<profile>`, with the same aoe4world autocomplete the
  existing bind command uses. This writes `tournament_players` and the entry together, in one transaction.
- **Every tournament after** — the `📝 Register` button, or `/tournament register` with no argument. The player
  list already has them.
- **The button cannot take an argument**, so a first-timer who presses it gets an ephemeral reply naming the
  command to use instead. This is deliberate: a modal would work but adds a whole interaction type, and
  autocomplete — the thing that makes picking the right profile easy — does not work inside one.
- **Changing a main profile** is `/tournament rebind`, refused while the player has an entry in a `running`
  tournament, since the profile is snapshotted onto entries and sets already reference the player.
- **Changing your display name** is a separate action from rebind — it never touches which aoe4 profile is
  bound, and unlike rebind is not blocked by a running tournament, since a name carries no result attribution
  (§4 notes). It writes through to every active entry immediately.

Registration must give unmistakable feedback:

1. **An ephemeral confirmation naming what was registered**, so the player can see it resolved the right
   profile — `✅ Registered as MarineLorD (ATR 2292, ELO 1180). You are entrant #12.` Silence, or a bare "ok",
   is what makes people press twice.
2. **The roster updates in the panel**, so signups are publicly visible without an announcement per player.
3. **Failures are equally explicit and ephemeral**: registration closed, already registered, the aoe4world lookup
   failed, a first-timer pressing the button, and — the one worth wording carefully — **that profile is already
   bound to another Discord user**. The last is either a typo or two people claiming the same profile; say which
   profile and tell them to ask an admin, rather than reporting a bare constraint violation.

Registering fetches the aoe4world profile anyway — it resolves the id to a real player and supplies
`display_name` — which is where the numbers in that confirmation come from. Snapshot ratings at registration and
**refresh them at seeding**, so a stale signup-time number never decides a seed.

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
  `withdraw:<tid>`, `checkin:<tid>`, `setdone:<set_id>`, `redraft:<set_id>` — matched in a single `EventHandler::interaction_create`
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
MarineLorD   2 ─┐
                ├─ MarineLorD     ─┐
Beasty       1 ─┘                  │
                                   ├─ ?
VortiX       0 ─┐                  │
                ├─ Anotand        ─┘
Anotand      2 ─┘
```

**Every column holds the participants of the match to its right, each with the games they won in it.** So a
score sits beside the player who earned it, in the round it was played — `MarineLorD 2` against `Beasty 1` is
that semi-final, read off two adjacent rows. The rightmost column is the winner of the final, who has no next
match and so no score. An earlier form put a combined `2-1` on the connector instead, which reads worse and
needed the score reordered to the winner's side to avoid `Anotand 0-2` appearing beside the player who won.

**A match that has not started leaves the score blank** rather than showing `0`, so "not begun" stays
distinguishable from "0-2 down".

**No seeds in the graph.** A seed prefix costs two or three of a name cell's twelve columns, which is what forces
an ordinary name into an ellipsis. Seeds stay in the per-round list view below, where there is room.

Four constraints, all easy to miss and all visible in production if missed:

1. **No markdown inside a code block.** Winners cannot be bolded. Nothing marks them explicitly: advancing to the
   next column *is* the mark, and the games each player won are right there to compare.
2. **Backticks in a player name break the fence.** `ranked::escape()` (`src/ranked.rs`) is the wrong
   tool here: it escapes markdown *outside* code blocks. Inside a fence the only hazards are backticks and the
   fence sequence, so strip or replace them.
3. **CJK names break monospace alignment.** This guild is Traditional Chinese speaking, and CJK characters are
   double-width, so `str::chars().count()` returns the wrong column width and every row after a CJK name
   misaligns. Column math must use East Asian display width — add `unicode-width` (tiny, no transitive deps)
   rather than counting chars.
4. **The 2000-char message limit.** Measured at a 12-column name width, every match decided: 4 players 294
   chars, 8 → 855, **16 → 2308**, 32 → 5897. So the split starts at 16, not 32 — an earlier estimate here put 16
   at ≈1700 and inside the limit, which the implementation disproved. Most of the bulk is structural: in a
   16-player bracket 14 of the 31 rows exist only to carry a `│` in a far column, so no amount of narrowing
   rescues it. Render as top half / bottom half plus a final message for the closing rounds and champion,
   recursing while a part still does not fit, and store each message id in `tournament_bracket_messages` so all
   chunks are edited in place.

Names truncate to a fixed display width (default 12) with a single-cell ellipsis, and a wide character is never
split in half — the cell is padded back up instead.

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
3. Create the draft from the round's public preset, as the bot's own account (§3.3); store
   `draft_external_id`, `thread_id`. The bot is the draft's host.
4. Post a pinned control panel **in the thread**, carrying the room link and the seat instruction, with both
   players @-mentioned so it pings them.

> **There is one room URL, not two seats.** `/match/<id>` is the same link for both players, and a seat is
> claimed by whoever clicks an empty one first (§3.2 item 3). So the panel must say which seat to take:
> **the higher seed takes Player 1, the lower seed takes Player 2.** The bot cannot enforce this and does not
> try to detect a violation — it assumes compliance, and `/set redraft` is the remedy when someone gets it wrong.

> **Instructions live in the thread, not in DMs.** The thread's membership already *is* the right audience —
> both players plus admins (step 2) — so it scopes the room link without any DM machinery, without a closed-DM
> fallback, and leaves a record both players can scroll back to mid-series. The cost is that admins see the link
> too and could in principle claim a seat; they are trusted, and this is far better than a public channel. Every
> public surface carries `/watch/<id>` instead, which cannot claim a seat (`app/watch/[id]/page.tsx`).

```
⚔️ **Round 1 · Match 1 — Bo3**   @MarineLorD  @Beasty
1·MarineLorD  vs  8·Beasty

Draft room: <link>
**@MarineLorD takes seat Player 1** · **@Beasty takes seat Player 2**
Seats are first-come — if you end up in the wrong one, press 🔄 Regenerate draft.

[ 🔗 Watch draft ]  [ 🔄 Regenerate draft ]  [ ✅ Set complete ]
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

#### Announcing the draft

Once **both seats are claimed**, the bot posts one message in `#…-draft` and stores its id in
`draft_announce_message_id` — which doubles as the guard that stops a poll tick posting it twice:

```
⚔️ **Round 1 · Match 1** — Bo3 · Standard Bo3 (map BP, hand draft, offer & snipe)
`1` MarineLorD  vs  Beasty `8`
                                                        [ 🔗 Watch ]
```

The same message is edited with the final score when the set completes, so the channel reads as a match log.

- **Seat-claim detection** is `hasPlayer1` / `hasPlayer2` from `GET /api/matches/<id>` — the only
  unauthenticated source for it today, and the one sanctioned use of that endpoint (§3.1). Item 1's
  `seats[].claimed` is meant to replace it.
- It tells us only *that* both seats are filled, never *who* took which. The post therefore states the mapping
  the players were instructed to use; if it is wrong, that is what `/set redraft` is for (§4 notes).
- **If the round's preset is `anonymous`, post seeds and the link without names.** Naming both players in a
  public channel defeats `options.anonymous`, which the tool enforces by stripping seat names from every
  non-participant (§3.4).

#### `/set redraft`

Either player or an admin, in the set thread — also the `🔄 Regenerate draft` button
(`custom_id = "redraft:<set_id>"`). It creates a fresh draft from the same preset, **overwrites**
`draft_external_id`, increments `redraft_count`, clears `draft_synced_at` and
`draft_announce_message_id`, re-posts the panel with the seat instruction, and leaves a visible notice in the
thread naming who triggered it. The old room is orphaned on the tool's side; it cannot be deleted (§4).

Guards, in order of how likely each is to matter:

1. **Refused once the set is `completed`.** A finished set is not redraftable; corrections go through
   `/set report`.
2. **`source = 'draft_import'` games for that set are voided; `manual` rows survive.** A redraft after a game has
   been played discards the imported record of it, which is the point — the new draft is the record now.
3. **Rate-limited by `redraft_count`.** Beyond a small threshold it becomes admin-only: each redraft leaves an
   undeletable room on someone else's server, and a button either player can press is a button that can be
   pressed in frustration.

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
  `draft_channel_id`, `matches_channel_id`, `checkin_message_id`, `checkin_closes_at`. Both panels live in the
  register channel, so one channel id covers them.
- `tournament_entries`: `checked_in_at timestamp`.
- `tournament_sets`: `thread_id bigint`, `draft_announce_message_id bigint`, `redraft_count integer`.

### 8.9 New infrastructure this introduces

All first-of-its-kind in this codebase, so the cost is visible up front:

1. Permission gating — `required_permissions`, `guild_only`, `ephemeral`, and an `on_error` handler.
2. Message components and `interaction_create` handling, with one dispatcher over `"<action>:<id>"` custom_ids
   and deferred ephemeral responses wherever a handler makes an HTTP call.
3. Channel and thread creation, permission overwrites, thread membership.
4. Rate-limit-aware message editing (registration roster, check-in counter, bracket chunks).
5. `unicode-width` as a dependency, for CJK-safe column alignment.
6. Boot-time reconciliation of long-lived panel messages.
7. An authenticated HTTP session against the draft tool: a cookie store, the Auth.js credentials handshake, and
   re-auth on 401 (§3.3).
8. Per-user localization of the bot's own reply text (§8.10) — the first locale-aware text in the tournament
   feature.

Deliberately **not** on that list: direct messages. Everything player-facing happens in a channel or a thread,
so the design needs no DM path and no closed-DM fallback.

### 8.10 Localization

Two locales, on purpose: **Traditional Chinese (`zh-TW`) and English**, the fallback for everything else —
including `zh-CN` and any locale Discord adds later. This is not "detect Chinese and guess a variant"; it is one
exact string match, with English underneath everything unmatched. This is a **different, narrower** mechanism
from the home guild's existing approach of hardcoding Chinese into individual commands (`/查分`, `bind`'s
subcommands) — those are untouched; this adds per-user detection for the tournament feature's own text only.

**Detection is per-interaction, not per-guild.** Every `CommandInteraction`/`ComponentInteraction` carries the
invoking user's own client-locale setting as `locale: String`, "the selected language of the invoking user"
(verified against the vendored serenity 0.12.5 source, `command_interaction.rs`/`component_interaction.rs`); poise
exposes the slash-command side as `Context::locale() -> Option<&str>`. There is a separate `guild_locale:
Option<String>`, "the guild's preferred locale" — deliberately not used: it is the guild's own default, not any
one member's language, and every reply this applies to (an outcome message, a panel) is already either ephemeral
to one user or a shared panel where a language pick has to be made anyway — the invoking user's own setting is
the only signal that is actually about *them*.

**Scope: the tournament feature's own dynamic reply text.** Outcome messages (`RegisterOutcome`,
`WithdrawOutcome`, `RebindOutcome`, `CheckinOutcome`, `OpenCheckinOutcome`, `CloseCheckinOutcome`), both panels'
content and button labels, and `access.rs`'s ephemeral refusals. **Not** in scope:

- Slash command names and descriptions — Discord has its own static localization for these
  (`name_localizations`/`description_localizations`), a different code path from runtime reply text, and out of
  scope here.
- The home guild's existing hardcoded Chinese (`/查分`, `bind`'s subcommands) — untouched.

**Mechanism.** A `Locale` enum (`ZhTw`, `En`) and a pure `from_discord_locale(code: &str) -> Locale` — anything
other than the exact string `"zh-TW"` maps to `En`. Every message-producing function this applies to gains a
`locale: Locale` parameter and picks its template accordingly. This is retroactive: chunks 7–10 already shipped
without it, so their outcome/panel/refusal messages are retrofitted rather than written fresh — and every chunk
from here on writing new user-facing text follows the same shape from the start rather than adding it later.

## 9. Delivery notes

**Schema delivery needs work before any of the above lands.** Today `schema.sql` is `include_str!`'d and
executed as one batch at `src/main.rs`, and apart from one `drop table if exists` it is entirely
`create table if not exists`. The problem: **there is no versioned migration mechanism**, so an `alter table`
has nowhere to live — and a six-table feature will need alters. (An earlier draft of this section also flagged a
missing trailing semicolon in `schema.sql`; that was fixed in `fb4b44e0`.)

Add a `migrations/` directory driven by `sqlx::migrate!`. This needs no dependency change: sqlx's default
features are not disabled in `Cargo.toml`, so `migrate` is already available. Run the migrator *after* the
existing `schema.sql` execute so the live database on the Fly volume (`bot_data` → `/data/bot.db`, single
machine) is unaffected; `sqlx::migrate!` maintains its own `_sqlx_migrations` table.

Note `src/integration_tests.rs` reads `schema.sql` by relative path in three separate tests. Tests will need the
same two-step setup, so add a shared `test_pool()` helper there rather than repeating the preamble a fourth time.

## 10. Test plan

No network in tests. `src/ranked.rs` does have tests that call aoe4world, but they carry
`#[ignore = "hits the live aoe4world API"]`, so a default `cargo test` never runs them and CI does not depend on
that service being up. Follow that pattern if a live check is ever genuinely wanted; otherwise draft-API and
esports-leaderboard deserialization are tested against saved sample payloads.

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

- the migrator runs clean on an empty database and on one that already has `accounts`;
- `pragma foreign_keys` is on, and an entry for a user with no `tournament_players` row is rejected;
- **player binding**: a second profile for the same Discord user replaces the main rather than adding one; the
  same profile claimed by a second user is rejected; a rebind is refused while the user has an entry in a
  `running` tournament, and an entry keeps its snapshotted `aoe4_id` across a permitted rebind;
- **`accounts` is untouched** by every tournament code path — a user bound there is not implicitly a tournament
  player, and vice versa;
- seeding tiers correctly when only some entrants have an `atr`, and an organizer's `seed` override survives
  bracket generation;
- draft import maps slots correctly (slot 1 is the higher seed);
- re-import overwrites `draft_import` rows and preserves `manual` ones;
- **completion is derived, not read**: a payload with `status = "running"` and a Bo3 score of 2–0 completes the
  set, and one with `status = "running"` at 1–0 does not;
- **the draft-channel post fires exactly once** across repeated poll ticks, still fires for a draft first
  observed with both seats already claimed, and omits player names when the preset is `anonymous`;
- **redraft** overwrites the pointer, increments `redraft_count`, voids that set's `draft_import` games while
  preserving `manual` ones, clears the announcement handle, and is refused on a `completed` set;
- a set reaching a majority of its games completes, eliminates the loser, and places the winner in the correct
  slot of the next set;
- a draft's reported `score` disagreeing with the imported games is flagged rather than silently resolved;
- **lifecycle transitions**: every illegal move is rejected — starting before check-in closes, checking in on a
  `running` tournament, registering after start, starting with non-contiguous seeds;
- **check-in**: a second check-in is idempotent; an unregistered user is rejected; closing marks exactly the
  non-checked-in entrants `no_show` and seeds only the rest;
- **registration**: a first sign-up writes the player row and the entry in one transaction, and neither survives
  if the other fails; a second registration is idempotent; a later tournament needs no profile argument;
  withdrawal works only before start;
- **`setdone` on an unfinished draft** imports nothing and leaves the set untouched;
- **localization**: `from_discord_locale` maps `"zh-TW"` and only `"zh-TW"` to `Locale::ZhTw` — `"zh-CN"`, an
  empty string, and an unrecognized future locale code all fall back to `Locale::En`; every retrofitted
  outcome/panel/refusal message renders correctly in both locales.

## 11. Follow-ups

Tracked separately; not part of this design.

Two follow-ups this document used to carry — sending a compliant User-Agent to aoe4world, and deleting the
leftover `Secrets.toml` from the removed Shuttle setup — have since been done, and are dropped from the list.

- **Result cross-checking.** `GET /api/v0/players/:profile_id/games?opponent_profile_id=X` returns games
  between two players with map, civs and winner, and supports `since=`/`updated_since=` for cheap incremental
  polling. This could verify the draft tool's results independently. Once migrations exist, adding
  `aoe4world_game_id` is a one-line `alter table`.
- **Autocomplete.** The `bind` autocomplete in `src/commands.rs` calls `players/search` on every keystroke with
  no caching; `GET /api/v0/players/autocomplete` is purpose-built for it.

## 12. Open questions

- **When, if ever, do we offer item 1 upstream?** The route is known — the tool is MIT, its `CONTRIBUTING.md` is
  explicitly issue-first, and agreed issues get an `accepting-pr` label — and the endpoint is small enough to
  write ourselves. Nothing is committed here; the question is only timing, and whether we run our own fork
  meanwhile.
- **Is a bot account acceptable to the tool's author?** Every draft we create is hosted by it and shows up in the
  tool's own history under that account. Worth disclosing rather than looking like an unusually busy player.
- **How many redrafts before `/set redraft` becomes admin-only?** Each one strands an undeletable room on
  someone else's server, and a button either player can press will occasionally be pressed in frustration.
- **How do entrants learn they need a draft-tool account,** and should check-in verify it rather than discovering
  it when a set opens? Discord login (§3.6) would retire this question rather than answer it.
- **If Discord login lands, does the seat instruction go away entirely,** or stay as the fallback for players who
  signed in with a password account?
- **Who may file a manual result override** (`/set report`) — both players, the winner only, or organizers — and
  does it need opponent confirmation? Distinct from `/set done`, which is open to either player because the
  draft tool is authoritative; a manual override bypasses that authority, so it may warrant a tighter rule.
- **Should the bot enforce an event-level map pool?** Currently no: the draft preset owns the pool. Adding a
  `tournament_maps` table would give organizers a pool the bot checks, at the cost of a second source of truth.
- **Civ/map key mapping** between the draft tool's kebab-case and aoe4world's snake_case still needs to be built
  and tested rather than assumed. Both vocabularies are now known exactly (§3.1), so this is work, not a question
  — but it is work nobody has done.
- **Should a first sign-up offer a profile from `accounts` as a default?** A user with exactly one row there has
  an unambiguous candidate, which would make their first registration a single button press. It is a read, not a
  dependency, but it is still coupling between two tables the design just separated — and it does nothing for a
  user with several. Currently no.
- **Who resolves a contested profile** — two Discord users claiming the same `aoe4_id`? The constraint rejects the
  second; nothing says who adjudicates, or whether an admin can force a reassignment.
- **Registration roster contents** (§8.5) — names only, or names with ATR/ELO? Ratings make the field's strength
  visible during signup but turn registration into a public leaderboard, which some players dislike. The
  ephemeral confirmation shows a registrant their own numbers either way.
- **`MANAGE_GUILD` bypassing the admin list** (§8.2) — proposed for recoverability when a creator leaves the
  server, at the cost of letting any server admin act on any tournament.
- **Read-only bracket channel** (§8.1) — proposed, though some organizers like a chat-along bracket channel.
- **Check-in reminders** — should the bot ping registered players when check-in opens, or shortly before it
  closes, and in the register channel or by DM? Cheap to add on the existing cron; not requested. A DM would be
  the one player-facing message with no channel to live in (§8.9).
- **Scheduling** — `/set schedule` stores `scheduled_at`, but nothing acts on it: no reminders, no timezone
  handling.
