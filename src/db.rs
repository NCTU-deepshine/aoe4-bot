use sqlx::{FromRow, PgPool};

#[derive(FromRow)]
pub(crate) struct Account {
    pub user_id: i32,
    pub aoe4_id: i32,
}

pub(crate) async fn bind_account(pool: &PgPool, user_id: i32, aoe4_id: i32) -> Result<String, sqlx::Error> {
    sqlx::query("INSERT INTO accounts (user_id, aoe4_id) VALUES ($1, $2)")
        .bind(user_id)
        .bind(aoe4_id)
        .execute(pool)
        .await?;

    Ok(format!(
        "Bound discord user `{}` to aoe4 world profile `{}` ",
        user_id, aoe4_id
    ))
}

pub(crate) async fn list_all(pool: &PgPool) -> Result<Vec<Account>, sqlx::Error> {
    let accounts: Vec<Account> = sqlx::query_as("select user_id, aoe4_id from accounts")
        .fetch_all(pool)
        .await?;
    Ok(accounts)
}
