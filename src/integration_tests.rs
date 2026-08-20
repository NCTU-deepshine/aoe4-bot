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
    async fn migrating_a_populated_database_defaults_seed_source_to_suggested() {
        // Every other migrator test starts from empty, so none of them would see
        // a column added `not null` landing on rows that already exist.
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        let tournament = crate::tournament::db::get_tournament(&pool, id).await.unwrap().unwrap();
        assert_eq!(tournament.seed_source, "suggested");
    }

    /// The version that relaxes `aoe4_id`. Named once so the two tests below and
    /// the reader agree on which migration is under test.
    const OPTIONAL_AOE4_ID: i64 = 7;

    /// The version that adds `invited_by`.
    const INVITED_ENTRANTS: i64 = 8;

    /// The version that adds `registration_mode`.
    const REGISTRATION_MODE: i64 = 9;

    /// A pool migrated to just *before* `version`, so a migration can be applied to
    /// a database that already holds rows.
    ///
    /// `sqlx::migrate!().run()` is all-or-nothing, so it cannot express this on its
    /// own: by the time it returns, the migration under test has already run against
    /// an empty database. Driving a trimmed `Migrator` instead keeps the real runner
    /// — including its bookkeeping, so the full migrator afterwards applies exactly
    /// the one migration left, through the same path production takes.
    async fn pool_migrated_to_before(version: i64) -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        pool.execute(include_str!("../schema.sql")).await.unwrap();

        let full = sqlx::migrate!();
        let earlier = sqlx::migrate::Migrator {
            migrations: full.iter().filter(|m| m.version < version).cloned().collect(),
            ..full
        };
        earlier.run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn recreating_the_player_tables_clears_the_feature_and_leaves_the_rest_alone() {
        // The migration discards every tournament rather than copying rows into the
        // rebuilt tables. What has to survive is the ranked board, which belongs to
        // another feature and shares no keys with this one.
        let pool = pool_migrated_to_before(OPTIONAL_AOE4_ID).await;
        crate::db::bind_account(&pool, 7, 700).await.unwrap();
        sqlx::query("insert into tournaments (slug, name, created_by) values ('cup', 'Cup', 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into tournament_players (user_id, aoe4_id, display_name) values (1, 100, 'A'), (2, 200, 'B')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, seed) \
             values (1, 1, 100, 'A', 1), (1, 2, 200, 'B', 2)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into tournament_stages (tournament_id, ordinal, name, format) values (1,1,'M','single_elim')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("insert into tournament_rounds (stage_id, ordinal, name, best_of) values (1,1,'Final',3)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into tournament_sets (tournament_id, round_id, position, slot1_user_id, slot2_user_id, \
             winner_user_id, status) values (1,1,1,1,2,1,'completed')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into tournament_games (set_id, game_number, winner_user_id, status, source) \
             values (1,1,1,'completed','manual')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        for table in [
            "tournaments",
            "tournament_players",
            "tournament_entries",
            "tournament_sets",
            "tournament_games",
        ] {
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(count, 0, "{table} should have been cleared");
        }
        assert_eq!(
            crate::db::list_all(&pool).await.unwrap().len(),
            1,
            "the ranked board is a separate feature and must survive"
        );

        // Recreating a table three others reference has to leave those references
        // resolving to it, not dangling.
        let dangling: i64 = sqlx::query_scalar("select count(*) from pragma_foreign_key_check")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(dangling, 0, "no table should reference one that is no longer there");
        for table in ["tournament_entries", "tournament_sets", "tournament_games"] {
            let targets: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select \"table\" from pragma_foreign_key_list('{table}')"
            )))
            .fetch_all(&pool)
            .await
            .unwrap();
            assert!(
                targets.iter().any(|(name,)| name == "tournament_players"),
                "{table} should still reference tournament_players, got {targets:?}"
            );
        }

        // Still enforced: an entry for a player row that was never created.
        let orphan = sqlx::query(
            "insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name) values (1, 999, 999, 'X')",
        )
        .execute(&pool)
        .await;
        assert!(orphan.is_err(), "an entry for an unknown player is still refused");
        // Migration 7 relaxed `aoe4_id` to nullable; 10 and 11 tightened both
        // tables back to `not null` once nothing ever left it empty again — so
        // by the time the full chain has run, a null is refused, not stored.
        let unbound =
            sqlx::query("insert into tournament_players (user_id, aoe4_id, display_name) values (3, null, 'Unbound')")
                .execute(&pool)
                .await;
        assert!(
            unbound.is_err(),
            "a player with no profile is refused again once the full chain has run"
        );
    }

    #[tokio::test]
    async fn aoe4_id_is_required_and_still_one_owner_per_profile() {
        let pool = test_pool().await;
        sqlx::query("insert into tournament_players (user_id, aoe4_id, display_name) values (1, 100, 'Bound')")
            .execute(&pool)
            .await
            .unwrap();

        // Migration 7 relaxed this to nullable so an unbound entrant could exist;
        // 11 tightened it back once nothing wrote a null any more.
        let unbound =
            sqlx::query("insert into tournament_players (user_id, aoe4_id, display_name) values (2, null, 'X')")
                .execute(&pool)
                .await;
        assert!(unbound.is_err(), "a player with no profile is refused");

        let err =
            sqlx::query("insert into tournament_players (user_id, aoe4_id, display_name) values (5, 100, 'Thief')")
                .execute(&pool)
                .await
                .expect_err("a second owner for one profile is still refused");
        // `registration::is_aoe4_id_conflict` matches this substring to word a
        // profile-claim race, so the rebuild must not have renamed the table or
        // moved the constraint off the column.
        assert!(
            err.to_string().contains("tournament_players.aoe4_id"),
            "the conflict must still name the column: {err}"
        );
    }

    #[tokio::test]
    async fn seed_source_rejects_a_value_outside_its_vocabulary() {
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();

        let result = crate::tournament::db::set_seed_source(&pool, id, "invited").await;
        assert!(result.is_err(), "the check constraint should have refused it");
    }

    #[tokio::test]
    async fn registration_mode_round_trips_and_rejects_anything_else() {
        let pool = test_pool().await;
        let id = crate::tournament::db::insert_tournament(&pool, "relic-cup", "Relic Cup", 1)
            .await
            .unwrap();

        let mode_of = async |id| {
            crate::tournament::db::get_tournament(&pool, id)
                .await
                .unwrap()
                .unwrap()
                .registration_mode
        };
        assert_eq!(mode_of(id).await, "open", "a new tournament has a public door");

        crate::tournament::db::set_registration_mode(&pool, id, "invite_only")
            .await
            .unwrap();
        assert_eq!(mode_of(id).await, "invite_only");

        let result = crate::tournament::db::set_registration_mode(&pool, id, "members_only").await;
        assert!(result.is_err(), "the check constraint should have refused it");
    }

    #[tokio::test]
    async fn migrating_a_populated_database_defaults_registration_mode_to_open() {
        // A tournament that predates the column was taking public sign-ups, and
        // must keep taking them rather than silently shutting its door.
        let pool = pool_migrated_to_before(REGISTRATION_MODE).await;
        sqlx::query("insert into tournaments (slug, name, created_by) values ('cup', 'Cup', 1)")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        let tournament = crate::tournament::db::get_tournament(&pool, 1).await.unwrap().unwrap();
        assert_eq!(tournament.registration_mode, "open");
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

        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "Old Name")
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
    async fn listing_live_tournaments_excludes_completed_and_canceled_ones() {
        // Boot reconciliation must not repost panels for an event that has
        // already finished — `list_live_tournaments` is the filter that
        // keeps it from trying.
        let pool = test_pool().await;
        let registration_id = crate::tournament::db::insert_tournament(&pool, "registration-slug", "Registration", 1)
            .await
            .unwrap();
        let running_id = crate::tournament::db::insert_tournament(&pool, "running-slug", "Running", 1)
            .await
            .unwrap();
        crate::tournament::db::update_tournament_status(&pool, running_id, "running")
            .await
            .unwrap();
        let completed_id = crate::tournament::db::insert_tournament(&pool, "completed-slug", "Completed", 1)
            .await
            .unwrap();
        crate::tournament::db::update_tournament_status(&pool, completed_id, "completed")
            .await
            .unwrap();
        let canceled_id = crate::tournament::db::insert_tournament(&pool, "canceled-slug", "Canceled", 1)
            .await
            .unwrap();
        crate::tournament::db::update_tournament_status(&pool, canceled_id, "canceled")
            .await
            .unwrap();

        let live: Vec<i64> = crate::tournament::db::list_live_tournaments(&pool)
            .await
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();

        assert!(live.contains(&registration_id));
        assert!(live.contains(&running_id));
        assert!(!live.contains(&completed_id));
        assert!(!live.contains(&canceled_id));
    }

    #[tokio::test]
    async fn upsert_player_binding_leaves_an_existing_display_name_untouched() {
        let pool = test_pool().await;
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "Name")
            .await
            .unwrap();
        // The name argument only ever lands on a fresh insert — an existing
        // row keeps its own, which is what lets `set_player_display_name`
        // stay the one place that changes it for a caller who already has one.
        crate::tournament::db::upsert_player_binding(&pool, 1, 200, "Ignored")
            .await
            .unwrap();

        let player = crate::tournament::db::get_player(&pool, 1).await.unwrap().unwrap();
        assert_eq!(player.aoe4_id, 200);
        assert_eq!(player.display_name, "Name");
    }

    #[tokio::test]
    async fn upsert_player_binding_creates_the_row_when_none_exists() {
        let pool = test_pool().await;
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "Fresh")
            .await
            .unwrap();

        let player = crate::tournament::db::get_player(&pool, 1).await.unwrap().unwrap();
        assert_eq!(player.aoe4_id, 100);
        assert_eq!(player.display_name, "Fresh");
    }

    // `/tournament create`, the admin list gate tests.

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

    // Registration, which is also binding gate tests.

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
        // insert does not survive on its own: neither write survives if the
        // other fails.
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 2, 100, "B")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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

        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
            .await
            .unwrap();
        crate::tournament::db::upsert_player_binding(&pool, 2, 200, "B")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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

    // Check-in gate tests.

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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "A")
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
            crate::tournament::db::upsert_player_binding(&pool, user_id, aoe4_id, "P")
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

    // /tournament reopen-registration gate tests.

    /// A tournament in `status` with three entrants and a check-in round already
    /// run over them: 1 checked in, 2 was marked no-show, 3 withdrew beforehand.
    /// Panel handles are set so a reopen has something to clear.
    async fn setup_reopenable_tournament(pool: &SqlitePool, status: &str) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "checkin").await;
        for (user_id, aoe4_id) in [(1, 100), (2, 200), (3, 300)] {
            crate::tournament::db::upsert_player_binding(pool, user_id, aoe4_id, "P")
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
        crate::tournament::db::set_seed_message_id(pool, tournament.id, Some(888))
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
    async fn reopen_registration_clears_a_suggested_seed_order() {
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "seeding").await;
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
    async fn reopen_registration_keeps_a_seed_order_the_organizers_made() {
        // A curated field exists to be arranged by hand, so the one
        // backward edge in the lifecycle must not silently undo the arranging.
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "seeding").await;
        crate::tournament::db::set_seed_order(&pool, tournament.id, &[2, 1], false)
            .await
            .unwrap();
        crate::tournament::db::set_seed_source(&pool, tournament.id, "manual")
            .await
            .unwrap();
        let tournament = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();

        crate::tournament::checkin::reopen_registration(&pool, &tournament)
            .await
            .unwrap();

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let seed_of = |user_id: i64| entries.iter().find(|e| e.user_id == user_id).unwrap().seed;
        assert_eq!(seed_of(2), Some(1), "the hand-made order should survive the rewind");
        assert_eq!(seed_of(1), Some(2));
        // The check-in round itself is still undone — only the order is spared.
        assert!(entries.iter().all(|e| e.checked_in_at.is_none()));
    }

    #[tokio::test]
    async fn reopen_registration_drops_the_checkin_handle_and_keeps_the_seeding_one() {
        // The check-in panel is deleted by the caller, so leaving its id behind
        // points the next post at a message that no longer exists. The seeding
        // panel is not deleted — it belongs to the event rather than to the
        // check-in round — so dropping its id would strand a live message and post
        // a second one over it.
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "seeding").await;

        crate::tournament::checkin::reopen_registration(&pool, &tournament)
            .await
            .unwrap();

        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.checkin_message_id, None);
        assert_eq!(after.seed_message_id, Some(888));
    }

    #[tokio::test]
    async fn clearing_checkins_and_clearing_seeds_are_separable() {
        // The split is what lets a reopen undo one without the other.
        let pool = test_pool().await;
        let tournament = setup_reopenable_tournament(&pool, "seeding").await;
        crate::tournament::db::set_entry_seed(&pool, tournament.id, 1, Some(1), Some(2))
            .await
            .unwrap();

        crate::tournament::db::clear_checkins(&pool, tournament.id)
            .await
            .unwrap();
        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let seeded = entries.iter().find(|e| e.user_id == 1).unwrap();
        assert!(seeded.checked_in_at.is_none());
        assert_eq!(seeded.seed, Some(1), "clear_checkins must not touch the order");
        assert_eq!(seeded.suggested_seed, Some(2));

        crate::tournament::db::clear_seeds(&pool, tournament.id).await.unwrap();
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "P")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "P")
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
        crate::tournament::db::upsert_player_binding(&pool, 42, 4200, "Me")
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
        // they are #2: entries are never deleted, and the number is a rank
        // by registration time over every row, not a count of the live field.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        for (user_id, aoe4_id) in [(1, 100), (2, 200)] {
            crate::tournament::db::upsert_player_binding(&pool, user_id, aoe4_id, "P")
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
        crate::tournament::db::upsert_player_binding(&pool, 2, 200, "Other")
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

    // Bracket persistence and start.

    /// A seeded, checked-in field of `n`, configured enough to start.
    async fn setup_startable(pool: &SqlitePool, n: i64) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "seeding").await;
        for user_id in 1..=n {
            crate::tournament::db::upsert_player_binding(pool, user_id, user_id * 100, "P")
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
        assert_eq!(
            sets.len(),
            8,
            "an 8-bracket is 4 + 2 + 1 sets, plus the 3rd place match"
        );

        // Two sets have nowhere to advance a winner to: the final, and the 3rd
        // place match (whose winner is 3rd, not further advancement).
        let rootless: Vec<_> = sets.iter().filter(|s| s.winner_advances_to_set_id.is_none()).collect();
        assert_eq!(
            rootless.len(),
            2,
            "the final and the 3rd place match are both winner-rootless"
        );

        // Both semifinal sets feed a loser to the same set — the 3rd place
        // match — which is one of the two rootless sets above.
        let third_place_feeders: Vec<_> = sets.iter().filter(|s| s.loser_advances_to_set_id.is_some()).collect();
        assert_eq!(
            third_place_feeders.len(),
            2,
            "both semifinal sets feed a loser somewhere"
        );
        let third_place_id = third_place_feeders[0].loser_advances_to_set_id.unwrap();
        assert!(
            third_place_feeders
                .iter()
                .all(|s| s.loser_advances_to_set_id == Some(third_place_id)),
            "both semifinal losers feed the same 3rd place set"
        );
        assert!(
            rootless.iter().any(|s| s.id == third_place_id),
            "the 3rd place set is winner-rootless too"
        );

        // And every link points at a set that exists, in a slot that is 1 or 2.
        let ids: Vec<i64> = sets.iter().map(|s| s.id).collect();
        for set in sets.iter().filter(|s| s.winner_advances_to_set_id.is_some()) {
            assert!(ids.contains(&set.winner_advances_to_set_id.unwrap()));
            assert!(matches!(set.winner_advances_to_slot, Some(1) | Some(2)));
        }
        for set in sets.iter().filter(|s| s.loser_advances_to_set_id.is_some()) {
            assert!(ids.contains(&set.loser_advances_to_set_id.unwrap()));
            assert!(matches!(set.loser_advances_to_slot, Some(1) | Some(2)));
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
        assert_eq!(
            sets[3].status, "pending",
            "the 3rd place match waits on both semifinal losers, neither of which exists yet"
        );
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

        // And seed 1 is already sitting in the final — still the last set here,
        // since a bye semifinal (exactly this case) means no 3rd place match
        // exists to sit after it.
        let final_set = sets.last().unwrap();
        assert_eq!(final_set.slot1_user_id, Some(1));
        assert_eq!(final_set.status, "pending", "still waiting on the other half");
    }

    // Set completion and advancement.
    //
    // These drive `completion`'s pure half and `db::complete_set_and_advance`
    // rather than `completion::finish`, which needs a Discord `CacheHttp` and so
    // cannot run here. Everything the database is responsible for is below; the
    // thread archive and the next thread are covered only by hand.

    /// A started 4-player bracket: two round-one sets, both feeding the final.
    /// Seed order is 1..4, so set 1 is users 1 v 4 and set 2 is users 2 v 3.
    async fn setup_running_bracket(pool: &SqlitePool) -> crate::tournament::db::Tournament {
        let tournament = setup_startable(pool, 4).await;
        crate::tournament::start::start(pool, &tournament).await.unwrap();
        crate::tournament::db::get_tournament(pool, tournament.id)
            .await
            .unwrap()
            .unwrap()
    }

    /// One completed game per entry in `winners`, numbered on from whatever the
    /// set already holds — `unique (set_id, game_number)` means a second call
    /// cannot restart at 1.
    async fn report_games(pool: &SqlitePool, set_id: i64, winners: &[i64]) {
        let played = crate::tournament::db::list_games_for_set(pool, set_id)
            .await
            .unwrap()
            .len();
        for (index, winner) in winners.iter().enumerate() {
            crate::tournament::db::insert_game(
                pool,
                crate::tournament::db::NewGame {
                    set_id,
                    game_number: i64::try_from(played + index).unwrap() + 1,
                    map: None,
                    slot1_civ: None,
                    slot2_civ: None,
                    winner_user_id: Some(*winner),
                    status: "completed".to_string(),
                    source: "manual".to_string(),
                    reported_by: Some(99),
                    reported_at: Some(chrono::Utc::now()),
                },
            )
            .await
            .unwrap();
        }
    }

    /// What `completion::finish` does minus the Discord half: tally, decide, and
    /// run the transaction. `None` when the games have not decided the set.
    async fn decide_and_complete(
        pool: &SqlitePool,
        tournament_id: i64,
        set_id: i64,
    ) -> Option<crate::tournament::db::Advanced> {
        let set = crate::tournament::db::get_set(pool, set_id).await.unwrap().unwrap();
        let round = crate::tournament::db::get_round(pool, set.round_id)
            .await
            .unwrap()
            .unwrap();
        let games = crate::tournament::db::list_games_for_set(pool, set.id).await.unwrap();
        let (slot1, slot2) = (set.slot1_user_id.unwrap(), set.slot2_user_id.unwrap());
        let tally = crate::tournament::completion::tally(&games, slot1, slot2);
        let winning_slot = crate::tournament::completion::decide(&tally, round.best_of)?;
        let (winner_user_id, loser_user_id) = match winning_slot {
            crate::tournament::bracket::Slot::One => (slot1, slot2),
            crate::tournament::bracket::Slot::Two => (slot2, slot1),
        };
        Some(
            crate::tournament::db::complete_set_and_advance(
                pool,
                crate::tournament::db::SetResult {
                    set_id: set.id,
                    tournament_id,
                    slot1_wins: tally.slot1_wins,
                    slot2_wins: tally.slot2_wins,
                    winner_user_id,
                    loser_user_id,
                    status: "completed",
                },
            )
            .await
            .unwrap(),
        )
    }

    async fn set_ids(pool: &SqlitePool, tournament_id: i64) -> Vec<i64> {
        crate::tournament::db::list_sets_for_tournament(pool, tournament_id)
            .await
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect()
    }

    async fn status_of(pool: &SqlitePool, tournament_id: i64, user_id: i64) -> String {
        crate::tournament::db::get_entry(pool, tournament_id, user_id)
            .await
            .unwrap()
            .unwrap()
            .status
    }

    #[tokio::test]
    async fn a_set_reaching_a_majority_completes_and_places_its_winner() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1, 1]).await; // user 1 takes it 2-0
        let advanced = decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        assert!(advanced.completed);

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.status, "completed");
        assert_eq!(set.winner_user_id, Some(1));
        assert_eq!((set.slot1_wins, set.slot2_wins), (2, 0));
        assert!(set.completed_at.is_some());

        assert_eq!(status_of(&pool, tournament.id, 4).await, "eliminated");
        assert_eq!(
            status_of(&pool, tournament.id, 1).await,
            "active",
            "the winner plays on"
        );

        let final_set = crate::tournament::db::get_set(&pool, ids[2]).await.unwrap().unwrap();
        assert_eq!(final_set.slot1_user_id, Some(1), "position 1 feeds slot 1");
        assert_eq!(final_set.status, "pending", "the other half has not finished");
        assert!(!advanced.target_became_ready);
    }

    #[tokio::test]
    async fn the_next_set_opens_only_once_both_halves_have_finished() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1, 1]).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        // The even position feeds slot 2, which is a separate statement.
        report_games(&pool, ids[1], &[3, 3]).await;
        let advanced = decide_and_complete(&pool, tournament.id, ids[1]).await.unwrap();

        assert!(advanced.target_became_ready, "the final is now playable");
        let final_set = crate::tournament::db::get_set(&pool, ids[2]).await.unwrap().unwrap();
        assert_eq!(final_set.slot1_user_id, Some(1));
        assert_eq!(final_set.slot2_user_id, Some(3), "position 2 feeds slot 2");
        assert_eq!(final_set.status, "ready");
        assert!(!advanced.tournament_completed, "the final has not been played");
    }

    #[tokio::test]
    async fn completing_the_final_ends_the_tournament() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1, 1]).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        report_games(&pool, ids[1], &[3, 3]).await;
        decide_and_complete(&pool, tournament.id, ids[1]).await.unwrap();

        report_games(&pool, ids[2], &[3, 1, 3]).await; // user 3 wins the final 1-2
        let advanced = decide_and_complete(&pool, tournament.id, ids[2]).await.unwrap();

        assert!(advanced.tournament_completed);
        assert!(!advanced.target_became_ready, "a final advances nowhere");
        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, "completed");
        assert!(after.completed_at.is_some());
        assert_eq!(
            status_of(&pool, tournament.id, 3).await,
            "active",
            "the champion is not eliminated"
        );
    }

    #[tokio::test]
    async fn both_semifinal_losers_fill_the_3rd_place_match_and_it_settles_independently_of_the_final() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        assert_eq!(ids.len(), 4, "semifinal x2, final, 3rd place");

        report_games(&pool, ids[0], &[1, 1]).await; // user 1 beats user 4
        let first = decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        assert!(!first.is_third_place);

        let third_place = crate::tournament::db::get_set(&pool, ids[3]).await.unwrap().unwrap();
        assert_eq!(
            third_place.slot1_user_id,
            Some(4),
            "the first semifinal's loser seats slot 1"
        );
        assert_eq!(
            third_place.status, "pending",
            "still waiting on the other semifinal's loser"
        );

        report_games(&pool, ids[1], &[3, 3]).await; // user 3 beats user 2
        let second = decide_and_complete(&pool, tournament.id, ids[1]).await.unwrap();
        assert!(!second.is_third_place);

        let third_place = crate::tournament::db::get_set(&pool, ids[3]).await.unwrap().unwrap();
        assert_eq!(third_place.slot1_user_id, Some(4));
        assert_eq!(
            third_place.slot2_user_id,
            Some(2),
            "the second semifinal's loser seats slot 2"
        );
        assert_eq!(third_place.status, "ready");

        // The final decides the tournament, regardless of whether the 3rd place
        // match has been played yet.
        report_games(&pool, ids[2], &[3, 1, 3]).await;
        let final_advanced = decide_and_complete(&pool, tournament.id, ids[2]).await.unwrap();
        assert!(final_advanced.tournament_completed);
        assert!(!final_advanced.is_third_place);

        // The 3rd place match settles afterward without re-completing the
        // tournament.
        report_games(&pool, ids[3], &[4, 4]).await; // user 4 takes 3rd
        let third_place_advanced = decide_and_complete(&pool, tournament.id, ids[3]).await.unwrap();
        assert!(third_place_advanced.completed);
        assert!(third_place_advanced.is_third_place);
        assert!(
            !third_place_advanced.tournament_completed,
            "the tournament was already completed by the final"
        );
        assert!(
            !third_place_advanced.target_became_ready,
            "a 3rd place match advances nowhere"
        );

        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, "completed", "still completed, not re-stamped");

        let third_place = crate::tournament::db::get_set(&pool, ids[3]).await.unwrap().unwrap();
        assert_eq!(third_place.status, "completed");
        assert_eq!(third_place.winner_user_id, Some(4));
    }

    #[tokio::test]
    async fn the_3rd_place_match_can_settle_before_the_final_without_ending_the_tournament() {
        // Order-independence: whichever of the two rootless sets is decided
        // first, only the final flips the tournament's status.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1, 1]).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        report_games(&pool, ids[1], &[3, 3]).await;
        decide_and_complete(&pool, tournament.id, ids[1]).await.unwrap();

        report_games(&pool, ids[3], &[4, 4]).await;
        let third_place_advanced = decide_and_complete(&pool, tournament.id, ids[3]).await.unwrap();
        assert!(third_place_advanced.is_third_place);
        assert!(
            !third_place_advanced.tournament_completed,
            "the final hasn't been played yet"
        );

        let before = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.status, "running");

        report_games(&pool, ids[2], &[3, 1, 3]).await;
        let final_advanced = decide_and_complete(&pool, tournament.id, ids[2]).await.unwrap();
        assert!(final_advanced.tournament_completed);
        assert!(!final_advanced.is_third_place);

        let after = crate::tournament::db::get_tournament(&pool, tournament.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, "completed");
    }

    #[tokio::test]
    async fn a_second_completion_writes_nothing_even_if_the_games_now_say_otherwise() {
        // The set row is the only thing serialising two presses of the same
        // button, so a stale caller must not advance a second winner.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1, 1]).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();

        // Rewrite history underneath it: void both games and give the other side
        // two of its own, so the tally now decides for user 4. The guard has to be
        // the set row rather than the score, or this would advance a second winner.
        for game in crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap() {
            crate::tournament::db::update_game_result(&pool, game.id, None, "void", None, None, None)
                .await
                .unwrap();
        }
        report_games(&pool, ids[0], &[4, 4]).await;
        let advanced = decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        assert!(!advanced.completed, "the set was already decided");

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.winner_user_id, Some(1), "the first result stands");
        assert_eq!((set.slot1_wins, set.slot2_wins), (2, 0));
        let final_set = crate::tournament::db::get_set(&pool, ids[2]).await.unwrap().unwrap();
        assert_eq!(final_set.slot1_user_id, Some(1), "nobody was advanced twice");
        assert_eq!(status_of(&pool, tournament.id, 1).await, "active");
    }

    #[tokio::test]
    async fn a_set_below_the_majority_is_left_entirely_alone() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1]).await; // 1-0 in a Bo3
        assert!(
            decide_and_complete(&pool, tournament.id, ids[0]).await.is_none(),
            "1-0 in a Bo3 decides nothing"
        );

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.status, "ready");
        assert_eq!(set.winner_user_id, None);
        assert_eq!(status_of(&pool, tournament.id, 4).await, "active");
        let final_set = crate::tournament::db::get_set(&pool, ids[2]).await.unwrap().unwrap();
        assert_eq!(final_set.slot1_user_id, None);
    }

    #[tokio::test]
    async fn an_eliminated_entrant_keeps_their_place_in_the_seeding() {
        // Completing a set is the only thing that writes `eliminated`, and a
        // seeding panel that dropped them would end up listing only the champion.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_games(&pool, ids[0], &[1, 1]).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();

        let entries = crate::tournament::db::list_entries_for_tournament(&pool, tournament.id)
            .await
            .unwrap();
        let field = crate::tournament::seeding::display_order(&entries);
        assert_eq!(field.len(), 4, "all four still stand in the seeding");
        assert!(field.iter().any(|e| e.user_id == 4 && e.status == "eliminated"));
    }

    // Result import.
    //
    // `import::apply` calls `completion::finish` exactly like the manual
    // paths do, which needs a Discord `CacheHttp` — but never fails through
    // that: every Discord call downstream is best-effort and logs its own
    // error rather than propagating one (`set_thread::close`,
    // `bracket_view::reconcile`), so a fake token is enough to exercise the
    // whole path and verify the database it leaves behind.

    fn fake_http() -> serenity::all::Http {
        serenity::all::Http::new("faketoken")
    }

    /// A wide-open throttle: these tests exercise `import::apply`/`sync` in
    /// isolation, one call each, so there is nothing for a throttle window to
    /// coalesce.
    fn fake_throttle() -> crate::tournament::throttle::EditThrottle {
        crate::tournament::throttle::EditThrottle::new(std::time::Duration::ZERO)
    }

    fn draft_seat(claimed: bool) -> crate::drafttool::DraftSeat {
        crate::drafttool::DraftSeat { claimed }
    }

    fn draft_game(number: i64, map: Option<&str>, winner_slot: Option<i64>) -> crate::drafttool::DraftGame {
        crate::drafttool::DraftGame {
            number,
            map: map.map(str::to_string),
            civ_by_slot: crate::drafttool::CivBySlot {
                slot1: None,
                slot2: None,
            },
            winner_slot,
        }
    }

    fn draft_state(
        status: &str,
        finished: bool,
        best_of: i64,
        score: (i64, i64),
        games: Vec<crate::drafttool::DraftGame>,
    ) -> crate::drafttool::DraftState {
        crate::drafttool::DraftState {
            status: status.to_string(),
            finished,
            seats: vec![draft_seat(true), draft_seat(true)],
            best_of,
            score: crate::drafttool::SlotValues {
                slot1: score.0,
                slot2: score.1,
            },
            games,
        }
    }

    /// Points `set_id` at `external_id` and reads the set back, so the
    /// snapshot passed to `apply` agrees with what a fresh read would show —
    /// exactly what `sync` would hand it in production.
    async fn set_pointer(pool: &SqlitePool, set_id: i64, external_id: &str) -> crate::tournament::db::TournamentSet {
        crate::tournament::db::set_draft_pointer(pool, set_id, external_id)
            .await
            .unwrap();
        crate::tournament::db::get_set(pool, set_id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_game_is_mapped_to_the_higher_seed_and_a_lone_win_does_not_decide_a_bo3() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = set_pointer(&pool, ids[0], "draft-1").await;
        assert_eq!(set.slot1_user_id, Some(1), "seed 1 is the higher seed in this bracket");

        let state = draft_state(
            "running",
            false,
            3,
            (1, 0),
            vec![draft_game(1, Some("prairie"), Some(1))],
        );
        let outcome = crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        let games = crate::tournament::db::list_games_for_set(&pool, set.id).await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].winner_user_id, Some(1), "slot 1 -> the higher seed's user id");
        assert_eq!(games[0].source, "draft_import");
        assert!(
            matches!(
                outcome,
                crate::tournament::import::SyncOutcome::Progress {
                    outcome: crate::tournament::completion::CompleteOutcome::StillPlaying { .. },
                    score_mismatch: false,
                }
            ),
            "{outcome:?}"
        );
        let reloaded = crate::tournament::db::get_set(&pool, set.id).await.unwrap().unwrap();
        assert_eq!(reloaded.status, "ready", "1-0 in a Bo3 is not a result");
    }

    #[tokio::test]
    async fn a_clinching_score_completes_the_set_even_though_status_still_reads_running() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = set_pointer(&pool, ids[0], "draft-1").await;

        let games = vec![
            draft_game(1, Some("prairie"), Some(1)),
            draft_game(2, Some("dry-arabia"), Some(1)),
        ];
        let state = draft_state("running", false, 3, (2, 0), games);
        let outcome = crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        let crate::tournament::import::SyncOutcome::Progress {
            outcome: crate::tournament::completion::CompleteOutcome::Completed { .. },
            score_mismatch: false,
        } = outcome
        else {
            panic!("expected a completed set, got {outcome:?}");
        };
        let reloaded = crate::tournament::db::get_set(&pool, set.id).await.unwrap().unwrap();
        assert_eq!(reloaded.status, "completed");
        assert_eq!(reloaded.winner_user_id, Some(1));
        assert_eq!(status_of(&pool, tournament.id, 4).await, "eliminated");
    }

    #[tokio::test]
    async fn a_finished_report_with_no_majority_reached_stays_open() {
        // What a nonzero head start would look like if the bot modeled one:
        // `finished: true` with only one win recorded in a Bo3, which needs
        // two. Since head start is assumed zero and not acted on, this stays
        // `StillPlaying` rather than settling — the known gap the doc records.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = set_pointer(&pool, ids[0], "draft-1").await;

        let state = draft_state(
            "running",
            true,
            3,
            (1, 0),
            vec![draft_game(1, Some("prairie"), Some(1))],
        );
        let outcome = crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        assert!(
            matches!(
                outcome,
                crate::tournament::import::SyncOutcome::Progress {
                    outcome: crate::tournament::completion::CompleteOutcome::StillPlaying { .. },
                    ..
                }
            ),
            "{outcome:?}"
        );
        let reloaded = crate::tournament::db::get_set(&pool, set.id).await.unwrap().unwrap();
        assert_eq!(
            reloaded.status, "ready",
            "the set is left exactly as undecided as it was"
        );
    }

    #[tokio::test]
    async fn a_completed_set_attempts_to_edit_its_panel_and_announcement() {
        // The DB half is what a test can actually verify; the edits themselves
        // fail against fake_http's bogus token and log rather than propagate,
        // exactly like the existing thread-archive call already does. The
        // point of this test is that `close`'s two new edit attempts execute
        // at all — a set with no panel or announcement wired up would skip
        // them silently, which is every other test in this suite.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        crate::tournament::db::set_tournament_channels(
            &pool,
            tournament.id,
            crate::tournament::db::TournamentChannels {
                category_id: None,
                announce_channel_id: 900,
                register_channel_id: 901,
                bracket_channel_id: 902,
                matches_channel_id: 903,
                draft_channel_id: 904,
            },
        )
        .await
        .unwrap();
        let tournament = reload(&pool, tournament.id).await;
        let ids = set_ids(&pool, tournament.id).await;
        crate::tournament::db::set_thread(&pool, ids[0], 1000).await.unwrap();
        crate::tournament::db::set_panel_message(&pool, ids[0], 1001)
            .await
            .unwrap();
        crate::tournament::db::set_draft_announce_message(&pool, ids[0], 1002)
            .await
            .unwrap();
        let set = set_pointer(&pool, ids[0], "draft-1").await;

        let games = vec![
            draft_game(1, Some("prairie"), Some(1)),
            draft_game(2, Some("dry-arabia"), Some(1)),
        ];
        let state = draft_state("running", false, 3, (2, 0), games);
        let outcome = crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        assert!(
            matches!(
                outcome,
                crate::tournament::import::SyncOutcome::Progress {
                    outcome: crate::tournament::completion::CompleteOutcome::Completed { .. },
                    ..
                }
            ),
            "{outcome:?}"
        );
        let reloaded = crate::tournament::db::get_set(&pool, set.id).await.unwrap().unwrap();
        assert_eq!(
            reloaded.status, "completed",
            "the DB write is unaffected by the Discord calls failing"
        );
    }

    #[tokio::test]
    async fn reimport_overwrites_draft_import_rows_and_preserves_manual_ones() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = set_pointer(&pool, ids[0], "draft-1").await;

        // An organizer's own correction of game 1, naming the lower seed.
        crate::tournament::db::record_manual_game(
            &pool,
            crate::tournament::db::ManualGame {
                set_id: set.id,
                game_number: 1,
                winner_user_id: 4,
                reported_by: 99,
                map: None,
                slot1_civ: None,
                slot2_civ: None,
            },
        )
        .await
        .unwrap();

        // The draft's own record of the same game disagrees with it.
        let state = draft_state(
            "running",
            false,
            3,
            (1, 0),
            vec![draft_game(1, Some("prairie"), Some(1))],
        );
        crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        let game = crate::tournament::db::get_game(&pool, set.id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(game.source, "manual", "the organizer's correction survives the sync");
        assert_eq!(game.winner_user_id, Some(4));
    }

    #[tokio::test]
    async fn sync_on_an_already_complete_set_reports_so_without_touching_it() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        report_games(&pool, ids[0], &[1, 1]).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        let completed = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();

        // No pointer, no network call — `sync` bails before either.
        let outcome = crate::tournament::import::sync(fake_http(), &pool, &fake_throttle(), &tournament, &completed)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            crate::tournament::import::SyncOutcome::AlreadyComplete
        ));
    }

    #[tokio::test]
    async fn sync_on_a_set_with_no_draft_pointer_reports_so() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.draft_external_id, None, "a fresh set has no room yet");

        let outcome = crate::tournament::import::sync(fake_http(), &pool, &fake_throttle(), &tournament, &set)
            .await
            .unwrap();
        assert!(matches!(outcome, crate::tournament::import::SyncOutcome::NoPointer));
    }

    #[tokio::test]
    async fn a_redraft_landing_before_the_write_supersedes_the_stale_fetch() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = set_pointer(&pool, ids[0], "draft-1").await;
        // A redraft repoints the set while our fetch of "draft-1" is in flight.
        crate::tournament::db::set_draft_pointer(&pool, set.id, "draft-2")
            .await
            .unwrap();

        let state = draft_state(
            "running",
            false,
            3,
            (1, 0),
            vec![draft_game(1, Some("prairie"), Some(1))],
        );
        let outcome = crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, crate::tournament::import::SyncOutcome::Superseded));
        let games = crate::tournament::db::list_games_for_set(&pool, set.id).await.unwrap();
        assert!(games.is_empty(), "a superseded fetch writes nothing");
    }

    #[tokio::test]
    async fn a_lobby_with_no_seat_claimed_writes_nothing() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = set_pointer(&pool, ids[0], "draft-1").await;

        let mut state = draft_state("lobby", false, 3, (0, 0), vec![draft_game(1, Some("prairie"), Some(1))]);
        state.seats = vec![draft_seat(false), draft_seat(false)];
        let outcome = crate::tournament::import::apply(
            fake_http(),
            &pool,
            &fake_throttle(),
            &tournament,
            &set,
            "draft-1",
            state,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, crate::tournament::import::SyncOutcome::NotSeated));
        let games = crate::tournament::db::list_games_for_set(&pool, set.id).await.unwrap();
        assert!(games.is_empty(), "nothing is imported before both seats are taken");
    }

    // Manual result reporting.
    //
    // The command bodies need a Discord context, so these drive `report`'s own
    // engine — everything the database is responsible for, which is all of it
    // except the reply.

    /// Reports `winner` as having taken game `game` of `set_id`, from user 99.
    async fn report_one(
        pool: &SqlitePool,
        set_id: i64,
        game: i64,
        winner: i64,
    ) -> crate::tournament::report::ReportOutcome {
        let set = crate::tournament::db::get_set(pool, set_id).await.unwrap().unwrap();
        crate::tournament::report::report_game(
            pool,
            &set,
            crate::tournament::report::Report {
                game_number: game,
                winner_user_id: winner,
                winner_name: "P".to_string(),
                reported_by: 99,
                map: None,
                slot1_civ: None,
                slot2_civ: None,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_reported_game_is_stored_as_the_organizers_own_record() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        let outcome = report_one(&pool, ids[0], 1, 1).await;
        assert!(outcome.recorded(), "{outcome:?}");

        let games = crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_number, 1);
        assert_eq!(games[0].winner_user_id, Some(1));
        assert_eq!(games[0].status, "completed");
        assert_eq!(games[0].source, "manual", "a later sync must not overwrite this");
        assert_eq!(games[0].reported_by, Some(99));
        assert!(games[0].reported_at.is_some());

        // One game of a Bo3 decides nothing, so the set is still playable.
        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.status, "ready");
        assert_eq!(set.winner_user_id, None);
    }

    #[tokio::test]
    async fn re_reporting_a_game_corrects_it_rather_than_adding_a_second_row() {
        // The whole reason the write is an upsert: `unique (set_id, game_number)`
        // would otherwise reject the correction outright.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_one(&pool, ids[0], 1, 1).await;
        report_one(&pool, ids[0], 1, 4).await;

        let games = crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap();
        assert_eq!(games.len(), 1, "still one game 1");
        assert_eq!(games[0].winner_user_id, Some(4), "the correction stands");
    }

    #[tokio::test]
    async fn reporting_up_to_the_majority_hands_a_decided_set_to_the_completion_path() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_one(&pool, ids[0], 1, 1).await;
        let outcome = report_one(&pool, ids[0], 2, 1).await;
        let crate::tournament::report::ReportOutcome::Recorded { tally, .. } = outcome else {
            panic!("expected a recorded game");
        };
        // `report` records and reports the score; deciding is `completion`'s job,
        // so the set is untouched until the command calls it.
        assert_eq!((tally.slot1_wins, tally.slot2_wins), (2, 0));
        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.status, "ready");

        let advanced = decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();
        assert!(advanced.completed);
        assert_eq!(status_of(&pool, tournament.id, 4).await, "eliminated");
    }

    #[tokio::test]
    async fn a_finished_set_refuses_a_report_and_keeps_its_games() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_one(&pool, ids[0], 1, 1).await;
        report_one(&pool, ids[0], 2, 1).await;
        decide_and_complete(&pool, tournament.id, ids[0]).await.unwrap();

        let outcome = report_one(&pool, ids[0], 3, 4).await;
        assert_eq!(outcome, crate::tournament::report::ReportOutcome::AlreadyComplete);

        let games = crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap();
        assert_eq!(games.len(), 2, "the refused report wrote nothing");
        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.winner_user_id, Some(1));
    }

    #[tokio::test]
    async fn a_report_is_refused_before_it_writes_when_the_arguments_are_wrong() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        // Bo3, so games 0 and 4 do not exist; user 2 plays in the other set.
        let bad_number = crate::tournament::report::ReportOutcome::BadGameNumber { best_of: 3 };
        assert_eq!(report_one(&pool, ids[0], 0, 1).await, bad_number);
        assert_eq!(report_one(&pool, ids[0], 4, 1).await, bad_number);
        assert_eq!(
            report_one(&pool, ids[0], 1, 2).await,
            crate::tournament::report::ReportOutcome::NotInSet
        );
        // And the final, whose slots are still empty, has nothing to report.
        assert_eq!(
            report_one(&pool, ids[2], 1, 1).await,
            crate::tournament::report::ReportOutcome::NotPlayable
        );

        for set_id in [ids[0], ids[2]] {
            let games = crate::tournament::db::list_games_for_set(&pool, set_id).await.unwrap();
            assert!(games.is_empty(), "a refused report must write nothing");
        }
    }

    #[tokio::test]
    async fn an_organizers_own_record_survives_a_redraft() {
        // Regenerating a draft discards the imported record of a game and keeps a
        // correction someone made by hand. Untestable until manual rows existed.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_one(&pool, ids[0], 1, 1).await;
        report_games(&pool, ids[0], &[4]).await; // an import-shaped row
        crate::tournament::db::update_game_result(
            &pool,
            crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap()[1].id,
            Some(4),
            "completed",
            None,
            None,
            None,
        )
        .await
        .unwrap();
        sqlx::query("update tournament_games set source = 'draft_import' where game_number = 2")
            .execute(&pool)
            .await
            .unwrap();

        crate::tournament::db::void_games_for_set(&pool, ids[0]).await.unwrap();

        let games = crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap();
        let manual = games.iter().find(|g| g.game_number == 1).unwrap();
        let imported = games.iter().find(|g| g.game_number == 2).unwrap();
        assert_eq!(manual.status, "completed", "a hand-made record survives");
        assert_eq!(imported.status, "void", "the imported one does not");
    }

    #[tokio::test]
    async fn a_set_is_found_by_the_thread_it_is_played_in() {
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        crate::tournament::db::set_thread(&pool, ids[1], 4242).await.unwrap();

        let found = crate::tournament::db::get_set_by_thread(&pool, 4242).await.unwrap();
        assert_eq!(found.map(|s| s.id), Some(ids[1]));
        assert!(
            crate::tournament::db::get_set_by_thread(&pool, 9999)
                .await
                .unwrap()
                .is_none(),
            "an unknown thread is not a set"
        );
    }

    #[tokio::test]
    async fn an_awarded_set_advances_its_winner_like_any_other() {
        // The no-show case: nothing was played, and the bracket still has to move.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        let advanced = crate::tournament::db::complete_set_and_advance(
            &pool,
            crate::tournament::db::SetResult {
                set_id: set.id,
                tournament_id: tournament.id,
                slot1_wins: 0,
                slot2_wins: 0,
                winner_user_id: 1,
                loser_user_id: 4,
                status: "walkover",
            },
        )
        .await
        .unwrap();
        assert!(advanced.completed);

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.status, "walkover", "the record says it was not played out");
        assert_eq!(set.winner_user_id, Some(1));
        assert!(set.completed_at.is_some());
        assert_eq!(status_of(&pool, tournament.id, 4).await, "eliminated");

        let final_set = crate::tournament::db::get_set(&pool, ids[2]).await.unwrap().unwrap();
        assert_eq!(final_set.slot1_user_id, Some(1), "the awarded winner advances");
    }

    #[tokio::test]
    async fn an_awarded_set_is_terminal_for_every_other_route() {
        // A walkover has to shut the same doors a played result does, or a late
        // report would advance a second winner out of the same set.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        crate::tournament::db::complete_set_and_advance(
            &pool,
            crate::tournament::db::SetResult {
                set_id: set.id,
                tournament_id: tournament.id,
                slot1_wins: 0,
                slot2_wins: 0,
                winner_user_id: 1,
                loser_user_id: 4,
                status: "walkover",
            },
        )
        .await
        .unwrap();

        // A report is refused...
        assert_eq!(
            report_one(&pool, ids[0], 1, 4).await,
            crate::tournament::report::ReportOutcome::AlreadyComplete
        );
        // ...and a second settlement writes nothing.
        let again = crate::tournament::db::complete_set_and_advance(
            &pool,
            crate::tournament::db::SetResult {
                set_id: set.id,
                tournament_id: tournament.id,
                slot1_wins: 0,
                slot2_wins: 0,
                winner_user_id: 4,
                loser_user_id: 1,
                status: "completed",
            },
        )
        .await
        .unwrap();
        assert!(!again.completed, "a decided set stays decided");
        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(set.winner_user_id, Some(1));
    }

    #[tokio::test]
    async fn awarding_a_half_played_set_keeps_the_games_that_were_played() {
        // Abandoned at 1-0 the other way: the score says what happened on the
        // field, the status says who was given the set.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;

        report_one(&pool, ids[0], 1, 4).await;
        let games = crate::tournament::db::list_games_for_set(&pool, ids[0]).await.unwrap();
        let tally = crate::tournament::completion::tally(&games, 1, 4);
        assert_eq!((tally.slot1_wins, tally.slot2_wins), (0, 1));

        crate::tournament::db::complete_set_and_advance(
            &pool,
            crate::tournament::db::SetResult {
                set_id: ids[0],
                tournament_id: tournament.id,
                slot1_wins: tally.slot1_wins,
                slot2_wins: tally.slot2_wins,
                winner_user_id: 1,
                loser_user_id: 4,
                status: "walkover",
            },
        )
        .await
        .unwrap();

        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!((set.slot1_wins, set.slot2_wins), (0, 1), "the played game stands");
        assert_eq!(set.winner_user_id, Some(1), "and the set still goes the other way");
        assert_eq!(
            crate::tournament::db::list_games_for_set(&pool, ids[0])
                .await
                .unwrap()
                .len(),
            1
        );
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
        assert_eq!(rounds.len(), 3, "a 4-bracket is two rounds plus the 3rd place match");
        // Ordered by ordinal, so outermost first — the same order the presets are
        // resolved in, which is the mapping that could silently come out reversed.
        assert_eq!(rounds[0].draft_preset_id.as_deref(), Some("preset"));
        assert_eq!(
            rounds[1].draft_preset_id.as_deref(),
            Some("final-preset"),
            "an assignment at depth 1 covers the final and nothing else"
        );
        // The 3rd place round resolves no preset of its own — it borrows the
        // semifinal's, which is exactly the guard against a silently-NULL
        // draft_preset_id (and therefore no draft room) on this set.
        assert_eq!(
            rounds[2].draft_preset_id.as_deref(),
            Some("preset"),
            "the 3rd place match reuses the semifinal's preset, not the final's"
        );
    }

    #[tokio::test]
    async fn announcing_a_set_records_the_message_id_and_a_redraft_clears_it() {
        // The announcement handle is stored
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
    async fn the_panel_handle_survives_a_redrafts_pointer_change() {
        // Unlike the announcement handle, the panel is struck and replaced
        // explicitly by `redraft::run`, not implicitly by `set_draft_pointer` —
        // so the pointer change alone must leave it untouched.
        let pool = test_pool().await;
        let set_id = setup_set(&pool).await;

        crate::tournament::db::set_draft_pointer(&pool, set_id, "65f1a0")
            .await
            .unwrap();
        crate::tournament::db::set_panel_message(&pool, set_id, 4242)
            .await
            .unwrap();

        crate::tournament::db::set_draft_pointer(&pool, set_id, "65f1b1")
            .await
            .unwrap();
        let set = crate::tournament::db::get_set(&pool, set_id).await.unwrap().unwrap();
        assert_eq!(
            set.panel_message_id,
            Some(4242),
            "a redraft strikes and replaces the panel itself; the pointer write must not clear it"
        );
    }

    #[tokio::test]
    async fn a_redraft_is_refused_on_a_completed_set_but_goes_through_for_a_player_or_an_admin() {
        // The guard order and the two allowed callers, exercised against real
        // rows rather than the fixtures `redraft::tests` builds by hand.
        let pool = test_pool().await;
        let tournament = setup_running_bracket(&pool).await;
        let ids = set_ids(&pool, tournament.id).await;
        let set = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        let round = crate::tournament::db::get_round(&pool, set.round_id)
            .await
            .unwrap()
            .unwrap();
        let has_preset = round.draft_preset_id.is_some();

        let (slot1, slot2) = (set.slot1_user_id.unwrap(), set.slot2_user_id.unwrap());
        assert_eq!(crate::tournament::redraft::refuse(&set, has_preset, true, false), None);
        assert_eq!(
            crate::tournament::redraft::refuse(&set, has_preset, false, false),
            Some(crate::tournament::redraft::RedraftOutcome::NotYours),
            "neither slot's occupant nor an admin"
        );

        crate::tournament::db::complete_set_and_advance(
            &pool,
            crate::tournament::db::SetResult {
                set_id: set.id,
                tournament_id: tournament.id,
                slot1_wins: 2,
                slot2_wins: 0,
                winner_user_id: slot1,
                loser_user_id: slot2,
                status: "completed",
            },
        )
        .await
        .unwrap();
        let completed = crate::tournament::db::get_set(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(
            crate::tournament::redraft::refuse(&completed, has_preset, true, false),
            Some(crate::tournament::redraft::RedraftOutcome::AlreadyComplete)
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
        // Withdrawal stays open through seeding, which is how a gap happens.
        crate::tournament::db::update_entry_status(&pool, tournament.id, 2, "withdrawn")
            .await
            .unwrap();

        let outcome = crate::tournament::start::start(&pool, &tournament).await.unwrap();
        assert_eq!(outcome, crate::tournament::start::StartOutcome::SeedsNotContiguous);
    }

    // Bracket message reconciliation.

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

    // Entrant cap gate tests.

    async fn register_nth(
        pool: &SqlitePool,
        tournament: &crate::tournament::db::Tournament,
        user_id: i64,
    ) -> crate::tournament::registration::RegisterOutcome {
        crate::tournament::db::upsert_player_binding(pool, user_id, user_id * 100, "P")
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
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "Player")
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
        crate::tournament::db::upsert_player_binding(&pool, 2, 100, "Someone Else")
            .await
            .unwrap();
        let claimed = crate::tournament::db::get_player_by_aoe4_id(&pool, 100).await.unwrap();
        assert_eq!(claimed.map(|p| p.user_id), Some(2));
    }

    #[tokio::test]
    async fn unbind_is_refused_while_any_entry_exists_even_a_withdrawn_one() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "Player")
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
        // `accounts` and `tournament_players` are deliberately unlinked.
        let pool = test_pool().await;
        bind_account(&pool, 1, 100).await.unwrap();
        crate::tournament::db::upsert_player_binding(&pool, 1, 100, "Player")
            .await
            .unwrap();

        crate::tournament::registration::unbind(&pool, 1).await.unwrap();

        assert_eq!(list_all(&pool).await.unwrap().len(), 1, "accounts must be untouched");
    }

    // Seeding gate tests.

    /// A checked-in field of `n` entrants, unseeded.
    async fn setup_seedable_field(pool: &SqlitePool, n: i64) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "seeding").await;
        for user_id in 1..=n {
            crate::tournament::db::upsert_player_binding(pool, user_id, user_id * 100, "P")
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

        // User 4 moved to the front; the rest keep their relative order.
        crate::tournament::db::set_seed_order(&pool, tournament.id, &[4, 1, 2, 3], false)
            .await
            .unwrap();

        assert_eq!(
            seeds_by_user(&pool, tournament.id).await,
            vec![(1, Some(2)), (2, Some(3)), (3, Some(4)), (4, Some(1))]
        );
    }

    #[tokio::test]
    async fn an_organizer_override_leaves_the_suggestion_intact() {
        // An organizer's override survives — and its converse: the panel
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
        // A reopen clears a suggested order; seeding must be able to write a
        // fresh one afterwards without colliding with the ones it just removed.
        let pool = test_pool().await;
        let tournament = setup_seedable_field(&pool, 3).await;
        crate::tournament::db::set_seed_order(&pool, tournament.id, &[1, 2, 3], true)
            .await
            .unwrap();

        crate::tournament::db::clear_seeds(&pool, tournament.id).await.unwrap();
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

    // /tournament delete gate tests.

    /// A tournament with a row in every table that references it, so a delete has
    /// something to cascade into at each level.
    async fn setup_fully_populated_tournament(pool: &SqlitePool, slug: &str, user_id: i64) -> i64 {
        let tournament_id = crate::tournament::db::insert_tournament(pool, slug, "Name", user_id)
            .await
            .unwrap();
        crate::tournament::db::add_admin(pool, tournament_id, user_id, user_id)
            .await
            .unwrap();
        crate::tournament::db::upsert_player_binding(pool, user_id, user_id * 100, "P")
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

        // The Discord↔aoe4world binding is global — it must outlive any one
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

    #[tokio::test]
    async fn adding_invited_by_leaves_every_existing_entry_untouched() {
        // Additive, unlike the rebuild before it — but a column landing on rows
        // that already exist is exactly what the empty-database migrator tests
        // cannot see, and the two migrations before this one both did it.
        let pool = pool_migrated_to_before(INVITED_ENTRANTS).await;
        // Status `seeding`, not the default `registration` — a later migration
        // (0013) clears a premature seed on any tournament still open, and this
        // test is about `invited_by` landing cleanly, not about that cleanup.
        sqlx::query("insert into tournaments (slug, name, created_by, status) values ('cup', 'Cup', 1, 'seeding')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into tournament_players (user_id, aoe4_id, display_name) values (1, 100, 'A')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into tournament_entries (tournament_id, user_id, aoe4_id, display_name, seed) \
             values (1, 1, 100, 'A', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        let entry = crate::tournament::db::get_entry(&pool, 1, 1).await.unwrap().unwrap();
        assert_eq!(entry.aoe4_id, 100);
        assert_eq!(entry.seed, Some(1));
        assert_eq!(entry.display_name, "A");
        // A row that predates the column is self-registered, not invited by nobody.
        assert_eq!(entry.invited_by, None);
    }

    /// An invited entrant, as `/tournament invite` creates one — `name` is
    /// what the entry ends up displaying. A real (test-only) profile is
    /// seeded first, `user_id * 100` — this file's existing convention for a
    /// player fixture's id — so the claim resolves by reusing it rather than
    /// reaching a network fetch; seeding is a no-op for a user a test already
    /// gave a real binding, so it never overrides one. User 1 is the inviting
    /// admin throughout.
    async fn invite_to(
        pool: &SqlitePool,
        tournament: &crate::tournament::db::Tournament,
        user_id: i64,
        name: &str,
        seed: Option<i64>,
    ) -> crate::tournament::invite::InviteOutcome {
        crate::tournament::db::upsert_player_binding(pool, user_id, user_id * 100, name)
            .await
            .unwrap();
        invite_with_profile(pool, tournament, user_id, user_id * 100, seed).await
    }

    /// The full form, for the tests that care exactly which profile got
    /// picked.
    async fn invite_with_profile(
        pool: &SqlitePool,
        tournament: &crate::tournament::db::Tournament,
        user_id: i64,
        profile: i64,
        seed: Option<i64>,
    ) -> crate::tournament::invite::InviteOutcome {
        crate::tournament::invite::invite(pool, tournament, user_id, profile, 1, seed)
            .await
            .unwrap()
    }

    /// A self-registered entrant, written the way `register` would.
    async fn sign_up(pool: &SqlitePool, tournament_id: i64, user_id: i64, aoe4_id: i64, name: &str) {
        crate::tournament::db::upsert_player_binding(pool, user_id, aoe4_id, name)
            .await
            .unwrap();
        crate::tournament::db::insert_entry(pool, tournament_id, user_id, aoe4_id, name, None)
            .await
            .unwrap();
    }

    async fn entries_of(pool: &SqlitePool, tournament_id: i64) -> Vec<crate::tournament::db::TournamentEntry> {
        crate::tournament::db::list_entries_for_tournament(pool, tournament_id)
            .await
            .unwrap()
    }

    async fn reload(pool: &SqlitePool, tournament_id: i64) -> crate::tournament::db::Tournament {
        crate::tournament::db::get_tournament(pool, tournament_id)
            .await
            .unwrap()
            .unwrap()
    }

    /// Simulates the close-time resolution — `seeding::resolved_order` +
    /// `set_seed_order`, the write half of what `refresh_ratings` does, without
    /// the network calls it also makes to refresh ratings first. A pin is only
    /// a `manual_seed` until something runs this; every seeded-invite/seed-set
    /// test that asserts a final, compacted `seed` needs to call it first.
    async fn resolve_seeds(pool: &SqlitePool, tournament_id: i64) {
        let entries = entries_of(pool, tournament_id).await;
        crate::tournament::db::set_seed_order(
            pool,
            tournament_id,
            &crate::tournament::seeding::resolved_order(&entries),
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_invitee_survives_close_checkin_and_is_left_out_of_its_counts() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;

        // 2 signs up and checks in, 3 signs up and never does, 4 is invited.
        sign_up(&pool, tournament.id, 2, 200, "Checked").await;
        sign_up(&pool, tournament.id, 3, 300, "Absent").await;
        let outcome = invite_to(&pool, &tournament, 4, "Invitee", None).await;
        assert!(
            matches!(outcome, crate::tournament::invite::InviteOutcome::Invited { .. }),
            "{outcome:?}"
        );

        crate::tournament::db::update_tournament_status(&pool, tournament.id, "checkin")
            .await
            .unwrap();
        let tournament = reload(&pool, tournament.id).await;
        crate::tournament::checkin::checkin(&pool, &tournament, 2)
            .await
            .unwrap();

        // 1 of 2, not 1 of 3: the invitee was never asked, so counting them
        // would report a field that had not confirmed.
        let outcome = crate::tournament::checkin::close(&pool, &tournament).await.unwrap();
        assert_eq!(
            outcome,
            crate::tournament::checkin::CloseCheckinOutcome::Closed {
                checked_in_count: 1,
                no_show_count: 1
            }
        );

        let entries = entries_of(&pool, tournament.id).await;
        let status_of = |user_id: i64| entries.iter().find(|e| e.user_id == user_id).unwrap().status.clone();
        assert_eq!(status_of(2), "active");
        assert_eq!(status_of(3), "no_show");
        assert_eq!(status_of(4), "active", "an invitee is exempt from the sweep");
    }

    #[tokio::test]
    async fn an_invite_past_the_cap_is_refused_but_a_correction_inside_a_full_field_is_not() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::set_entrant_cap(&pool, tournament.id, 2)
            .await
            .unwrap();
        let tournament = reload(&pool, tournament.id).await;

        invite_to(&pool, &tournament, 2, "A", None).await;
        invite_to(&pool, &tournament, 3, "B", None).await;

        let outcome = invite_to(&pool, &tournament, 4, "C", None).await;
        assert_eq!(outcome, crate::tournament::invite::InviteOutcome::FieldFull { cap: 2 });
        assert!(
            crate::tournament::db::get_entry(&pool, tournament.id, 4)
                .await
                .unwrap()
                .is_none(),
            "a refused invite must write nothing"
        );

        // Reinviting an already-active entrant is not a third entrant, so the
        // cap must not block it — even though the placeholder name passed this
        // time differs, since `Reenter` reuses the bound profile's real one
        // rather than reading anything typed this time.
        let outcome = invite_with_profile(&pool, &tournament, 3, 300, None).await;
        assert!(
            matches!(outcome, crate::tournament::invite::InviteOutcome::Reinvited { .. }),
            "{outcome:?}"
        );
        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 3)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.display_name, "B");
    }

    #[tokio::test]
    async fn uninvite_is_scoped_to_entries_an_admin_created() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        sign_up(&pool, tournament.id, 2, 200, "SelfMade").await;
        invite_to(&pool, &tournament, 3, "Invited", None).await;

        let uninvite = async |user_id| {
            crate::tournament::invite::uninvite(&pool, &tournament, user_id)
                .await
                .unwrap()
        };

        assert_eq!(
            uninvite(2).await,
            crate::tournament::invite::UninviteOutcome::NotInvited {
                display_name: "SelfMade".to_string()
            }
        );
        assert_eq!(
            uninvite(99).await,
            crate::tournament::invite::UninviteOutcome::NotInField
        );
        assert_eq!(
            uninvite(3).await,
            crate::tournament::invite::UninviteOutcome::Uninvited {
                display_name: "Invited".to_string()
            }
        );

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 3)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, "withdrawn", "entries are never deleted");
        assert_eq!(entry.invited_by, Some(1), "how the entry came to exist is kept");
        assert_eq!(
            uninvite(3).await,
            crate::tournament::invite::UninviteOutcome::AlreadyOut {
                display_name: "Invited".to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_reinvite_brings_an_uninvited_entrant_back() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        invite_to(&pool, &tournament, 2, "Invited", None).await;
        crate::tournament::invite::uninvite(&pool, &tournament, 2)
            .await
            .unwrap();

        let outcome = invite_to(&pool, &tournament, 2, "Invited", None).await;
        assert!(
            matches!(outcome, crate::tournament::invite::InviteOutcome::Reinvited { .. }),
            "{outcome:?}"
        );
        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, "active");
    }

    #[tokio::test]
    async fn a_seeded_invite_leaves_the_field_contiguous_and_the_order_manual() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        invite_to(&pool, &tournament, 2, "A", None).await;
        invite_to(&pool, &tournament, 3, "B", None).await;

        let outcome = invite_to(&pool, &tournament, 4, "C", Some(1)).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::Invited {
                display_name: "C".to_string(),
                seed: Some(1),
                displaced: None,
                elo: None,
            }
        );

        // A pin is only a `manual_seed` before close — `seed` itself is a
        // close-time computation, not something a plain invite writes early.
        let entries = entries_of(&pool, tournament.id).await;
        assert!(
            entries.iter().all(|e| e.seed.is_none()),
            "seed is a close-time value, not written by invite"
        );
        let placed = entries.iter().find(|e| e.user_id == 4).unwrap();
        assert_eq!(placed.manual_seed, Some(1));

        resolve_seeds(&pool, tournament.id).await;
        let entries = entries_of(&pool, tournament.id).await;
        let mut seeds: Vec<i64> = entries.iter().filter_map(|e| e.seed).collect();
        seeds.sort_unstable();
        assert_eq!(seeds, vec![1, 2, 3], "the field must stay 1..n for start");
        let placed = entries.iter().find(|e| e.user_id == 4).unwrap();
        assert_eq!(placed.seed, Some(1));

        // Without this the seeding pass at close-checkin discards the placement.
        assert_eq!(reload(&pool, tournament.id).await.seed_source, "manual");
    }

    #[tokio::test]
    async fn a_seed_past_the_cap_is_refused_before_the_invite_is_written() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::set_entrant_cap(&pool, tournament.id, 4)
            .await
            .unwrap();
        let tournament = reload(&pool, tournament.id).await;
        invite_to(&pool, &tournament, 2, "A", None).await;

        let outcome = invite_to(&pool, &tournament, 3, "B", Some(5)).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::SeedOutOfRange { cap: 4 }
        );
        assert!(
            crate::tournament::db::get_entry(&pool, tournament.id, 3)
                .await
                .unwrap()
                .is_none(),
            "a half-written invite would leave an admin guessing which half"
        );

        let outcome = invite_to(&pool, &tournament, 3, "B", Some(0)).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::SeedOutOfRange { cap: 4 }
        );
    }

    #[tokio::test]
    async fn a_seed_past_the_current_field_is_accepted_up_to_the_cap_and_compacts() {
        // The seat range is the event's own size, not the field composed so
        // far — an invite-only bracket preview already draws every seat up to
        // the cap, and an organizer should be able to place someone into one
        // before the seats in front of it are filled.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        invite_to(&pool, &tournament, 2, "A", None).await;

        let outcome = invite_to(&pool, &tournament, 3, "B", Some(8)).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::Invited {
                display_name: "B".to_string(),
                seed: Some(8),
                displaced: None,
                elo: None,
            },
            "the reply names the seat asked for, even though only 2 seats are filled"
        );

        // The pin is live (visible as `manual_seed`) immediately, but `seed`
        // itself is a close-time computation — resolving it now, before more
        // entrants can arrive, would compact the pin before it's clear whether
        // anyone will ever fill the seats in front of it.
        let entries = entries_of(&pool, tournament.id).await;
        assert!(
            entries.iter().all(|e| e.seed.is_none()),
            "seed is a close-time value, not written by invite"
        );
        let placed = entries.iter().find(|e| e.user_id == 3).unwrap();
        assert_eq!(placed.manual_seed, Some(8));

        // Growing the field to 8 and resolving now — the field's actual size —
        // is what lets the pin finally hold its own seat rather than compact.
        for (user_id, name) in [(4, "C"), (5, "D"), (6, "E"), (7, "F"), (8, "G"), (9, "H")] {
            invite_to(&pool, &tournament, user_id, name, None).await;
        }
        resolve_seeds(&pool, tournament.id).await;

        let entries = entries_of(&pool, tournament.id).await;
        let placed = entries.iter().find(|e| e.user_id == 3).unwrap();
        assert_eq!(
            placed.seed,
            Some(8),
            "the field grew into seat 8, so the pin holds it now"
        );
    }

    #[tokio::test]
    async fn pinning_a_seat_someone_else_holds_displaces_them() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        invite_to(&pool, &tournament, 2, "A", Some(1)).await;

        let outcome = invite_to(&pool, &tournament, 3, "B", Some(1)).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::Invited {
                display_name: "B".to_string(),
                seed: Some(1),
                displaced: Some("A".to_string()),
                elo: None,
            }
        );

        let entries = entries_of(&pool, tournament.id).await;
        let manual_seed_of = |user_id: i64| entries.iter().find(|e| e.user_id == user_id).unwrap().manual_seed;
        assert_eq!(manual_seed_of(3), Some(1));
        assert_eq!(
            manual_seed_of(2),
            None,
            "the previous holder's pin is unset, not just outranked"
        );

        resolve_seeds(&pool, tournament.id).await;
        let entries = entries_of(&pool, tournament.id).await;
        let seed_of = |user_id: i64| entries.iter().find(|e| e.user_id == user_id).unwrap().seed;
        assert_eq!(seed_of(3), Some(1));
        let mut seeds: Vec<i64> = entries.iter().filter_map(|e| e.seed).collect();
        seeds.sort_unstable();
        assert_eq!(seeds, vec![1, 2], "the field stays contiguous");
    }

    #[tokio::test]
    async fn uninviting_from_a_seeded_field_closes_the_gap_it_leaves() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        invite_to(&pool, &tournament, 2, "A", None).await;
        invite_to(&pool, &tournament, 3, "B", None).await;
        invite_to(&pool, &tournament, 4, "C", Some(1)).await;
        // Close, so the field actually has real seeds for uninvite to compact —
        // before that, `seed` is a close-time value, and `uninvite`'s own
        // compaction is guarded on one already existing.
        resolve_seeds(&pool, tournament.id).await;

        crate::tournament::invite::uninvite(&pool, &tournament, 4)
            .await
            .unwrap();

        let entries = entries_of(&pool, tournament.id).await;
        let mut seeds: Vec<i64> = entries
            .iter()
            .filter(|e| e.status == "active")
            .filter_map(|e| e.seed)
            .collect();
        seeds.sort_unstable();
        assert_eq!(
            seeds,
            vec![1, 2],
            "removing seed 1 must not leave the field starting at 2"
        );
    }

    #[tokio::test]
    async fn an_unseeded_field_is_left_unseeded_by_an_uninvite() {
        // Compacting an order nobody wrote would invent one, and the seeding
        // panel would then show a ranking no organizer asked for.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        invite_to(&pool, &tournament, 2, "A", None).await;
        invite_to(&pool, &tournament, 3, "B", None).await;

        crate::tournament::invite::uninvite(&pool, &tournament, 3)
            .await
            .unwrap();

        let entries = entries_of(&pool, tournament.id).await;
        assert!(
            entries.iter().all(|e| e.seed.is_none()),
            "no seed should have been invented"
        );
    }

    #[tokio::test]
    async fn inviting_closes_once_the_order_is_being_finalized() {
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "seeding").await;

        let outcome = invite_to(&pool, &tournament, 2, "A", None).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::InvitesClosed {
                current_status: "seeding".to_string()
            }
        );
        assert!(
            crate::tournament::db::get_entry(&pool, tournament.id, 2)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn inviting_someone_already_bound_elsewhere_resolves_it_immediately() {
        // Picking exactly the profile this Discord account already carries
        // is `Reenter`, not a fresh claim — no fetch, and the real name and
        // binding are reused rather than what was typed as a placeholder.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::upsert_player_binding(&pool, 2, 200, "RealName")
            .await
            .unwrap();

        let outcome = invite_with_profile(&pool, &tournament, 2, 200, None).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::Invited {
                display_name: "RealName".to_string(),
                seed: None,
                displaced: None,
                elo: None,
            }
        );

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.aoe4_id, 200);
        assert_eq!(entry.display_name, "RealName");

        // Already linked, so a later register call has nothing left to do.
        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::AlreadyRegistered {
                display_name: "RealName".to_string(),
                entrant_number: 1
            }
        );
    }

    #[tokio::test]
    async fn inviting_with_a_profile_someone_else_already_owns_is_refused() {
        // Case A: the picked profile is unrelated to this Discord account —
        // caught by `claim_profile`'s own pre-check, before any fetch and
        // before the tournament entry is written.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::upsert_player_binding(&pool, 9, 500, "Owner")
            .await
            .unwrap();

        let outcome = invite_with_profile(&pool, &tournament, 2, 500, None).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::ProfileClaimedByAnother {
                other_user_id: 9,
                other_display_name: "Owner".to_string(),
            }
        );
        assert!(
            crate::tournament::db::get_entry(&pool, tournament.id, 2)
                .await
                .unwrap()
                .is_none(),
            "a refused invite must write no tournament entry"
        );
        // Not even a player row: `claim_profile`'s own pre-check refuses before
        // `upsert_player_binding` ever runs, so there is nothing left behind at
        // all — an improvement over the old design, which had to tolerate an
        // empty row here as the price of ensuring one existed before a claim.
        assert!(crate::tournament::db::get_player(&pool, 2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn inviting_with_a_profile_that_conflicts_with_an_existing_binding_is_refused() {
        // Case B: the profile itself belongs to nobody else, but this Discord
        // account already carries a *different* one — refused outright rather
        // than either overriding it or silently keeping it.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        crate::tournament::db::upsert_player_binding(&pool, 2, 200, "RealName")
            .await
            .unwrap();

        let outcome = invite_with_profile(&pool, &tournament, 2, 999, None).await;
        assert_eq!(
            outcome,
            crate::tournament::invite::InviteOutcome::AlreadyBoundToDifferentProfile {
                display_name: "RealName".to_string(),
            }
        );
        assert!(
            crate::tournament::db::get_entry(&pool, tournament.id, 2)
                .await
                .unwrap()
                .is_none(),
            "a refused invite must write nothing"
        );
        let player = crate::tournament::db::get_player(&pool, 2).await.unwrap().unwrap();
        assert_eq!(player.aoe4_id, 200, "the real binding must survive the refused pick");
    }

    #[tokio::test]
    async fn a_bound_entry_still_ignores_a_supplied_profile() {
        // An existing entry never looks at the argument at all any more — a
        // real snapshot stays immutable, and `/tournament rebind` stays the
        // way to change a binding.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        sign_up(&pool, tournament.id, 2, 200, "Bound").await;

        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, Some(999))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::AlreadyRegistered {
                display_name: "Bound".to_string(),
                entrant_number: 1
            }
        );

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.aoe4_id, 200);
    }

    /// A tournament with the public door shut.
    async fn invite_only_tournament(pool: &SqlitePool) -> crate::tournament::db::Tournament {
        let tournament = setup_tournament(pool, "registration").await;
        crate::tournament::db::set_registration_mode(pool, tournament.id, "invite_only")
            .await
            .unwrap();
        reload(pool, tournament.id).await
    }

    #[tokio::test]
    async fn a_stranger_cannot_sign_themselves_into_an_invite_only_field() {
        let pool = test_pool().await;
        let tournament = invite_only_tournament(&pool).await;
        crate::tournament::db::upsert_player_binding(&pool, 2, 200, "Stranger")
            .await
            .unwrap();

        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, None)
            .await
            .unwrap();
        // Not RegistrationClosed: that wording sends people looking for a reopen
        // that is never coming.
        assert_eq!(outcome, crate::tournament::registration::RegisterOutcome::InviteOnly);
        assert!(
            crate::tournament::db::get_entry(&pool, tournament.id, 2)
                .await
                .unwrap()
                .is_none(),
            "a refused sign-up must write nothing"
        );
    }

    #[tokio::test]
    async fn an_invited_entrant_can_still_withdraw_from_an_invite_only_field() {
        // The whole reason this is three states and not two: shutting the public
        // door does not lock anyone in.
        let pool = test_pool().await;
        let tournament = invite_only_tournament(&pool).await;
        invite_to(&pool, &tournament, 2, "Invited", None).await;

        let outcome = crate::tournament::registration::withdraw(&pool, &tournament, 2)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::registration::WithdrawOutcome::Success);
    }

    #[tokio::test]
    async fn coming_back_from_a_withdrawal_needs_another_invite() {
        let pool = test_pool().await;
        let tournament = invite_only_tournament(&pool).await;
        invite_to(&pool, &tournament, 2, "Invited", None).await;
        crate::tournament::registration::withdraw(&pool, &tournament, 2)
            .await
            .unwrap();

        // Rejoining is a sign-up, so the shut door refuses it too.
        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, None)
            .await
            .unwrap();
        assert_eq!(outcome, crate::tournament::registration::RegisterOutcome::InviteOnly);
        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, "withdrawn", "the refusal must not have revived them");

        // The way back in is the organizers' own verb.
        invite_to(&pool, &tournament, 2, "Invited", None).await;
        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, "active");
    }

    #[tokio::test]
    async fn an_invitee_can_still_bind_a_profile_in_an_invite_only_field() {
        // Invite-only doesn't change how a binding resolves: an already-bound
        // account is linked immediately, and register on top of that is not
        // blocked by the invite-only gate — the existing-entry check runs first
        // regardless of mode.
        let pool = test_pool().await;
        let tournament = invite_only_tournament(&pool).await;
        crate::tournament::db::upsert_player_binding(&pool, 2, 200, "RealName")
            .await
            .unwrap();
        invite_to(&pool, &tournament, 2, "Guess", None).await;

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.aoe4_id, 200);
        assert_eq!(entry.display_name, "RealName");

        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::AlreadyRegistered {
                display_name: "RealName".to_string(),
                entrant_number: 1
            }
        );
    }

    #[tokio::test]
    async fn inviting_does_not_consult_the_mode() {
        // `invite` is the organizers' door; `registration_mode` governs the public
        // one. An open event can still have invited entrants alongside sign-ups.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        assert_eq!(tournament.registration_mode, "open");

        let outcome = invite_to(&pool, &tournament, 2, "Invited", None).await;
        assert!(
            matches!(outcome, crate::tournament::invite::InviteOutcome::Invited { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn shutting_the_door_leaves_everyone_already_in_the_field_in_it() {
        // The mode governs the door, not the roster — flipping it mid-registration
        // must not eject the people who arrived through the open one.
        let pool = test_pool().await;
        let tournament = setup_tournament(&pool, "registration").await;
        sign_up(&pool, tournament.id, 2, 200, "Early").await;

        crate::tournament::db::set_registration_mode(&pool, tournament.id, "invite_only")
            .await
            .unwrap();
        let tournament = reload(&pool, tournament.id).await;

        let entry = crate::tournament::db::get_entry(&pool, tournament.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, "active");
        // And they can still be told apart from an invitee, so the sweep and the
        // counter treat them as the self-registered entrant they are.
        assert_eq!(entry.invited_by, None);

        let outcome = crate::tournament::registration::register(&pool, &tournament, 2, None)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            crate::tournament::registration::RegisterOutcome::AlreadyRegistered {
                display_name: "Early".to_string(),
                entrant_number: 1
            }
        );
    }
}
