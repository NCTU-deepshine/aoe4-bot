use serenity::all::{ChannelId, MessageId, UserId};
use sqlx::{FromRow, SqlitePool};
use tracing::error;

// Discord snowflakes are u64 but SQLite integers are signed, so they are stored as the same 64
// bits reinterpreted. `as` round-trips every value exactly; `i64::try_from` would panic instead.
// `impl Into<u64>` so one function covers users, channels, messages and roles alike.
pub(crate) fn to_db_id(id: impl Into<u64>) -> i64 {
    id.into() as i64
}

// The way back needs one per type, since each id is its own newtype. Same
// reinterpretation, so `to_db_id` and these round-trip exactly.
pub(crate) fn to_user_id(id: i64) -> UserId {
    UserId::new(id as u64)
}

pub(crate) fn to_channel_id(id: i64) -> ChannelId {
    ChannelId::new(id as u64)
}

pub(crate) fn to_message_id(id: i64) -> MessageId {
    MessageId::new(id as u64)
}

#[derive(FromRow)]
pub(crate) struct Account {
    pub user_id: i64,
    pub aoe4_id: i64,
}

pub(crate) async fn bind_account(pool: &SqlitePool, user_id: i64, aoe4_id: i64) -> Result<String, sqlx::Error> {
    sqlx::query("insert into accounts (user_id, aoe4_id) values (?1, ?2) on conflict (aoe4_id) do update set user_id = excluded.user_id")
        .bind(user_id)
        .bind(aoe4_id)
        .execute(pool)
        .await
        .inspect_err(|err| {
            error!("database operation failed with error {}", err.to_string());
        })?;

    Ok(format!("綁定discord帳號 `{}` 與世紀帝國四帳號 `{}` ", user_id, aoe4_id))
}

pub(crate) async fn list_all(pool: &SqlitePool) -> Result<Vec<Account>, sqlx::Error> {
    let accounts: Vec<Account> = sqlx::query_as("select user_id, aoe4_id from accounts")
        .fetch_all(pool)
        .await
        .inspect_err(|err| {
            error!("database operation failed with error {}", err.to_string());
        })?;
    Ok(accounts)
}
