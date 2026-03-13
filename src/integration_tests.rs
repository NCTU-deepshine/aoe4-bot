#[cfg(test)]
mod tests {
    use sqlx::{SqlitePool, Executor};
    use crate::db::{bind_account, select_accounts, list_all};

    #[tokio::test]
    async fn reproduce_conflict_error() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        
        // Create table WITHOUT unique constraint to see if it reproduces the error
        // Note: The error "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"
        // occurs when you try to use ON CONFLICT (col) where col is not unique.
        pool.execute("create table accounts (
            id integer primary key autoincrement,
            user_id bigint not null,
            aoe4_id bigint not null
        )").await.unwrap();
        
        let result = bind_account(&pool, 123, 456).await;
        
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        println!("Error: {}", err);
        assert!(err.contains("ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"));
    }

    #[tokio::test]
    async fn test_with_schema_sql() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let schema = std::fs::read_to_string("schema.sql").unwrap();
        pool.execute(schema.as_str()).await.unwrap();
        
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
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let schema = std::fs::read_to_string("schema.sql").unwrap();
        pool.execute(schema.as_str()).await.unwrap();
        
        let user_id = 12345;
        let aoe4_id1 = 111;
        let aoe4_id2 = 222;
        
        // Bind first account
        let _ = bind_account(&pool, user_id, aoe4_id1).await.unwrap();
        
        // Bind second account to SAME user
        let _ = bind_account(&pool, user_id, aoe4_id2).await.unwrap();
        
        let accounts = select_accounts(&pool, user_id).await;
        assert_eq!(accounts.len(), 2);
        
        let ids: Vec<i64> = accounts.iter().map(|a| a.aoe4_id).collect::<Vec<_>>();
        assert!(ids.contains(&aoe4_id1));
        assert!(ids.contains(&aoe4_id2));
        
        // Verify list_all
        let all = list_all(&pool).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_aoe4_id_unique_constraint() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let schema = std::fs::read_to_string("schema.sql").unwrap();
        pool.execute(schema.as_str()).await.unwrap();
        
        let user1 = 123;
        let user2 = 456;
        let aoe4_id = 789;
        
        // Bind aoe4_id to user1
        let _ = bind_account(&pool, user1, aoe4_id).await.unwrap();
        
        // Try to bind SAME aoe4_id to user2
        // This should UPDATE the owner to user2 because of ON CONFLICT (aoe4_id) DO UPDATE SET user_id = excluded.user_id
        let _ = bind_account(&pool, user2, aoe4_id).await.unwrap();
        
        let accounts1 = select_accounts(&pool, user1).await;
        assert_eq!(accounts1.len(), 0);
        
        let accounts2 = select_accounts(&pool, user2).await;
        assert_eq!(accounts2.len(), 1);
        assert_eq!(accounts2[0].aoe4_id, aoe4_id);
    }
}
