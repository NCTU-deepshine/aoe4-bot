# aoe4-bot

A Discord bot for an Age of Empires IV server. It binds Discord users to their
[aoe4world](https://aoe4world.com) profiles, publishes a server-wide ranking, and nags people who
stop laddering.

Built with [poise](https://github.com/serenity-rs/poise) on
[serenity](https://github.com/serenity-rs/serenity), with SQLite via sqlx.

## Commands

| Command | Description |
| --- | --- |
| `/bind id <aoe4_id>` | Bind your Discord account to an aoe4world profile id |
| `/bind name <name>` | Same, but search by in-game name |
| `/查分 <aoe4_id>` | Look up a player's ranked stats |
| `/refresh` | Rebuild the ranking channel |
| `/rebuild` | Re-import bindings by scanning channel history |

## Running

Two environment variables are required:

```
DISCORD_TOKEN=<bot token>
GUILD_ID=<discord guild id>
```

```sh
cargo run
```

The database is SQLite at `/data/bot.db`, created on first run; `schema.sql` is applied at startup.

## Development

```sh
./check.sh           # format, then lint
./check.sh --check   # verify formatting, lint as errors, run tests — what CI runs
cargo test           # unit and in-memory database tests
cargo test -- --ignored   # additionally hit the live aoe4world API
```

## Deployment

Pushing to `main` deploys to [Fly.io](https://fly.io) via `.github/workflows/fly-deploy.yml`. The
SQLite database lives on a persistent volume mounted at `/data`.
