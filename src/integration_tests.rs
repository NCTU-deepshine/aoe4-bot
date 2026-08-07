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

    /// A tournament announcing in `announce`, with its own output channels.
    async fn setup_tournament_in_channel(pool: &SqlitePool, slug: &str, announce: i64, base: i64) -> i64 {
        let id = crate::tournament::db::insert_tournament(pool, slug, slug, 1)
            .await
            .unwrap();
        crate::tournament::db::set_tournament_channels(
            pool,
            id,
            crate::tournament::db::TournamentChannels {
                category_id: None,
                announce_channel_id: announce,
                register_channel_id: base + 1,
                bracket_channel_id: base + 2,
                matches_channel_id: base + 3,
                draft_channel_id: base + 4,
            },
        )
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn a_shared_announce_channel_resolves_to_the_live_tournament() {
        // A finished tournament keeps its channel ids, so reusing the channel
        // matches two rows. Without an ordering, resolution fell to row order
        // and every command in the channel could hit last season's event.
        let pool = test_pool().await;
        let finished = setup_tournament_in_channel(&pool, "old", 900, 900).await;
        crate::tournament::db::update_tournament_status(&pool, finished, "completed")
            .await
            .unwrap();
        let live = setup_tournament_in_channel(&pool, "new", 900, 910).await;

        let resolved = crate::tournament::db::get_tournament_by_any_channel_id(&pool, 900)
            .await
            .unwrap()
            .expect("the channel should resolve to something");
        assert_eq!(resolved.id, live, "the live tournament owns the channel");
    }

    #[tokio::test]
    async fn a_live_tournament_holds_its_announce_channel() {
        let pool = test_pool().await;
        setup_tournament_in_channel(&pool, "cup", 900, 900).await;

        let held = crate::tournament::db::get_live_tournament_by_announce_channel(&pool, 900)
            .await
            .unwrap()
            .expect("a live tournament should block the channel");
        assert_eq!(held.slug, "cup");
    }

    #[tokio::test]
    async fn a_finished_tournament_frees_its_announce_channel_for_the_next_one() {
        // Otherwise a recurring series could never run twice in the same
        // channel without deleting its own history.
        let pool = test_pool().await;
        let finished = setup_tournament_in_channel(&pool, "cup", 900, 900).await;
        for status in ["completed", "canceled"] {
            crate::tournament::db::update_tournament_status(&pool, finished, status)
                .await
                .unwrap();
            assert!(
                crate::tournament::db::get_live_tournament_by_announce_channel(&pool, 900)
                    .await
                    .unwrap()
                    .is_none(),
                "{status} should not hold the channel"
            );
        }
    }

    #[tokio::test]
    async fn tournament_entry_requires_a_tournament_players_row() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "slug", "Name", 1)
            .await
            .unwrap();

        let result = crate::tournament::db::insert_entry(&pool, tournament_id, 999, 111, "Nobody", None).await;
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
        crate::tournament::db::insert_entry(&pool, active_id, 1, 100, "Old Name", None)
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, completed_id, 1, 100, "Old Name", None)
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

    // Chunk 7 (`/tournament create`, the admin list) gate tests.

    #[tokio::test]
    async fn set_tournament_channels_round_trips() {
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();

        crate::tournament::db::set_tournament_channels(
            &pool,
            id,
            crate::tournament::db::TournamentChannels {
                category_id: Some(10),
                announce_channel_id: 20,
                register_channel_id: 21,
                bracket_channel_id: 22,
                matches_channel_id: 23,
                draft_channel_id: 24,
            },
        )
        .await
        .unwrap();

        let tournament = crate::tournament::db::get_tournament(&pool, id).await.unwrap().unwrap();
        assert_eq!(tournament.category_id, Some(10));
        assert_eq!(tournament.announce_channel_id, Some(20));
        assert_eq!(tournament.register_channel_id, Some(21));
        assert_eq!(tournament.bracket_channel_id, Some(22));
        assert_eq!(tournament.matches_channel_id, Some(23));
        assert_eq!(tournament.draft_channel_id, Some(24));
    }

    #[tokio::test]
    async fn resolves_a_tournament_from_any_of_its_five_channels() {
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        crate::tournament::db::set_tournament_channels(
            &pool,
            id,
            crate::tournament::db::TournamentChannels {
                category_id: None,
                announce_channel_id: 100,
                register_channel_id: 101,
                bracket_channel_id: 102,
                matches_channel_id: 103,
                draft_channel_id: 104,
            },
        )
        .await
        .unwrap();

        for channel_id in [100, 101, 102, 103, 104] {
            let found = crate::tournament::db::get_tournament_by_any_channel_id(&pool, channel_id)
                .await
                .unwrap()
                .expect("every stored channel id should resolve the tournament");
            assert_eq!(found.id, id);
        }

        assert!(
            crate::tournament::db::get_tournament_by_any_channel_id(&pool, 999)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn admin_list_add_and_remove_round_trip() {
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        // Mirrors what `create` does: the creator is added as the first admin.
        crate::tournament::db::add_admin(&pool, id, 1, 1).await.unwrap();

        crate::tournament::db::add_admin(&pool, id, 2, 1).await.unwrap();
        let admins = crate::tournament::db::list_admins(&pool, id).await.unwrap();
        assert_eq!(admins.len(), 2);
        assert!(crate::tournament::db::is_admin(&pool, id, 2).await.unwrap());

        crate::tournament::db::remove_admin(&pool, id, 2).await.unwrap();
        let admins = crate::tournament::db::list_admins(&pool, id).await.unwrap();
        assert_eq!(admins.len(), 1);
        assert!(!crate::tournament::db::is_admin(&pool, id, 2).await.unwrap());
    }

    // Chunk 9 (registration, which is also binding) gate tests.

    async fn setup_tournament(pool: &SqlitePool, status: &str) -> crate::tournament::db::Tournament {
        let id = crate::tournament::db::insert_tournament(pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        if status != "registration" {
            crate::tournament::db::update_tournament_status(pool, id, status)
                .await
                .unwrap();
        }
        crate::tournament::db::get_tournament(pool, id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn register_new_player_and_entry_rolls_back_together_on_failure() {
        let pool = test_pool().await;
        // A non-existent tournament_id makes the entry insert fail its FK,
        // forcing the whole transaction to roll back — proving the player
        // insert does not survive on its own (docs/tournament.md §8.5: "neither
        // survives if the other fails").
        let result = crate::tournament::db::register_new_player_and_entry(&pool, 999, 1, 100, "A", Some(1200)).await;
        assert!(result.is_err());
        assert!(
            crate::tournament::db::get_player(&pool, 1).await.unwrap().is_none(),
            "the player row survived a rolled-back transaction"
        );
    }

    #[tokio::test]
    async fn register_new_player_and_entry_writes_the_elo_snapshot() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        crate::tournament::db::register_new_player_and_entry(&pool, tournament_id, 1, 100, "A", Some(1234))
            .await
            .unwrap();

        let entry = crate::tournament::db::get_entry(&pool, tournament_id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.elo, Some(1234));
    }

    #[tokio::test]
    async fn has_running_tournament_entry_reflects_status() {
        let pool = test_pool().await;
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament_id, 1, 100, "A", None)
            .await
            .unwrap();

        assert!(
            !crate::tournament::db::has_running_tournament_entry(&pool, 1)
                .await
                .unwrap()
        );

        crate::tournament::db::update_tournament_status(&pool, tournament_id, "running")
            .await
            .unwrap();
        assert!(
            crate::tournament::db::has_running_tournament_entry(&pool, 1)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn set_register_message_id_round_trips() {
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        crate::tournament::db::set_register_message_id(&pool, id, 555)
            .await
            .unwrap();

        let tournament = crate::tournament::db::get_tournament(&pool, id).await.unwrap().unwrap();
        assert_eq!(tournament.register_message_id, Some(555));
    }

    #[tokio::test]
    async fn register_reactivates_a_withdrawn_entry() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "A", None)
            .await
            .unwrap();
        crate::tournament::db::update_entry_status(&pool, tournament.id, 1, "withdrawn")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::Reactivated { .. }
        ));

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, "active");
    }

    #[tokio::test]
    async fn register_is_idempotent_for_an_active_entry() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "A", None)
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::AlreadyRegistered { .. }
        ));
    }

    #[tokio::test]
    async fn register_asks_a_first_timer_for_a_profile() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::NeedsProfileArgument
        );
    }

    #[tokio::test]
    async fn register_refuses_a_different_profile_when_already_bound() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, Some(200))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::AlreadyBoundToDifferentProfile { .. }
        ));
    }

    #[tokio::test]
    async fn register_with_your_own_current_profile_falls_through_to_normal_signup() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, Some(100))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::Registered { .. }
        ));
    }

    #[tokio::test]
    async fn register_rejects_a_profile_already_claimed_by_someone_else() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 2, 100, "B")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, Some(100))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::ProfileClaimedByAnother { other_user_id: 2, .. }
        ));
    }

    #[tokio::test]
    async fn register_is_refused_once_the_tournament_has_started() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "running").await;

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::RegistrationClosed
        );
    }

    #[tokio::test]
    async fn register_for_a_later_tournament_needs_no_profile_argument() {
        let pool = test_pool().await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        let tournament = setup_tournament(&pool, "registration").await;

        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::Registered { .. }
        ));

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.aoe4_id, 100);
    }

    #[tokio::test]
    async fn withdraw_outcomes_not_registered_success_then_idempotent() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;

        let outcome = crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::registration::WithdrawOutcome::NotRegistered);

        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "A", None)
            .await
            .unwrap();

        let outcome = crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::registration::WithdrawOutcome::Success);

        let outcome = crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::WithdrawOutcome::AlreadyWithdrawn
        );
    }

    #[tokio::test]
    async fn withdraw_is_refused_once_the_tournament_has_started() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "running").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::WithdrawOutcome::TournamentAlreadyStarted
        );
    }

    #[tokio::test]
    async fn rebind_requires_an_existing_profile() {
        let pool = test_pool().await;
        let outcome = crate::tournament::registration::rebind(&pool, 1, 100).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RebindOutcome::NoExistingProfile
        );
    }

    #[tokio::test]
    async fn rebind_rejects_a_profile_claimed_by_someone_else() {
        let pool = test_pool().await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_player_if_absent(&pool, 2, 200, "B")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::rebind(&pool, 1, 200).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RebindOutcome::ProfileClaimedByAnother { other_user_id: 2 }
        );
    }

    #[tokio::test]
    async fn rebind_is_refused_while_an_entry_is_running() {
        let pool = test_pool().await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        let tournament_id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament_id, 1, 100, "A", None)
            .await
            .unwrap();
        crate::tournament::db::update_tournament_status(&pool, tournament_id, "running")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::rebind(&pool, 1, 999).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RebindOutcome::RefusedRunningTournament
        );
    }

    // Chunk 10 (check-in) gate tests.

    #[tokio::test]
    async fn checkin_rejects_an_unregistered_user() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "checkin").await;

        let outcome = crate::tournament::checkin::checkin(&pool, &tournament, 1)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::checkin::CheckinOutcome::NotRegistered);
    }

    #[tokio::test]
    async fn checkin_rejects_a_withdrawn_entry() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "checkin").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "A", None)
            .await
            .unwrap();
        crate::tournament::db::update_entry_status(&pool, tournament.id, 1, "withdrawn")
            .await
            .unwrap();

        let outcome = crate::tournament::checkin::checkin(&pool, &tournament, 1)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::checkin::CheckinOutcome::NotRegistered);
    }

    #[tokio::test]
    async fn checkin_is_rejected_before_it_opens() {
        let pool = test_pool().await;
        let registration = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, registration.id, 1, 100, "A", None)
            .await
            .unwrap();

        let outcome = crate::tournament::checkin::checkin(&pool, &registration, 1)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::checkin::CheckinOutcome::CheckinNotOpen);
    }

    #[tokio::test]
    async fn checkin_is_rejected_after_it_closes() {
        let pool = test_pool().await;
        let seeding = setup_tournament(&pool, "seeding").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, seeding.id, 1, 100, "A", None)
            .await
            .unwrap();

        let outcome = crate::tournament::checkin::checkin(&pool, &seeding, 1).await.unwrap();
        assert_eq!(outcome, crate::tournament::checkin::CheckinOutcome::CheckinNotOpen);
    }

    #[tokio::test]
    async fn checkin_second_press_is_idempotent() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "checkin").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "A", None)
            .await
            .unwrap();

        let outcome = crate::tournament::checkin::checkin(&pool, &tournament, 1)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::checkin::CheckinOutcome::CheckedIn { .. }
        ));
        let first_checked_in_at = crate::tournament::db::get_entry(&pool, tournament.id, 1)
            .await
            .unwrap()
            .unwrap()
            .checked_in_at
            .unwrap();

        let outcome = crate::tournament::checkin::checkin(&pool, &tournament, 1)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::checkin::CheckinOutcome::AlreadyCheckedIn { .. }
        ));
        let checked_in_at = crate::tournament::db::get_entry(&pool, tournament.id, 1)
            .await
            .unwrap()
            .unwrap()
            .checked_in_at
            .unwrap();
        assert_eq!(checked_in_at, first_checked_in_at);
    }

    #[tokio::test]
    async fn open_checkin_is_refused_unless_the_tournament_is_in_registration() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "checkin").await;

        let outcome = crate::tournament::checkin::open(&pool, &tournament, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::checkin::OpenCheckinOutcome::NotInRegistration {
                current_status: "checkin".to_string()
            }
        );
    }

    /// Brings the scheduled start close enough that check-in may open. A fresh
    /// tournament is a week out by default, which is the tripwire.
    async fn make_checkin_openable(pool: &SqlitePool, id: i64) -> crate::tournament::db::Tournament {
        crate::tournament::db::set_scheduled_start_at(pool, id, chrono::Utc::now())
            .await
            .unwrap();
        crate::tournament::db::get_tournament(pool, id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn open_checkin_is_refused_until_an_hour_before_the_start() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;

        // Untouched, so the start is a week out and check-in is far too early.
        let outcome = crate::tournament::checkin::open(&pool, &tournament, None)
            .await
            .unwrap();
        assert!(
            matches!(outcome, crate::tournament::checkin::OpenCheckinOutcome::TooEarly { .. }),
            "{outcome:?}"
        );
        // And the status did not move.
        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, "registration");
    }

    #[tokio::test]
    async fn open_checkin_is_allowed_within_the_hour_before_the_start() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::set_scheduled_start_at(
            &pool,
            tournament.id,
            chrono::Utc::now() + chrono::Duration::minutes(30),
        )
        .await
        .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();

        let outcome = crate::tournament::checkin::open(&pool, &tournament, None)
            .await
            .unwrap();
        assert!(
            matches!(outcome, crate::tournament::checkin::OpenCheckinOutcome::Opened { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_new_tournament_starts_a_week_out_by_default() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        assert!(
            crate::tournament::setup::start_time_is_default(&tournament),
            "insert_tournament should place the placeholder start time"
        );
    }

    #[tokio::test]
    async fn open_checkin_moves_to_checkin_and_stores_closes_at() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        let tournament = make_checkin_openable(&pool, tournament.id).await;

        let outcome = crate::tournament::checkin::open(&pool, &tournament, Some(30))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::checkin::OpenCheckinOutcome::Opened { closes_at: Some(_) }
        ));

        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tournament.status, "checkin");
        assert!(tournament.checkin_closes_at.is_some());
    }

    #[tokio::test]
    async fn close_checkin_is_refused_unless_checkin_is_open() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;

        let outcome = crate::tournament::checkin::close(&pool, &tournament).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::checkin::CloseCheckinOutcome::NotOpen {
                current_status: "registration".to_string()
            }
        );
    }

    #[tokio::test]
    async fn close_checkin_marks_exactly_the_non_checked_in_entrants_no_show() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "checkin").await;

        // 1 checks in, 2 does not, 3 already withdrew before check-in even opened.
        for (user_id, aoe4_id) in [(1, 100), (2, 200), (3, 300)] {
            crate::tournament::db::insert_player_if_absent(&pool, user_id, aoe4_id, "P")
                .await
                .unwrap();
            crate::tournament::db::insert_entry(&pool, tournament.id, user_id, aoe4_id, "P", None)
                .await
                .unwrap();
        }
        crate::tournament::db::update_entry_status(&pool, tournament.id, 3, "withdrawn")
            .await
            .unwrap();
        crate::tournament::checkin::checkin(&pool, &tournament, 1)
            .await
            .unwrap();

        let outcome = crate::tournament::checkin::close(&pool, &tournament).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::checkin::CloseCheckinOutcome::Closed {
                checked_in_count: 1,
                no_show_count: 1
            }
        );

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let status_of = |user_id: i64| entries.iter().find(|e| e.user_id == user_id).unwrap().status.clone();
        assert_eq!(status_of(1), "active");
        assert_eq!(status_of(2), "no_show");
        assert_eq!(status_of(3), "withdrawn");

        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tournament.status, "seeding");
    }

    // Chunk 25 (/tournament reopen-registration) gate tests.

    /// A tournament in `status` with three entrants and a check-in round already
    /// run over them: 1 checked in, 2 was marked no-show, 3 withdrew beforehand.
    /// Panel handles are set so a reopen has something to clear.
    async fn setup_reopenable_tournament(pool: &SqlitePool, status: &str) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "checkin").await;
        for (user_id, aoe4_id) in [(1, 100), (2, 200), (3, 300)] {
            crate::tournament::db::insert_player_if_absent(pool, user_id, aoe4_id, "P")
                .await
                .unwrap();
            crate::tournament::db::insert_entry(pool, tournament.id, user_id, aoe4_id, "P", None)
                .await
                .unwrap();
        }
        crate::tournament::db::update_entry_status(pool, tournament.id, 3, "withdrawn")
            .await
            .unwrap();
        crate::tournament::checkin::checkin(pool, &tournament, 1).await.unwrap();
        crate::tournament::db::mark_no_shows(pool, tournament.id).await.unwrap();
        crate::tournament::db::set_checkin_message_id(pool, tournament.id, Some(999))
            .await
            .unwrap();
        crate::tournament::db::set_checkin_closes_at(pool, tournament.id, Some(chrono::Utc::now()))
            .await
            .unwrap();

        crate::tournament::db::update_tournament_status(pool, tournament.id, status)
            .await
            .unwrap();
        crate::tournament::db::get_tournament(pool, tournament.id)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn reopen_registration_is_refused_once_the_field_is_locked_in() {
        for status in ["running", "completed", "canceled"] {
            let pool = test_pool().await;
            let tournament = setup_reopenable_tournament(&pool, status).await;

            let outcome = crate::tournament::checkin::reopen_registration(&pool, &tournament)
                .await
                .unwrap();
            assert_eq!(
                outcome,
                crate::tournament::checkin::ReopenRegistrationOutcome::NotReopenable {
                    current_status: status.to_string()
                }
            );

            // Refused means untouched, not merely unreported.
            let after = crate::tournament::db::get_tournament(&pool, tournament.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after.status, status);
            assert_eq!(after.checkin_message_id, Some(999));
            let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
                .await
                .unwrap();
            assert!(entries.iter().any(|e| e.status == "no_show"));
        }
    }

    #[tokio::test]
    async fn reopen_registration_is_a_no_op_when_already_in_registration() {
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "registration").await;

        let outcome = crate::tournament::checkin::reopen_registration(&pool, &tournament)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::checkin::ReopenRegistrationOutcome::AlreadyInRegistration
        );

        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.checkin_message_id, Some(999));
        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        assert!(entries.iter().any(|e| e.checked_in_at.is_some()));
    }

    #[tokio::test]
    async fn reopen_registration_from_checkin_clears_the_whole_checkin_round() {
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "checkin").await;

        let outcome = crate::tournament::checkin::reopen_registration(&pool, &tournament)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::checkin::ReopenRegistrationOutcome::Reopened {
                restored_count: 1,
                cleared_count: 1
            }
        );

        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, "registration");
        assert_eq!(after.checkin_message_id, None);
        assert_eq!(after.checkin_closes_at, None);

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        assert!(entries.iter().all(|e| e.checked_in_at.is_none()));
    }

    #[tokio::test]
    async fn reopen_registration_from_seeding_restores_no_shows_but_not_withdrawals() {
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "seeding").await;

        crate::tournament::checkin::reopen_registration(&pool, &tournament)
            .await
            .unwrap();

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let status_of = |user_id: i64| entries.iter().find(|e| e.user_id == user_id).unwrap().status.clone();
        assert_eq!(status_of(1), "active");
        assert_eq!(status_of(2), "active", "the no-show should be back in the field");
        assert_eq!(status_of(3), "withdrawn", "a withdrawal is not a no-show");
    }

    #[tokio::test]
    async fn reopen_registration_clears_seeds() {
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "seeding").await;
        // Nothing writes these before chunk 11, so seed them by hand.
        crate::tournament::db::set_entry_seed(&pool, tournament.id, 1, Some(1), Some(2))
            .await
            .unwrap();

        crate::tournament::checkin::reopen_registration(&pool, &tournament)
            .await
            .unwrap();

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        assert!(entries.iter().all(|e| e.seed.is_none() && e.suggested_seed.is_none()));
    }

    #[tokio::test]
    async fn set_entry_elo_leaves_a_previously_fetched_atr_alone() {
        // The reason it exists rather than reusing set_entry_ratings, which
        // writes elo, atr and atr_source together and would blank the ATR that
        // seeding had already fetched.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "P")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "P", None)
            .await
            .unwrap();
        crate::tournament::db::set_entry_ratings(&pool, tournament.id, 1, Some(1200), Some(1800.5), Some("esports"))
            .await
            .unwrap();

        crate::tournament::db::set_entry_elo(&pool, tournament.id, 1, 1350)
            .await
            .unwrap();

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        assert_eq!(entries[0].elo, Some(1350));
        assert_eq!(entries[0].atr, Some(1800.5), "atr must survive an elo snapshot");
        assert_eq!(entries[0].atr_source.as_deref(), Some("esports"));
    }

    #[tokio::test]
    async fn a_sign_up_records_the_elo_it_was_given() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "P")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "P", Some(1234))
            .await
            .unwrap();

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        assert_eq!(entries[0].elo, Some(1234));
    }

    #[tokio::test]
    async fn the_first_entrant_in_a_fresh_tournament_is_number_one() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 42, 4200, "Me")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 42, None)
            .await
            .unwrap();

        match outcome {
            crate::tournament::registration::RegisterOutcome::Registered { entrant_number, .. } => {
                let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
                    .await
                    .unwrap();
                assert_eq!(
                    entries.len(),
                    1,
                    "rows: {:?}",
                    entries.iter().map(|e| e.user_id).collect::<Vec<_>>()
                );
                assert_eq!(entrant_number, 1);
            },
            other => panic!("expected Registered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_withdrawn_entrant_still_occupies_an_entrant_number() {
        // Why someone can be the only entrant in the field and still be told
        // they are #2: entries are never deleted (§4), and the number is a rank
        // by registration time over every row, not a count of the live field.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        for (user_id, aoe4_id) in [(1, 100), (2, 200)] {
            crate::tournament::db::insert_player_if_absent(&pool, user_id, aoe4_id, "P")
                .await
                .unwrap();
        }

        crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, None)
            .await
            .unwrap();
        let crate::tournament::registration::RegisterOutcome::Registered { entrant_number, .. } = outcome else {
            panic!("expected Registered");
        };
        assert_eq!(
            entrant_number, 2,
            "the withdrawn entrant keeps their place, so the only active entrant is #2"
        );
    }

    #[tokio::test]
    async fn a_refused_registration_writes_nothing() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;

        // No binding and no profile given.
        let outcome = crate::tournament::registration::register(&pool, &tournament, 1, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::NeedsProfileArgument
        );

        // A profile someone else already holds.
        crate::tournament::db::insert_player_if_absent(&pool, 2, 200, "Other")
            .await
            .unwrap();
        let outcome = crate::tournament::registration::register(&pool, &tournament, 3, Some(200))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::registration::RegisterOutcome::ProfileClaimedByAnother { .. }
        ));

        assert!(
            crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
                .await
                .unwrap()
                .is_empty(),
            "a refused registration must leave no entry behind"
        );
        assert!(
            crate::tournament::db::get_player(&pool, 1).await.unwrap().is_none(),
            "nor a half-written player binding"
        );
        assert!(crate::tournament::db::get_player(&pool, 3).await.unwrap().is_none());
    }

    // Bracket persistence and start (chunk 12).

    /// A seeded, checked-in field of `n`, configured enough to start.
    async fn setup_startable(pool: &SqlitePool, n: i64) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "seeding").await;
        for user_id in 1..=n {
            crate::tournament::db::insert_player_if_absent(pool, user_id, user_id * 100, "P")
                .await
                .unwrap();
            crate::tournament::db::insert_entry(pool, tournament.id, user_id, user_id * 100, "P", None)
                .await
                .unwrap();
        }
        let order: Vec<i64> = (1..=n).collect();
        crate::tournament::db::set_seed_order(pool, tournament.id, &order, true)
            .await
            .unwrap();
        crate::tournament::db::upsert_round_preset(pool, tournament.id, 0, "preset", "Standard Bo3", 3)
            .await
            .unwrap();
        crate::tournament::db::set_scheduled_start_at(pool, tournament.id, chrono::Utc::now())
            .await
            .unwrap();
        crate::tournament::db::get_tournament(pool, tournament.id)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn starting_persists_the_whole_bracket_and_links_advancement() {
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 8).await;

        let outcome = crate::tournament::start::start(&pool, &tournament).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::start::StartOutcome::Started { entrants: 8, rounds: 3 }
        );

        let sets = crate::tournament::db::list_sets_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        assert_eq!(sets.len(), 7, "an 8-bracket is 4 + 2 + 1 sets");

        // Exactly one set has nowhere to advance to: the final.
        let finals: Vec<_> = sets.iter().filter(|s| s.winner_advances_to_set_id.is_none()).collect();
        assert_eq!(finals.len(), 1, "advancement should form a single-rooted tree");

        // And every link points at a set that exists, in a slot that is 1 or 2.
        let ids: Vec<i64> = sets.iter().map(|s| s.id).collect();
        for set in sets.iter().filter(|s| s.winner_advances_to_set_id.is_some()) {
            assert!(ids.contains(&set.winner_advances_to_set_id.unwrap()));
            assert!(matches!(set.winner_advances_to_slot, Some(1) | Some(2)));
        }
    }

    #[tokio::test]
    async fn starting_moves_the_tournament_to_running_and_stamps_it() {
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 4).await;

        crate::tournament::start::start(&pool, &tournament).await.unwrap();

        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, "running");
        assert!(after.started_at.is_some(), "started_at should be stamped");
    }

    #[tokio::test]
    async fn a_full_bracket_opens_every_first_round_set_and_no_others() {
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 4).await;

        crate::tournament::start::start(&pool, &tournament).await.unwrap();

        let sets = crate::tournament::db::list_sets_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        // Ordered by round then position, so the first two are round one.
        assert_eq!(sets[0].status, "ready");
        assert_eq!(sets[1].status, "ready");
        assert_eq!(sets[2].status, "pending", "the final waits on its feeders");
    }

    #[tokio::test]
    async fn byes_are_decided_at_start_and_their_occupant_advances() {
        let pool = test_pool().await;
        // 3 entrants in a 4-bracket: seed 1 is unopposed.
        let tournament = setup_startable(&pool, 3).await;

        crate::tournament::start::start(&pool, &tournament).await.unwrap();

        let sets = crate::tournament::db::list_sets_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let bye = sets
            .iter()
            .find(|s| s.status == "bye")
            .expect("seed 1 should have a bye");
        assert_eq!(bye.winner_user_id, Some(1), "the occupant wins it");
        assert!(bye.completed_at.is_some());

        // And seed 1 is already sitting in the final.
        let final_set = sets.last().unwrap();
        assert_eq!(final_set.slot1_user_id, Some(1));
        assert_eq!(final_set.status, "pending", "still waiting on the other half");
    }

    #[tokio::test]
    async fn starting_snapshots_each_rounds_preset_onto_the_round_row() {
        // The round row is where `set_thread::open` looks for a preset, so a null
        // here costs every set its draft room — and says so only in a log.
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 4).await;
        // `setup_startable` assigns "preset" to every round; give the final its own.
        crate::tournament::db::upsert_round_preset(&pool, tournament.id, 1, "final-preset", "Grand Final Bo5", 3)
            .await
            .unwrap();

        crate::tournament::start::start(&pool, &tournament).await.unwrap();

        let stage = crate::tournament::db::list_stages_for_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .remove(0);
        let rounds = crate::tournament::db::list_rounds_for_stage(&pool, stage.id)
            .await
            .unwrap();
        assert_eq!(rounds.len(), 2, "a 4-bracket is two rounds");
        // Ordered by ordinal, so outermost first — the same order the presets are
        // resolved in, which is the mapping that could silently come out reversed.
        assert_eq!(rounds[0].draft_preset_id.as_deref(), Some("preset"));
        assert_eq!(
            rounds[1].draft_preset_id.as_deref(),
            Some("final-preset"),
            "an assignment at depth 1 covers the final and nothing else"
        );
    }

    #[tokio::test]
    async fn announcing_a_set_records_the_message_id_and_a_redraft_clears_it() {
        // Chunk 17's half of chunk 20's contract: the announcement handle is stored
        // so the post can be edited on completion, and dropped when a redraft makes
        // it point at a superseded room.
        let pool = test_pool().await;
        let set_id = setup_set(&pool).await;

        crate::tournament::db::set_draft_pointer(&pool, set_id, "65f1a0")
            .await
            .unwrap();
        crate::tournament::db::set_draft_announce_message(&pool, set_id, 999)
            .await
            .unwrap();
        let set = crate::tournament::db::get_set(&pool, set_id).await.unwrap().unwrap();
        assert_eq!(set.draft_announce_message_id, Some(999));

        crate::tournament::db::set_draft_pointer(&pool, set_id, "65f1b1")
            .await
            .unwrap();
        let set = crate::tournament::db::get_set(&pool, set_id).await.unwrap().unwrap();
        assert_eq!(set.draft_external_id.as_deref(), Some("65f1b1"));
        assert_eq!(
            set.draft_announce_message_id, None,
            "a redraft must not leave the handle pointing at the old room's post"
        );
    }

    #[tokio::test]
    async fn a_set_fed_by_two_byes_is_playable_immediately() {
        let pool = test_pool().await;
        // 5 in an 8-bracket: seeds 1, 2 and 3 are unopposed, and round two's
        // lower set is fed by two of those byes.
        let tournament = setup_startable(&pool, 5).await;

        crate::tournament::start::start(&pool, &tournament).await.unwrap();

        let sets = crate::tournament::db::list_sets_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let byes = sets.iter().filter(|s| s.status == "bye").count();
        assert_eq!(byes, 3);

        let round_two_ready = sets
            .iter()
            .filter(|s| s.status == "ready" && s.slot1_user_id.is_some() && s.slot2_user_id.is_some())
            .count();
        assert!(
            round_two_ready >= 2,
            "the real round-one set and the two-bye round-two set should both be ready: {sets:?}",
            sets = sets
                .iter()
                .map(|s| (&s.status, s.slot1_user_id, s.slot2_user_id))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn starting_is_refused_outside_seeding_and_writes_nothing() {
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 4).await;
        crate::tournament::db::update_tournament_status(&pool, tournament.id, "registration")
            .await
            .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();

        let outcome = crate::tournament::start::start(&pool, &tournament).await.unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::start::StartOutcome::NotSeeding { .. }
        ));
        assert!(
            crate::tournament::db::list_sets_for_tournament(&pool, tournament.id)
                .await
                .unwrap()
                .is_empty(),
            "a refused start must not persist a bracket"
        );
    }

    #[tokio::test]
    async fn starting_is_refused_without_a_preset() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "seeding").await;
        crate::tournament::db::set_scheduled_start_at(&pool, tournament.id, chrono::Utc::now())
            .await
            .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();

        let outcome = crate::tournament::start::start(&pool, &tournament).await.unwrap();
        assert_eq!(outcome, crate::tournament::start::StartOutcome::NotConfigured);
    }

    #[tokio::test]
    async fn starting_is_refused_before_the_scheduled_time() {
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 4).await;
        crate::tournament::db::set_scheduled_start_at(
            &pool,
            tournament.id,
            chrono::Utc::now() + chrono::Duration::hours(2),
        )
        .await
        .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();

        let outcome = crate::tournament::start::start(&pool, &tournament).await.unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::start::StartOutcome::TooEarly { .. }
        ));
    }

    #[tokio::test]
    async fn starting_is_refused_when_a_withdrawal_left_a_seed_gap() {
        let pool = test_pool().await;
        let tournament = setup_startable(&pool, 4).await;
        // Withdrawal stays open through seeding (§8.4), which is how a gap happens.
        crate::tournament::db::update_entry_status(&pool, tournament.id, 2, "withdrawn")
            .await
            .unwrap();

        let outcome = crate::tournament::start::start(&pool, &tournament).await.unwrap();
        assert_eq!(outcome, crate::tournament::start::StartOutcome::SeedsNotContiguous);
    }

    // Bracket message reconciliation (chunk 29).

    #[tokio::test]
    async fn deleting_the_bracket_tail_removes_exactly_the_surplus() {
        // The field shrinking past a power of two leaves fewer chunks than last
        // time, and the leftovers are the bottom of a bracket that no longer
        // exists.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        for ordinal in 0..4 {
            crate::tournament::db::upsert_bracket_message(&pool, tournament.id, ordinal, 1000 + ordinal)
                .await
                .unwrap();
        }

        crate::tournament::db::delete_bracket_messages_from(&pool, tournament.id, 2)
            .await
            .unwrap();

        let left = crate::tournament::db::list_bracket_messages(&pool, tournament.id)
            .await
            .unwrap();
        assert_eq!(left.iter().map(|m| m.ordinal).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[tokio::test]
    async fn deleting_from_zero_clears_every_chunk() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::upsert_bracket_message(&pool, tournament.id, 0, 1000)
            .await
            .unwrap();

        crate::tournament::db::delete_bracket_messages_from(&pool, tournament.id, 0)
            .await
            .unwrap();

        assert!(
            crate::tournament::db::list_bracket_messages(&pool, tournament.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_bracket_message_belongs_to_its_own_tournament() {
        let pool = test_pool().await;
        let mine = setup_tournament(&pool, "registration").await;
        let theirs = crate::tournament::db::insert_tournament(&pool, "other-cup", "Other", 1)
            .await
            .unwrap();
        crate::tournament::db::upsert_bracket_message(&pool, mine.id, 0, 1000)
            .await
            .unwrap();
        crate::tournament::db::upsert_bracket_message(&pool, theirs, 0, 2000)
            .await
            .unwrap();

        crate::tournament::db::delete_bracket_messages_from(&pool, mine.id, 0)
            .await
            .unwrap();

        assert_eq!(
            crate::tournament::db::list_bracket_messages(&pool, theirs)
                .await
                .unwrap()
                .len(),
            1,
            "another tournament's bracket must be untouched"
        );
    }

    // Entrant cap gate tests (chunk 27).

    async fn register_nth(
        pool: &SqlitePool,
        tournament: &crate::tournament::db::Tournament,
        user_id: i64,
    ) -> crate::tournament::registration::RegisterOutcome {
        crate::tournament::db::insert_player_if_absent(pool, user_id, user_id * 100, "P")
            .await
            .unwrap();
        crate::tournament::registration::register(pool, tournament, user_id, None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_cap_defaults_to_32_without_any_setup() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        assert_eq!(tournament.entrant_cap, 32);
    }

    #[tokio::test]
    async fn registration_is_refused_once_the_field_is_full() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::set_entrant_cap(&pool, tournament.id, 2)
            .await
            .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            register_nth(&pool, &tournament, 1).await,
            crate::tournament::registration::RegisterOutcome::Registered { .. }
        ));
        assert!(matches!(
            register_nth(&pool, &tournament, 2).await,
            crate::tournament::registration::RegisterOutcome::Registered { .. }
        ));
        assert_eq!(
            register_nth(&pool, &tournament, 3).await,
            crate::tournament::registration::RegisterOutcome::FieldFull { cap: 2 }
        );
    }

    #[tokio::test]
    async fn a_withdrawal_frees_exactly_one_slot() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::set_entrant_cap(&pool, tournament.id, 2)
            .await
            .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        register_nth(&pool, &tournament, 1).await;
        register_nth(&pool, &tournament, 2).await;

        crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();

        assert!(matches!(
            register_nth(&pool, &tournament, 3).await,
            crate::tournament::registration::RegisterOutcome::Registered { .. }
        ));
        // And the field is full again.
        assert_eq!(
            register_nth(&pool, &tournament, 4).await,
            crate::tournament::registration::RegisterOutcome::FieldFull { cap: 2 }
        );
    }

    #[tokio::test]
    async fn rejoining_after_a_withdrawal_is_capped_too() {
        // Otherwise withdraw-then-rejoin walks straight past the cap: the slot
        // freed by the withdrawal has already been taken by someone else.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::set_entrant_cap(&pool, tournament.id, 2)
            .await
            .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        register_nth(&pool, &tournament, 1).await;
        register_nth(&pool, &tournament, 2).await;
        crate::tournament::registration::withdraw(&pool, &tournament, 1)
            .await
            .unwrap();
        register_nth(&pool, &tournament, 3).await;

        assert_eq!(
            register_nth(&pool, &tournament, 1).await,
            crate::tournament::registration::RegisterOutcome::FieldFull { cap: 2 }
        );
    }

    #[tokio::test]
    async fn round_presets_upsert_by_depth() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "seeding").await;

        crate::tournament::db::upsert_round_preset(&pool, tournament.id, 0, "A", "Standard Bo3", 3)
            .await
            .unwrap();
        crate::tournament::db::upsert_round_preset(&pool, tournament.id, 1, "C", "Final Bo7", 7)
            .await
            .unwrap();
        // Re-assigning the same depth replaces rather than duplicating.
        crate::tournament::db::upsert_round_preset(&pool, tournament.id, 1, "C2", "Final Bo5", 5)
            .await
            .unwrap();

        let presets = crate::tournament::db::list_round_presets(&pool, tournament.id)
            .await
            .unwrap();
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].from_depth, 0);
        assert_eq!(presets[1].draft_preset_id, "C2");
        assert_eq!(presets[1].best_of, 5);
        // The display name is replaced with the rest, not left pointing at the
        // preset it superseded.
        assert_eq!(presets[0].preset_name.as_deref(), Some("Standard Bo3"));
        assert_eq!(presets[1].preset_name.as_deref(), Some("Final Bo5"));
    }

    // Self-unbind gate tests.

    #[tokio::test]
    async fn unbind_clears_the_binding_and_frees_the_profile() {
        let pool = test_pool().await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "Player")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::unbind(&pool, 1).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::UnbindOutcome::Unbound {
                display_name: "Player".to_string()
            }
        );
        assert!(crate::tournament::db::get_player(&pool, 1).await.unwrap().is_none());

        // The point of unbinding: aoe4_id is unique, so the profile must be
        // claimable again — by this user or another.
        crate::tournament::db::insert_player_if_absent(&pool, 2, 100, "Someone Else")
            .await
            .unwrap();
        let claimed = crate::tournament::db::get_player_by_aoe4_id(&pool, 100).await.unwrap();
        assert_eq!(claimed.map(|p| p.user_id), Some(2));
    }

    #[tokio::test]
    async fn unbind_is_refused_while_any_entry_exists_even_a_withdrawn_one() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "Player")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(&pool, tournament.id, 1, 100, "Player", None)
            .await
            .unwrap();
        // Withdrawing does not delete the row, so it must not unblock the unbind.
        crate::tournament::db::update_entry_status(&pool, tournament.id, 1, "withdrawn")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::unbind(&pool, 1).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::UnbindOutcome::BlockedByEntries { count: 1 }
        );
        assert!(
            crate::tournament::db::get_player(&pool, 1).await.unwrap().is_some(),
            "a refused unbind must leave the binding alone"
        );
    }

    #[tokio::test]
    async fn unbind_is_a_no_op_when_nothing_is_bound() {
        let pool = test_pool().await;
        let outcome = crate::tournament::registration::unbind(&pool, 999).await.unwrap();
        assert_eq!(outcome, crate::tournament::registration::UnbindOutcome::NotBound);
    }

    #[tokio::test]
    async fn unbind_leaves_the_home_guild_binding_alone() {
        // §4 keeps `accounts` and `tournament_players` deliberately unlinked.
        let pool = test_pool().await;
        bind_account(&pool, 1, 100).await.unwrap();
        crate::tournament::db::insert_player_if_absent(&pool, 1, 100, "Player")
            .await
            .unwrap();

        crate::tournament::registration::unbind(&pool, 1).await.unwrap();

        assert_eq!(list_all(&pool).await.unwrap().len(), 1, "accounts must be untouched");
    }

    // Chunk 11 (seeding) gate tests.

    /// A checked-in field of `n` entrants, unseeded.
    async fn setup_seedable_field(pool: &SqlitePool, n: i64) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "seeding").await;
        for user_id in 1..=n {
            crate::tournament::db::insert_player_if_absent(pool, user_id, user_id * 100, "P")
                .await
                .unwrap();
            crate::tournament::db::insert_entry(pool, tournament.id, user_id, user_id * 100, "P", None)
                .await
                .unwrap();
        }
        tournament
    }

    async fn seeds_by_user(pool: &SqlitePool, tournament_id: i64) -> Vec<(i64, Option<i64>)> {
        let mut entries = crate::tournament::db::list_entries_for_tournament(pool, tournament_id)
            .await
            .unwrap();
        entries.sort_by_key(|e| e.user_id);
        entries.iter().map(|e| (e.user_id, e.seed)).collect()
    }

    #[tokio::test]
    async fn set_seed_order_writes_one_to_n_in_the_given_order() {
        let pool = test_pool().await;
        let tournament = setup_seedable_field(&pool, 4).await;

        crate::tournament::db::set_seed_order(&pool, tournament.id, &[3, 1, 4, 2], true)
            .await
            .unwrap();

        assert_eq!(
            seeds_by_user(&pool, tournament.id).await,
            vec![(1, Some(2)), (2, Some(4)), (3, Some(1)), (4, Some(3))]
        );
    }

    #[tokio::test]
    async fn reordering_an_already_seeded_field_does_not_trip_the_unique_index() {
        // The regression the null-first transaction exists for: shifting everyone
        // down by one collides on the first row without it.
        let pool = test_pool().await;
        let tournament = setup_seedable_field(&pool, 4).await;
        crate::tournament::db::set_seed_order(&pool, tournament.id, &[1, 2, 3, 4], true)
            .await
            .unwrap();

        let shifted = crate::tournament::seeding::reorder(&[1, 2, 3, 4], 4, 1);
        crate::tournament::db::set_seed_order(&pool, tournament.id, &shifted, false)
            .await
            .unwrap();

        assert_eq!(
            seeds_by_user(&pool, tournament.id).await,
            vec![(1, Some(2)), (2, Some(3)), (3, Some(4)), (4, Some(1))]
        );
    }

    #[tokio::test]
    async fn an_organizer_override_leaves_the_suggestion_intact() {
        // §10's "an organizer's override survives" — and its converse: the panel
        // must still be able to show what the tiering proposed.
        let pool = test_pool().await;
        let tournament = setup_seedable_field(&pool, 3).await;
        crate::tournament::db::set_seed_order(&pool, tournament.id, &[1, 2, 3], true)
            .await
            .unwrap();

        crate::tournament::db::set_seed_order(&pool, tournament.id, &[3, 1, 2], false)
            .await
            .unwrap();

        let mut entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        entries.sort_by_key(|e| e.user_id);
        let suggested: Vec<Option<i64>> = entries.iter().map(|e| e.suggested_seed).collect();
        let actual: Vec<Option<i64>> = entries.iter().map(|e| e.seed).collect();
        assert_eq!(suggested, vec![Some(1), Some(2), Some(3)], "suggestion must not move");
        assert_eq!(actual, vec![Some(2), Some(3), Some(1)]);
    }

    #[tokio::test]
    async fn seeding_survives_a_reopen_which_clears_every_seed() {
        // Chunk 25 clears seeds; chunk 11 must be able to write a fresh order
        // afterwards without colliding with the ones it just removed.
        let pool = test_pool().await;
        let tournament = setup_seedable_field(&pool, 3).await;
        crate::tournament::db::set_seed_order(&pool, tournament.id, &[1, 2, 3], true)
            .await
            .unwrap();

        crate::tournament::db::clear_checkins(&pool, tournament.id)
            .await
            .unwrap();
        assert!(
            seeds_by_user(&pool, tournament.id)
                .await
                .iter()
                .all(|(_, s)| s.is_none()),
            "reopening should have cleared every seed"
        );

        crate::tournament::db::set_seed_order(&pool, tournament.id, &[2, 3, 1], true)
            .await
            .unwrap();
        assert_eq!(
            seeds_by_user(&pool, tournament.id).await,
            vec![(1, Some(3)), (2, Some(1)), (3, Some(2))]
        );
    }

    // Chunk 26 (/tournament delete) gate tests.

    /// A tournament with a row in every table that references it, so a delete has
    /// something to cascade into at each level.
    async fn setup_fully_populated_tournament(pool: &SqlitePool, slug: &str, user_id: i64) -> i64 {
        let tournament_id = crate::tournament::db::insert_tournament(pool, slug, "Name", user_id)
            .await
            .unwrap();
        crate::tournament::db::add_admin(pool, tournament_id, user_id, user_id)
            .await
            .unwrap();
        crate::tournament::db::insert_player_if_absent(pool, user_id, user_id * 100, "P")
            .await
            .unwrap();
        crate::tournament::db::insert_entry(pool, tournament_id, user_id, user_id * 100, "P", None)
            .await
            .unwrap();
        crate::tournament::db::upsert_bracket_message(pool, tournament_id, 1, 555)
            .await
            .unwrap();
        crate::tournament::db::upsert_round_preset(pool, tournament_id, 0, "preset", "Standard Bo3", 3)
            .await
            .unwrap();

        let stage_id = crate::tournament::db::insert_stage(pool, tournament_id, 1, "Main Bracket", "single_elim")
            .await
            .unwrap();
        let round_id = crate::tournament::db::insert_round(pool, stage_id, 1, "Final", 3, None)
            .await
            .unwrap();
        let set_id = crate::tournament::db::insert_set(pool, tournament_id, round_id, 1, None, None, "pending")
            .await
            .unwrap();
        crate::tournament::db::insert_game(
            pool,
            crate::tournament::db::NewGame {
                set_id,
                game_number: 1,
                map: None,
                slot1_civ: None,
                slot2_civ: None,
                winner_user_id: None,
                status: "pending".to_string(),
                source: "manual".to_string(),
                reported_by: None,
                reported_at: None,
            },
        )
        .await
        .unwrap();
        tournament_id
    }

    /// `table` only ever comes from `TOURNAMENT_SCOPED_TABLES` below — literals,
    /// with nothing external reaching the string.
    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Every table that hangs off a tournament. **Add to this when a migration
    /// adds one** — the cascade test is only as complete as this list, and a
    /// missing entry is a table that silently survives a delete.
    const TOURNAMENT_SCOPED_TABLES: [&str; 9] = [
        "tournaments",
        "tournament_admins",
        "tournament_entries",
        "tournament_bracket_messages",
        "tournament_stages",
        "tournament_rounds",
        "tournament_sets",
        "tournament_games",
        "tournament_round_presets",
    ];

    #[tokio::test]
    async fn delete_tournament_cascades_to_every_tournament_scoped_table() {
        let pool = test_pool().await;
        let tournament_id = setup_fully_populated_tournament(&pool, "relic-cup", 1).await;
        for table in TOURNAMENT_SCOPED_TABLES {
            assert_eq!(count(&pool, table).await, 1, "{table} should be populated up front");
        }

        crate::tournament::db::delete_tournament(&pool, tournament_id)
            .await
            .unwrap();

        for table in TOURNAMENT_SCOPED_TABLES {
            assert_eq!(count(&pool, table).await, 0, "{table} should have been cascaded away");
        }
    }

    #[tokio::test]
    async fn delete_tournament_leaves_the_global_player_binding_intact() {
        let pool = test_pool().await;
        let tournament_id = setup_fully_populated_tournament(&pool, "relic-cup", 1).await;

        crate::tournament::db::delete_tournament(&pool, tournament_id)
            .await
            .unwrap();

        // The Discord↔aoe4world binding is global (§4) — it must outlive any one
        // tournament, or a returning player would have to rebind.
        assert_eq!(count(&pool, "tournament_players").await, 1);
        let player = crate::tournament::db::get_player(&pool, 1).await.unwrap();
        assert!(player.is_some());
    }

    #[tokio::test]
    async fn delete_tournament_leaves_other_tournaments_untouched() {
        let pool = test_pool().await;
        let doomed = setup_fully_populated_tournament(&pool, "relic-cup", 1).await;
        let survivor = setup_fully_populated_tournament(&pool, "other-cup", 2).await;

        crate::tournament::db::delete_tournament(&pool, doomed).await.unwrap();

        // One row left everywhere, not zero — the assertion that catches a delete
        // that forgot its where clause.
        for table in TOURNAMENT_SCOPED_TABLES {
            assert_eq!(count(&pool, table).await, 1, "{table} should still hold the survivor");
        }
        assert!(
            crate::tournament::db::get_tournament(&pool, survivor)
                .await
                .unwrap()
                .is_some()
        );
    }
}
