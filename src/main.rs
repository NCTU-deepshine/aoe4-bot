use crate::emperor::Emperor;
use crate::guilds::{Feature, Guilds};
use crate::refresh::do_refresh;
use serenity::all::Http;
use serenity::prelude::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

mod aoe4world;
mod commands;
mod db;
mod emperor;
mod errors;
mod guilds;
#[cfg(test)]
mod integration_tests;
mod ranked;
mod refresh;
mod reply;

struct Data {
    database: SqlitePool,
    guilds: Guilds,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
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

    let pool_cloned = pool.clone();
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

                Ok(Data {
                    database: pool_cloned,
                    guilds,
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
                    async move {
                        let http = Http::new(&token_cloned);
                        let data = Data {
                            database: pool_cloned,
                            guilds,
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

    /// The two guilds' command lists must never overlap (docs/tournament.md §8.0):
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
