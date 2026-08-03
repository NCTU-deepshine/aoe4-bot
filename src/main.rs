use crate::emperor::Emperor;
use crate::refresh::do_refresh;
use serenity::all::Http;
use serenity::model::id::GuildId;
use serenity::prelude::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::info;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

mod aoe4world;
mod commands;
mod db;
mod emperor;
#[cfg(test)]
mod integration_tests;
mod ranked;
mod refresh;

struct Data {
    database: SqlitePool,
    guild_id: GuildId,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    info!("starting app");

    // Get the discord token set in `Secrets.toml`
    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set");
    // Get the guild_id set in `Secrets.toml`
    let guild_id: GuildId = std::env::var("GUILD_ID")
        .expect("GUILD_ID must be set")
        .parse()
        .expect("GUILD_ID must be a valid integer");

    let conn_opts = SqliteConnectOptions::from_str("sqlite:///data/bot.db")
        .expect("failed to parse database url")
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(20)
        .connect_with(conn_opts)
        .await
        .expect("failed to connect to database");

    // Run the schema migration
    pool.execute(include_str!("../schema.sql"))
        .await
        .expect("failed to run migrations");

    let pool_cloned = pool.clone();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::rebuild(),
                commands::bind(),
                commands::id(),
                commands::name(),
                commands::refresh(),
                commands::check(),
            ],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id).await?;
                Ok(Data {
                    database: pool_cloned,
                    guild_id,
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
    .event_handler(Emperor)
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
                            guild_id,
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
}
