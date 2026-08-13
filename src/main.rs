use crate::emperor::Emperor;
use crate::guilds::{Feature, Guilds};
use crate::refresh::do_refresh;
use crate::tournament::throttle::EditThrottle;
use serenity::all::Http;
use serenity::prelude::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;
use std::sync::Arc;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

mod aoe4world;
mod commands;
mod db;
// The draft-state read is callable but uncalled until chunks 39/40 land.
#[allow(dead_code)]
mod drafttool;
mod emperor;
mod errors;
mod guilds;
#[cfg(test)]
mod integration_tests;
mod locale;
mod ranked;
mod refresh;
mod reply;
mod tournament;

struct Data {
    database: SqlitePool,
    guilds: Guilds,
    // Shared with `tournament::dispatch::Dispatcher` so edits throttle across
    // both — a command-triggered panel edit and a
    // button-triggered one must coalesce against each other, not just within
    // their own path, which is only true if both hold the same instance.
    panel_throttle: Arc<EditThrottle>,
}

#[tokio::main]
async fn main() {
    // Not `fmt::init()`: that reads RUST_LOG only with the env-filter feature, and
    // silently pins the level at INFO without it. Same default, but overridable.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    info!("starting app");

    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set");
    let guilds = Guilds::configured();

    let conn_opts = SqliteConnectOptions::from_str("sqlite:///data/bot.db")
        .expect("failed to parse database url")
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(20)
        .connect_with(conn_opts)
        .await
        .expect("failed to connect to database");

    // The pre-migration schema. Kept as a single batch so the database already on the
    // Fly volume is unaffected by the migrator below.
    pool.execute(include_str!("../schema.sql"))
        .await
        .expect("failed to apply schema.sql");

    // Versioned migrations, tracked by sqlx in its own _sqlx_migrations table. Runs
    // after schema.sql; everything from the tournament feature onwards lives here.
    sqlx::migrate!().run(&pool).await.expect("failed to run migrations");

    // One list per guild. poise needs every command in a
    // single `commands` vec to dispatch them, so the two lists are concatenated and
    // the boundary kept as an index — Command is not Clone, so slicing the one vec is
    // how both halves stay available.
    let mut all_commands = commands::home();
    let home_count = all_commands.len();
    all_commands.extend(commands::tournament());

    // One instance, shared between the poise `Data` (the slash-command path) and
    // the `Dispatcher` event handler (the button path) below — see the field
    // doc comment on `Data::panel_throttle`.
    let panel_throttle = Arc::new(EditThrottle::new(tournament::panel::PANEL_EDIT_MIN_INTERVAL));

    let pool_cloned = pool.clone();
    let boot_pool = pool.clone();
    let panel_throttle_cloned = panel_throttle.clone();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: all_commands,
            on_error: |error| Box::pin(errors::on_error(error)),
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                let registered = &framework.options().commands;
                let (home, tournament) = registered.split_at(home_count);

                // Fatal: without its commands the bot is useless.
                poise::builtins::register_in_guild(ctx, home, guilds.guild_for(Feature::Home)).await?;

                let tournament_guild = guilds.guild_for(Feature::Tournament);
                if let Err(err) = poise::builtins::register_in_guild(ctx, tournament, tournament_guild).await {
                    error!("could not register tournament commands in guild {tournament_guild}: {err:?}");
                }

                // Spawned rather than awaited, since `setup` must return `Data` promptly.
                let boot_http = ctx.http.clone();
                tokio::spawn(async move {
                    tournament::startup::reconcile_all(boot_http, &boot_pool).await;
                });

                Ok(Data {
                    database: pool_cloned,
                    guilds,
                    panel_throttle: panel_throttle_cloned,
                })
            })
        })
        .build();

    info!("prepared frameworks");

    let mut client = Client::builder(
        &token,
        GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT,
    )
    .framework(framework)
    .event_handler(Emperor::new(guilds))
    .event_handler(tournament::dispatch::Dispatcher::new(
        guilds,
        pool.clone(),
        panel_throttle.clone(),
    ))
    .await
    .expect("Err creating client");
    info!("prepared client");

    let sched = JobScheduler::new().await.unwrap();
    sched
        .add(
            Job::new_async("0 0 0,12 * * *", move |_uuid, _l| {
                Box::pin({
                    let token_cloned = token.clone();
                    let pool_cloned = pool.clone();
                    let panel_throttle_cloned = panel_throttle.clone();
                    async move {
                        let http = Http::new(&token_cloned);
                        let data = Data {
                            database: pool_cloned,
                            guilds,
                            panel_throttle: panel_throttle_cloned,
                        };
                        info!("refresh triggered by cron");
                        do_refresh(&http, &data).await.unwrap();
                    }
                })
            })
            .unwrap(),
        )
        .await
        .unwrap();
    sched.start().await.unwrap();

    info!("starting serenity client");
    client.start().await.unwrap();
}

#[cfg(test)]
mod tests {
    use serenity::all::GatewayIntents;

    #[test]
    fn test_intents() {
        let intents = GatewayIntents::non_privileged();
        assert!(intents.guild_emojis_and_stickers());
        assert!(intents.guild_message_reactions());
        assert!(intents.guild_message_typing());
    }

    /// The two guilds' command lists must never overlap:
    /// a command in both would be registered in both guilds, which is the leak the
    /// split exists to prevent. Empty on one side today, so this is here to fail the
    /// moment a later chunk adds a tournament command to the wrong list.
    #[test]
    fn command_lists_are_disjoint() {
        use std::collections::HashSet;

        let home: HashSet<String> = crate::commands::home().into_iter().map(|c| c.name).collect();
        let tournament: HashSet<String> = crate::commands::tournament().into_iter().map(|c| c.name).collect();

        let both: Vec<&String> = home.intersection(&tournament).collect();
        assert!(both.is_empty(), "registered in both guilds: {both:?}");
    }

    /// This asserts no behaviour — it is a canary that fails to compile the moment they are not available.
    #[test]
    fn serenity_builders_are_available() {
        use serenity::all::{CreateActionRow, CreateButton, CreateChannel, CreateThread, EditThread};

        let _ = CreateChannel::new("relic-cup-draft");
        let _ = CreateThread::new("R1M1");
        let _ = EditThread::new().archived(true);
        let _ = CreateActionRow::Buttons(vec![CreateButton::new("setdone:1")]);
    }
}
