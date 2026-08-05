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

    // Tournament schema (migrations/0002_tournament_schema.sql) gate tests.

    async fn setup_round(pool: &SqlitePool) -> (i64, i64) {
        let tournament_id = crate::tournament::db::insert_tournament(pool, "slug", "Name", 1)
            .await
            .unwrap();
        let stage_id = crate::tournament::db::insert_stage(pool, tournament_id, 1, "Main Bracket", "single_elim")
            .await
            .unwrap();
        let round_id = crate::tournament::db::insert_round(pool, stage_id, 1, "Final", 3, None)
            .await
            .unwrap();
        (tournament_id, round_id)
    }

    async fn setup_set(pool: &SqlitePool) -> i64 {
        let (tournament_id, round_id) = setup_round(pool).await;
        crate::tournament::db::insert_set(pool, tournament_id, round_id, 1, None, None, "pending")
            .await
            .unwrap()
    }

    fn assert_check_constraint_failed<T>(result: Result<T, sqlx::Error>) {
        assert!(result.is_err(), "expected a check constraint violation, got Ok");
        let err = result.err().unwrap().to_string();
        assert!(err.contains("CHECK constraint failed"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn tournament_entry_requires_a_tournament_players_row() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();

        let result = crate::tournament::db::insert_entry(&pool, tournament_id, 999, 111, "Nobody").await;
        assert!(result.is_err(), "entry inserted without a tournament_players row");
        let err = result.err().unwrap().to_string();
        assert!(err.contains("FOREIGN KEY constraint failed"), "unexpected error: {err}");
    }

    async fn insert_player_row(
        pool: &SqlitePool,
        user_id: i64,
        aoe4_id: i64,
        display_name: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r"
            insert into tournament_players (user_id, aoe4_id, display_name)
            values (?1, ?2, ?3)
            ",
        )
        .bind(user_id)
        .bind(aoe4_id)
        .bind(display_name)
        .execute(pool)
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn tournament_players_user_id_is_the_primary_key() {
        let pool = test_pool().await;
        insert_player_row(&pool, 1, 100, "A").await.unwrap();

        let result = insert_player_row(&pool, 1, 200, "B").await;
        assert!(result.is_err(), "a second aoe4_id for the same user_id was accepted");
    }

    #[tokio::test]
    async fn tournament_players_aoe4_id_is_unique() {
        let pool = test_pool().await;
        insert_player_row(&pool, 1, 100, "A").await.unwrap();

        let result = insert_player_row(&pool, 2, 100, "B").await;
        assert!(
            result.is_err(),
            "a second user_id claiming the same aoe4_id was accepted"
        );
    }

    #[tokio::test]
    async fn tournaments_status_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let result = sqlx::query(
            r"
            insert into tournaments (slug, name, status, created_by)
            values (?1, ?2, 'bogus', ?3)
            ",
        )
        .bind("slug")
        .bind("Name")
        .bind(1_i64)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_stages_format_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();
        let result = sqlx::query(
            r"
            insert into tournament_stages (tournament_id, ordinal, name, format)
            values (?1, 1, 'Main', 'bogus')
            ",
        )
        .bind(tournament_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_stages_status_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();
        let result = sqlx::query(
            r"
            insert into tournament_stages (tournament_id, ordinal, name, status)
            values (?1, 1, 'Main', 'bogus')
            ",
        )
        .bind(tournament_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_rounds_bracket_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();
        let stage_id = crate::tournament::db::insert_stage(&pool, tournament_id, 1, "Main Bracket", "single_elim")
            .await
            .unwrap();
        let result = sqlx::query(
            r"
            insert into tournament_rounds (stage_id, ordinal, name, best_of, bracket)
            values (?1, 1, 'Final', 3, 'bogus')
            ",
        )
        .bind(stage_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_rounds_best_of_check_rejects_an_even_value() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();
        let stage_id = crate::tournament::db::insert_stage(&pool, tournament_id, 1, "Main Bracket", "single_elim")
            .await
            .unwrap();
        let result = sqlx::query(
            r"
            insert into tournament_rounds (stage_id, ordinal, name, best_of)
            values (?1, 1, 'Final', 4)
            ",
        )
        .bind(stage_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_entries_status_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();
        insert_player_row(&pool, 1, 100, "A").await.unwrap();

        let result = sqlx::query(
            r"
            insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, status)
            values (?1, 1, 100, 'A', 'bogus')
            ",
        )
        .bind(tournament_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_entries_atr_source_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();
        insert_player_row(&pool, 1, 100, "A").await.unwrap();

        let result = sqlx::query(
            r"
            insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, atr_source)
            values (?1, 1, 100, 'A', 'bogus')
            ",
        )
        .bind(tournament_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_sets_status_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let (tournament_id, round_id) = setup_round(&pool).await;
        let result = sqlx::query(
            r"
            insert into tournament_sets (tournament_id, round_id, position, status)
            values (?1, ?2, 1, 'bogus')
            ",
        )
        .bind(tournament_id)
        .bind(round_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_sets_winner_advances_to_slot_check_rejects_an_out_of_range_value() {
        let pool = test_pool().await;
        let (tournament_id, round_id) = setup_round(&pool).await;
        let result = sqlx::query(
            r"
            insert into tournament_sets (tournament_id, round_id, position, winner_advances_to_slot)
            values (?1, ?2, 1, 3)
            ",
        )
        .bind(tournament_id)
        .bind(round_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_games_status_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let set_id = setup_set(&pool).await;
        let result = sqlx::query(
            r"
            insert into tournament_games (set_id, game_number, status)
            values (?1, 1, 'bogus')
            ",
        )
        .bind(set_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn tournament_games_source_check_rejects_unknown_value() {
        let pool = test_pool().await;
        let set_id = setup_set(&pool).await;
        let result = sqlx::query(
            r"
            insert into tournament_games (set_id, game_number, source)
            values (?1, 1, 'bogus')
            ",
        )
        .bind(set_id)
        .execute(&pool)
        .await;
        assert_check_constraint_failed(result);
    }

    #[tokio::test]
    async fn set_player_display_name_updates_active_entries_but_not_completed_ones() {
        let pool = test_pool().await;
        let active_id = crate::tournament::db::insert_tournament(&pool, "active-slug", "Active", 1)
            .await
            .unwrap();
        let completed_id = crate::tournament::db::insert_tournament(&pool, "completed-slug", "Completed", 1)
            .await
            .unwrap();
        crate::tournament::db::update_tournament_status(&pool, completed_id, "completed")
            .await
            .unwrap();

        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "Old Name")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, active_id, 1, 100, "Old Name")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, completed_id, 1, 100, "Old Name")
            .await
            .unwrap();

        crate::tournament::db::set_player_display_name(&pool, 1, "New Name")
            .await
            .unwrap();

        let player = crate::tournament::db::get_player(&pool, 1).await.unwrap().unwrap();
        assert_eq!(player.display_name, "New Name");

        let active_entry = crate::tournament::db::get_entry(&pool, active_id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            active_entry.display_name, "New Name",
            "active entry did not pick up the new name"
        );

        let completed_entry = crate::tournament::db::get_entry(&pool, completed_id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            completed_entry.display_name, "Old Name",
            "completed entry's name should stay frozen"
        );
    }

    #[tokio::test]
    async fn update_player_binding_leaves_display_name_untouched() {
        let pool = test_pool().await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "Name")
            .await
            .unwrap();
        crate::tournament::db::update_player_binding(&pool, 1, 200)
            .await
            .unwrap();

        let player = crate::tournament::db::get_player(&pool, 1).await.unwrap().unwrap();
        assert_eq!(player.aoe4_id, 200);
        assert_eq!(player.display_name, "Name");
    }
}
