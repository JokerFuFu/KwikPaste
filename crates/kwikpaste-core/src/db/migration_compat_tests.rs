//! Upgrade fixtures are frozen SQL read from fork main 7a17ef5 and author main f4cfdc0.
//! Build the old schema from those fixtures, never from the current migration list.

use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::{AssertSqlSafe, QueryBuilder, SqlSafeStr, Sqlite, SqlitePool};

use super::{adopt_published_checksums, hex, MIGRATOR, PUBLISHED};

const FORK_SQL: [&str; 8] = [
    include_str!("fixtures/fork-main-7a17ef5/0001_init.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0002_fts_update_trigger.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0003_list_sort_indexes.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0004_last_used_at.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0005_origin_device.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0006_sync_seq.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0007_manual_order.sql"),
    include_str!("fixtures/fork-main-7a17ef5/0008_image_ocr.sql"),
];
const AUTHOR_SQL: &str = include_str!("fixtures/author-f4cfdc0/0008_image_texts.sql");
const FORK_EIGHT_LF: &str = "7dda1a0877cddd410657f66cfbd35e3ba1d50f5fcee21f8390902077ca66516a8eef52164acbc67e4de315d2c34f95b5";
const FORK_EIGHT_CRLF: &str = "2a60b92a1a78742cad7435e7e2b26264f3ce05f28fd502d9e6e6e6ebd5bb08ec9f1d6d580d93a39b136ab42afcc35cbc";
const AUTHOR_EIGHT_LF: &str = "e9a4905e3f6285520158e89fd59054ad5beb1549c5cd2ee9bd099e7efc64c6cbd7d9a30500171243a6a4091f26ddd958";
const AUTHOR_EIGHT_CRLF: &str = "32ae6ae06370708b427782341654b48bdefa38d232320db81581b3a4d9122ac34e7c0b4396544c7561f71f95dd790920";

/// Compute a released platform checksum from frozen SQL rather than current migrations.
fn historical_migration(version: i64, sql: &str, crlf: bool) -> Migration {
    let normalized = sql.replace("\r\n", "\n");
    let bytes = if crlf {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    };
    Migration::new(
        version,
        "released fixture".into(),
        MigrationType::Simple,
        AssertSqlSafe(bytes).into_sql_str(),
        false,
    )
}

fn native_crlf() -> bool {
    MIGRATOR
        .iter()
        .next()
        .unwrap()
        .sql
        .as_str()
        .contains("\r\n")
}

/// Create and seed a released fork database independently of the upgrade migrator.
pub(crate) async fn historical_database(pool: &SqlitePool, version: usize, crlf: bool) {
    let migrations = FORK_SQL
        .iter()
        .take(version)
        .enumerate()
        .map(|(index, sql)| historical_migration(index as i64 + 1, sql, crlf))
        .collect();
    Migrator::with_migrations(migrations)
        .run(pool)
        .await
        .unwrap();
    seed_history(pool, version).await;
}

/// Create the conflicting author schema with its original native 8 ledger entry.
pub(crate) async fn author_database(pool: &SqlitePool, crlf: bool) {
    historical_database(pool, 7, crlf).await;
    let mut migrations: Vec<_> = FORK_SQL
        .iter()
        .take(7)
        .enumerate()
        .map(|(index, sql)| historical_migration(index as i64 + 1, sql, crlf))
        .collect();
    migrations.push(historical_migration(8, AUTHOR_SQL, crlf));
    Migrator::with_migrations(migrations)
        .run(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO image_texts VALUES('original', 'done', 'native invoice 已识别', 'zh', 1, '2026-01-01', '2026-01-02')")
        .execute(pool).await.unwrap();
}

async fn seed_history(pool: &SqlitePool, version: usize) {
    sqlx::raw_sql(
        "INSERT INTO clipboard_groups VALUES('group', '重要', 'folder', 1, 23, '2026-01-01', '2026-01-02');
         INSERT INTO clipboard_apps VALUES('source', 'Preview', 'source.png', 'macos', '2026-01-01', '2026-01-02');
         INSERT INTO clipboard_items(id,kind,sub_kind,group_id,source_app_id,content,content_hash,search_text,summary,file_types,size,width,height,use_count,is_favorite,is_pinned,is_sensitive,platform,note,created_at,updated_at)
         VALUES('original','image','png','group','source','original.png','original-hash','original search','原摘要','png',301,640,480,12,1,1,1,'macos','保留备注','2026-01-01','2026-01-02');
         INSERT INTO clipboard_items(id,kind,content,content_hash,platform,created_at,updated_at)
         VALUES('text','text','原始文字','text-hash','windows','2026-01-03','2026-01-04');",
    ).execute(pool).await.unwrap();
    if version >= 4 {
        sqlx::query("UPDATE clipboard_items SET last_used_at = '2026-02-03'")
            .execute(pool)
            .await
            .unwrap();
    }
    if version >= 5 {
        sqlx::query("UPDATE clipboard_items SET origin_device_id = 'remote-device'")
            .execute(pool)
            .await
            .unwrap();
    }
    if version >= 6 {
        sqlx::raw_sql(
            "UPDATE clipboard_items SET sync_seq = 41; UPDATE sync_counter SET value = 42;",
        )
        .execute(pool)
        .await
        .unwrap();
    }
    if version >= 7 {
        sqlx::query(
            "UPDATE clipboard_items SET pin_order = 73, favorite_order = 91 WHERE id = 'original'",
        )
        .execute(pool)
        .await
        .unwrap();
    }
    if version >= 8 {
        sqlx::query("INSERT INTO image_ocr VALUES('original', 'old-token', 'completed', 'legacy invoice 保留发票', '2026-01-05', '2026-01-06')")
            .execute(pool).await.unwrap();
    }
}

/// Capture original columns so older upgrades can add fields without hiding value changes.
pub(crate) async fn columns(pool: &SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Serialize every original column's SQLite type and bytes, including ledger blobs.
pub(crate) async fn snapshot(pool: &SqlitePool, table: &str, columns: &[String]) -> Vec<String> {
    let mut query = QueryBuilder::<Sqlite>::new("SELECT json_array(");
    let mut fields = query.separated(", ");
    for column in columns {
        let name = column.replace('"', "\"\"");
        fields.push(format!("typeof(\"{name}\"), hex(\"{name}\")"));
    }
    query.push(") FROM ");
    query.push(format!("\"{}\"", table.replace('"', "\"\"")));
    query.push(" ORDER BY rowid");
    query.build_query_scalar().fetch_all(pool).await.unwrap()
}

async fn empty_pool() -> SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .in_memory(true)
                .foreign_keys(true),
        )
        .await
        .unwrap()
}

#[test]
fn frozen_fixtures_identify_both_released_eights_and_platforms() {
    for (sql, lf, crlf) in [
        (FORK_SQL[7], FORK_EIGHT_LF, FORK_EIGHT_CRLF),
        (AUTHOR_SQL, AUTHOR_EIGHT_LF, AUTHOR_EIGHT_CRLF),
    ] {
        assert_eq!(hex(&historical_migration(8, sql, false).checksum), lf);
        assert_eq!(hex(&historical_migration(8, sql, true).checksum), crlf);
    }
    assert_eq!(PUBLISHED[7], (8, FORK_EIGHT_LF, FORK_EIGHT_CRLF));
    assert_eq!(PUBLISHED[8], (9, AUTHOR_EIGHT_LF, AUTHOR_EIGHT_CRLF));
}

#[tokio::test]
async fn released_fork_versions_one_through_eight_upgrade_without_losing_original_columns() {
    for version in 1..=8 {
        let pool = empty_pool().await;
        historical_database(&pool, version, native_crlf()).await;
        let mut before = Vec::new();
        let mut tables = vec![
            "clipboard_items",
            "clipboard_groups",
            "clipboard_apps",
            "_sqlx_migrations",
        ];
        if version >= 6 {
            tables.push("sync_counter");
        }
        if version == 8 {
            tables.push("image_ocr");
        }
        for table in tables {
            let names = columns(&pool, table).await;
            let rows = snapshot(&pool, table, &names).await;
            before.push((table, names, rows));
        }
        MIGRATOR.run(&pool).await.unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        for (table, names, rows) in before {
            let after = snapshot(&pool, table, &names).await;
            if table == "_sqlx_migrations" {
                assert_eq!(
                    rows,
                    after[..version],
                    "released ledger changed for version {version}"
                );
            } else {
                assert_eq!(
                    rows, after,
                    "original {table} rows changed for version {version}"
                );
            }
        }
        let ledger: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(ledger, (1..=9).collect::<Vec<_>>());
        let texts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM image_texts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(texts, 0, "old derived text must not seed native OCR");
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clipboard_items i LEFT JOIN image_texts t ON t.item_id = i.id WHERE i.kind = 'image' AND t.item_id IS NULL").fetch_one(&pool).await.unwrap();
        assert_eq!(
            queued, 1,
            "old images must remain available for native recognition"
        );
        if version == 8 {
            let hits: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM image_ocr_fts WHERE image_ocr_fts MATCH 'invoice'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(hits, 1);
        }
        sqlx::query("INSERT INTO image_texts VALUES('original', 'done', 'native receipt 新识别', 'zh', 1, '2026-03-01', '2026-03-02')").execute(&pool).await.unwrap();
        let hits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM image_texts_fts WHERE image_texts_fts MATCH 'receipt'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(hits, 1, "native FTS trigger must work after upgrade");
        pool.close().await;
    }
}

#[tokio::test]
async fn unknown_fork_eight_checksum_is_not_repaired_or_bypassed() {
    let pool = empty_pool().await;
    historical_database(&pool, 8, native_crlf()).await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 8")
        .execute(&pool)
        .await
        .unwrap();
    let names = columns(&pool, "_sqlx_migrations").await;
    let before = snapshot(&pool, "_sqlx_migrations", &names).await;
    assert_eq!(adopt_published_checksums(&pool).await.unwrap(), 0);
    assert!(matches!(
        MIGRATOR.run(&pool).await,
        Err(MigrateError::VersionMismatch(8))
    ));
    assert_eq!(snapshot(&pool, "_sqlx_migrations", &names).await, before);
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'image_texts'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(exists, 0);
    pool.close().await;
}

#[tokio::test]
async fn author_native_eight_is_rejected_without_relabeling_its_ledger_or_cache() {
    let pool = empty_pool().await;
    author_database(&pool, native_crlf()).await;
    let ledger_columns = columns(&pool, "_sqlx_migrations").await;
    let cache_columns = columns(&pool, "image_texts").await;
    let ledger = snapshot(&pool, "_sqlx_migrations", &ledger_columns).await;
    let cache = snapshot(&pool, "image_texts", &cache_columns).await;
    assert_eq!(adopt_published_checksums(&pool).await.unwrap(), 0);
    assert!(matches!(
        MIGRATOR.run(&pool).await,
        Err(MigrateError::VersionMismatch(8))
    ));
    assert_eq!(
        snapshot(&pool, "_sqlx_migrations", &ledger_columns).await,
        ledger
    );
    assert_eq!(snapshot(&pool, "image_texts", &cache_columns).await, cache);
    pool.close().await;
}
