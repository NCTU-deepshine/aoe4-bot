use sqlx::{FromRow, PgPool};

#[derive(FromRow)]
struct Account {
    pub user_id: i32,
    pub aoe4_id: i32,
}

pub(crate) async fn bind_account(pool: &PgPool, user_id: i32, aoe4_id: i32) -> Result<String, sqlx::Error> {
    sqlx::query("INSERT INTO todos (user_id, aoe4_id) VALUES ($1, $2)")
        .bind(user_id)
        .bind(aoe4_id)
        .execute(pool)
        .await?;

    Ok(format!("Bound discord user `{}` to aoe4 world profile `{}` ", user_id, aoe4_id))
}