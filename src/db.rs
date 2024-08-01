use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use tracing::error;

#[derive(FromRow)]
pub(crate) struct Account {
    pub user_id: i64,
    pub aoe4_id: i64,
}

#[derive(FromRow)]
pub(crate) struct Reminder {
    pub user_id: i64,
    pub last_played: DateTime<Utc>,
}

pub(crate) async fn bind_account(pool: &PgPool, user_id: i64, aoe4_id: i64) -> Result<String, sqlx::Error> {
    sqlx::query("insert into accounts (user_id, aoe4_id) values ($1, $2)")
        .bind(user_id)
        .bind(aoe4_id)
        .execute(pool)
        .await
        .map_err(|err| {
            error!("database operation failed with error {}", err.to_string());
            err
        })?;

    Ok(format!("綁定discord帳號 `{}` 與世紀帝國四帳號 `{}` ", user_id, aoe4_id))
}

pub(crate) async fn list_all(pool: &PgPool) -> Result<Vec<Account>, sqlx::Error> {
    let accounts: Vec<Account> = sqlx::query_as("select user_id, aoe4_id from accounts")
        .fetch_all(pool)
        .await
        .map_err(|err| {
            error!("database operation failed with error {}", err.to_string());
            err
        })?;
    Ok(accounts)
}

pub(crate) async fn add_reminder(pool: &PgPool, user_id: i64, days: i32) -> Result<String, sqlx::Error> {
    sqlx::query("insert into reminders (user_id, days) values ($1, $2)")
        .bind(user_id)
        .bind(days)
        .execute(pool)
        .await
        .map_err(|err| {
            error!("database operation failed with error {}", err.to_string());
            err
        })?;

    Ok(format!(
        "新增天梯提醒 如果{}天沒打單挑積分將會有溫馨提示 敬請期待",
        days
    ))
}

pub(crate) async fn delete_reminder(pool: &PgPool, user_id: i64) -> Result<String, sqlx::Error> {
    sqlx::query("delete from reminders where user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|err| {
            error!("database operation failed with error {}", err.to_string());
            err
        })?;

    Ok("已解除天梯提醒".to_string())
}

pub(crate) async fn list_reminder_needed(pool: &PgPool) -> Vec<Reminder> {
    sqlx::query_as(
        "select
                user_id, last_played
            from reminders
            where
                extract(day from now() - last_played) > days
                and extract(day from now() - last_reminded) > 5",
    )
    .fetch_all(pool)
    .await
    .inspect_err(|err| {
        error!("database operation failed with error {}", err.to_string());
    })
    .unwrap_or_else(|_| Vec::new())
}

pub(crate) async fn reminder_update_last_played(pool: &PgPool, user_id: i64, last_played: DateTime<Utc>) {
    let _ = sqlx::query("update reminders set last_played = greatest(last_played, $1) where user_id = $2")
        .bind(last_played)
        .bind(user_id)
        .execute(pool)
        .await
        .inspect_err(|err| {
            error!("database operation failed with error {}", err.to_string());
        });
}

pub(crate) async fn reminder_update_last_reminded(pool: &PgPool, user_id: i64) {
    let _ = sqlx::query("update reminders set last_reminded = now() where user_id = $2")
        .bind(user_id)
        .execute(pool)
        .await
        .inspect_err(|err| {
            error!("database operation failed with error {}", err.to_string());
        });
}
