use anyhow::Context as _;
use reqwest::Url;
use serde::Deserialize;
use serenity::all::Timestamp;
use serenity::model::id::GuildId;
use serenity::prelude::*;
use shuttle_runtime::SecretStore;
use sqlx::{Executor, PgPool};
use crate::db::bind_account;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

mod db;

struct Data {
    database: PgPool,
    guild_id: GuildId,
}

#[poise::command(slash_command)]
async fn hello(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("world!").await?;
    Ok(())
}

#[poise::command(slash_command, subcommands("id"), subcommand_required, aliases("綁定"))]
pub async fn bind(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub async fn id(ctx: Context<'_>, aoe4_id: i32) -> Result<(), Error> {
    let user_id = ctx.author().id;
    ctx.say("bind!").await?;
    let message = bind_account(&ctx.data().database, i32::try_from(u64::from(user_id)).unwrap(), aoe4_id).await?;
    ctx.say(message).await?;
    Ok(())
}

#[derive(Deserialize, Debug)]
struct Profile {
    name: String,
    modes: Modes
}

#[derive(Deserialize, Debug)]
struct Modes {
    rm_solo: RankedData,
    rm_1v1_elo: RankedEloData
}

#[derive(Deserialize, Debug)]
struct RankedData {
    rank_level: String,
    games_count: i32,
    civilizations: Vec<CivData>
}

#[derive(Deserialize, Debug)]
struct CivData {
    civilization: String,
    pick_rate: f64,
}

#[derive(Deserialize, Debug)]
struct RankedEloData {
    rating: i32,
    max_rating: i32,
    max_rating_1m: i32,
    rank: i32,
    last_game_at: Timestamp
}

#[tokio::test]
async fn get_profile() -> Result<(), Error>{
    let id = "76561198086414555";
    let url = Url::parse("https://aoe4world.com/api/v0/players/")?.join(id)?;
    let profile = reqwest::get(url).await?.json::<Profile>().await?;
    println!("In game name: {}, top civ: {}, max rating: {}, last played: {}", profile.name, profile.modes.rm_solo.civilizations[0].civilization, profile.modes.rm_1v1_elo.max_rating, profile.modes.rm_1v1_elo.last_game_at);
    Ok(())
}


#[shuttle_runtime::main]
async fn serenity(
    #[shuttle_shared_db::Postgres] pool: PgPool,
    #[shuttle_runtime::Secrets] secret_store: SecretStore,
) -> shuttle_serenity::ShuttleSerenity {
    // Get the discord token set in `Secrets.toml`
    let token = secret_store
        .get("DISCORD_TOKEN")
        .context("'DISCORD_TOKEN' was not found")?;
    // Get the guild_id set in `Secrets.toml`
    let guild_id: GuildId = secret_store
        .get("GUILD_ID")
        .context("'GUILD_ID' was not found")?
        .parse()
        .unwrap();

    // Run the schema migration
    pool.execute(include_str!("../schema.sql"))
        .await
        .context("failed to run migrations")?;

    let data = Data {
        database: pool,
        guild_id,
    };

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![hello(), bind(), id()],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id.clone()).await?;
                Ok(data)
            })
        })
        .build();

    let client = Client::builder(&token, GatewayIntents::empty())
        .framework(framework)
        .await
        .expect("Err creating client");

    Ok(client.into())
}
