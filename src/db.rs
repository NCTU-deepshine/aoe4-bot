use sqlx::{FromRow, PgPool};
use tracing::error;

#[derive(FromRow)]
pub(crate) struct Account {
    pub user_id: i64,
    pub aoe4_id: i64,
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

    Ok(format!(
        "Bound discord user `{}` to aoe4 world profile `{}` ",
        user_id, aoe4_id
    ))
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
