#[cfg(test)]
mod tests {
    use crate::db::{bind_account, list_all};
    use sqlx::{Executor, SqlitePool};

    /// An in-memory pool set up exactly as main.rs sets up the real one: schema.sql
    /// first, then the versioned migrations. `include_str!` rather than a runtime
    /// relative path, so a test does not depend on the working directory.
    pub(crate) async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        pool.execute(include_str!("../schema.sql")).await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn migrator_runs_on_an_empty_database() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        let applied: i64 = sqlx::query_scalar("select count(*) from _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(applied > 0, "the migrator recorded nothing");
    }

    #[tokio::test]
    async fn migrator_runs_on_a_database_that_already_has_accounts() {
        // test_pool() is itself the case under test: schema.sql, then the migrator.
        let pool = test_pool().await;

        // And re-running is a no-op, which is what makes a restart safe.
        sqlx::migrate!().run(&pool).await.unwrap();

        let result = bind_account(&pool, 123, 456).await;
        assert!(result.is_ok(), "accounts unusable after migrating: {result:?}");
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        // sqlx enables this by default on SqliteConnectOptions, which is the same
        // default main.rs relies on. Every `references` in the tournament schema is
        // inert if it ever changes, so assert it rather than assume it.
        let pool = test_pool().await;

        let enabled: i64 = sqlx::query_scalar("pragma foreign_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(enabled, 1, "foreign keys are not enforced");
    }

    #[tokio::test]
    async fn reproduce_conflict_error() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();

        // Create table WITHOUT unique constraint to see if it reproduces the error
        // Note: The error "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"
        // occurs when you try to use ON CONFLICT (col) where col is not unique.
        pool.execute(
            "create table accounts (
            id integer primary key autoincrement,
            user_id bigint not null,
            aoe4_id bigint not null
        )",
        )
        .await
        .unwrap();

        let result = bind_account(&pool, 123, 456).await;

        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        println!("Error: {}", err);
        assert!(err.contains("ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"));
    }

    #[tokio::test]
    async fn test_with_schema_sql() {
        let pool = test_pool().await;

        let result = bind_account(&pool, 123, 456).await;
        assert!(result.is_ok());

        // Try to bind again with same aoe4_id but different user_id
        // This should UPDATE because of ON CONFLICT (aoe4_id) DO UPDATE SET user_id = excluded.user_id
        let result = bind_account(&pool, 789, 456).await;
        assert!(result.is_ok());

        let accounts = list_all(&pool).await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].user_id, 789);
    }

    #[tokio::test]
    async fn test_multi_account_binding() {
        let pool = test_pool().await;

        let user_id = 12345;
        let aoe4_id1 = 111;
        let aoe4_id2 = 222;

        // Bind first account
        let _ = bind_account(&pool, user_id, aoe4_id1).await.unwrap();

        // Bind second account to SAME user
        let _ = bind_account(&pool, user_id, aoe4_id2).await.unwrap();

        let all = list_all(&pool).await.unwrap();
        assert_eq!(all.len(), 2);

        let owned: Vec<i64> = all.iter().filter(|a| a.user_id == user_id).map(|a| a.aoe4_id).collect();
        assert_eq!(owned.len(), 2);
        assert!(owned.contains(&aoe4_id1));
        assert!(owned.contains(&aoe4_id2));
    }

    #[tokio::test]
    async fn test_aoe4_id_unique_constraint() {
        let pool = test_pool().await;

        let user1 = 123;
        let user2 = 456;
        let aoe4_id = 789;

        // Bind aoe4_id to user1
        let _ = bind_account(&pool, user1, aoe4_id).await.unwrap();

        // Try to bind SAME aoe4_id to user2
        // This should UPDATE the owner to user2 because of ON CONFLICT (aoe4_id) DO UPDATE SET user_id = excluded.user_id
        let _ = bind_account(&pool, user2, aoe4_id).await.unwrap();

        let all = list_all(&pool).await.unwrap();
        assert_eq!(all.iter().filter(|a| a.user_id == user1).count(), 0);

        let owned: Vec<&crate::db::Account> = all.iter().filter(|a| a.user_id == user2).collect();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].aoe4_id, aoe4_id);
    }
}
