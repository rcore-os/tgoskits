#include <sqlite3.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/stat.h>
#include <errno.h>

static int passed = 0;
static int failed = 0;

#define CHECK(cond, fmt, ...) do {                                    \
    if (cond) { printf("  PASS: " fmt "\n", ##__VA_ARGS__); passed++; } \
    else      { printf("  FAIL: " fmt "\n", ##__VA_ARGS__); failed++; } \
} while (0)

static int exec_sql(sqlite3 *db, const char *sql) {
    char *err = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &err);
    if (err) {
        printf("    SQL error: %s\n", err);
        sqlite3_free(err);
    }
    return rc;
}

static int callback_count(void *data, int argc, char **argv, char **col) {
    int *cnt = (int *)data;
    (*cnt)++;
    (void)argc; (void)argv; (void)col;
    return 0;
}

static void test_memory_db(void) {
    printf("\n=== T1: Memory DB ===\n");
    sqlite3 *db;
    int rc = sqlite3_open(":memory:", &db);
    CHECK(rc == SQLITE_OK, "open :memory:");
    if (rc != SQLITE_OK) { sqlite3_close(db); return; }

    rc = exec_sql(db, "CREATE TABLE t1(id INTEGER PRIMARY KEY, v TEXT)");
    CHECK(rc == SQLITE_OK, "CREATE TABLE");

    rc = exec_sql(db, "INSERT INTO t1(v) VALUES('hello')");
    CHECK(rc == SQLITE_OK, "INSERT");

    int cnt = 0;
    char *err = NULL;
    sqlite3_exec(db, "SELECT * FROM t1", callback_count, &cnt, &err);
    if (err) sqlite3_free(err);
    CHECK(cnt == 1, "SELECT count = 1 (got %d)", cnt);

    sqlite3_close(db);
}

static void test_file_db_basic(void) {
    printf("\n=== T2: File DB basic ===\n");
    const char *path = "/tmp/deep_test.db";
    unlink(path);

    sqlite3 *db;
    int rc = sqlite3_open(path, &db);
    CHECK(rc == SQLITE_OK, "open %s", path);
    if (rc != SQLITE_OK) { sqlite3_close(db); return; }

    rc = exec_sql(db, "PRAGMA journal_mode=DELETE");
    CHECK(rc == SQLITE_OK, "PRAGMA journal_mode=DELETE");

    rc = exec_sql(db, "CREATE TABLE t2(id INTEGER PRIMARY KEY, v TEXT)");
    CHECK(rc == SQLITE_OK, "CREATE TABLE");

    rc = exec_sql(db, "BEGIN TRANSACTION");
    CHECK(rc == SQLITE_OK, "BEGIN");
    for (int i = 0; i < 100; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql), "INSERT INTO t2(v) VALUES('row-%d')", i);
        exec_sql(db, sql);
    }
    rc = exec_sql(db, "COMMIT");
    CHECK(rc == SQLITE_OK, "COMMIT 100 rows");

    int cnt = 0;
    char *err = NULL;
    sqlite3_exec(db, "SELECT * FROM t2", callback_count, &cnt, &err);
    if (err) sqlite3_free(err);
    CHECK(cnt == 100, "row count = 100 (got %d)", cnt);

    struct stat st;
    int ret = stat(path, &st);
    CHECK(ret == 0 && st.st_size > 0, "db file exists and non-empty (size=%ld)",
          ret == 0 ? (long)st.st_size : -1L);

    sqlite3_close(db);
}

static void test_rollback(void) {
    printf("\n=== T3: Rollback ===\n");
    const char *path = "/tmp/deep_test.db";
    sqlite3 *db;
    int rc = sqlite3_open(path, &db);
    CHECK(rc == SQLITE_OK, "open existing db");
    if (rc != SQLITE_OK) { sqlite3_close(db); return; }

    int before = 0;
    char *err = NULL;
    sqlite3_exec(db, "SELECT * FROM t2", callback_count, &before, &err);
    if (err) sqlite3_free(err);

    exec_sql(db, "BEGIN");
    exec_sql(db, "INSERT INTO t2(v) VALUES('should-not-exist')");
    int during = 0;
    sqlite3_exec(db, "SELECT * FROM t2", callback_count, &during, &err);
    if (err) sqlite3_free(err);
    CHECK(during == before + 1, "row during txn = %d (before=%d)", during, before);

    exec_sql(db, "ROLLBACK");
    int after = 0;
    sqlite3_exec(db, "SELECT * FROM t2", callback_count, &after, &err);
    if (err) sqlite3_free(err);
    CHECK(after == before, "row after rollback = %d (same as before=%d)", after, before);

    sqlite3_close(db);
}

static void test_wal_mode(void) {
    printf("\n=== T4: WAL mode ===\n");
    const char *path = "/tmp/wal_deep.db";
    unlink(path);

    sqlite3 *db;
    int rc = sqlite3_open(path, &db);
    CHECK(rc == SQLITE_OK, "open wal db");
    if (rc != SQLITE_OK) { sqlite3_close(db); return; }

    char *mode = NULL;
    char *err = NULL;
    sqlite3_exec(db, "PRAGMA journal_mode=WAL", callback_count, &mode, &err);
    if (err) { printf("    WAL err: %s\n", err); sqlite3_free(err); }

    rc = exec_sql(db, "CREATE TABLE w(id INTEGER PRIMARY KEY, d BLOB)");
    CHECK(rc == SQLITE_OK, "CREATE TABLE in WAL");

    exec_sql(db, "BEGIN");
    for (int i = 0; i < 500; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql),
                 "INSERT INTO w(d) VALUES(zeroblob(%d))", 256 + (i % 64));
        exec_sql(db, sql);
    }
    rc = exec_sql(db, "COMMIT");
    CHECK(rc == SQLITE_OK, "COMMIT 500 blob rows in WAL");

    int cnt = 0;
    sqlite3_exec(db, "SELECT * FROM w", callback_count, &cnt, &err);
    if (err) sqlite3_free(err);
    CHECK(cnt == 500, "WAL row count = 500 (got %d)", cnt);

    exec_sql(db, "PRAGMA wal_checkpoint(TRUNCATE)");
    rc = exec_sql(db, "PRAGMA journal_mode=DELETE");
    CHECK(rc == SQLITE_OK, "switch back to DELETE mode");

    sqlite3_close(db);
}

static void test_integrity(void) {
    printf("\n=== T5: Integrity check ===\n");
    const char *paths[] = {"/tmp/deep_test.db", "/tmp/wal_deep.db"};
    for (int i = 0; i < 2; i++) {
        sqlite3 *db;
        int rc = sqlite3_open(paths[i], &db);
        if (rc != SQLITE_OK) { sqlite3_close(db); continue; }

        int cnt = 0;
        char *err = NULL;
        sqlite3_exec(db, "PRAGMA integrity_check",
                     callback_count, &cnt, &err);
        if (err) {
            printf("    integrity err on %s: %s\n", paths[i], err);
            sqlite3_free(err);
            failed++;
        }
        sqlite3_close(db);
        CHECK(err == NULL, "integrity_check %s", paths[i]);
    }
}

static void test_fsync_behavior(void) {
    printf("\n=== T6: Fsync stress ===\n");
    const char *path = "/tmp/fsync_test.db";
    unlink(path);

    sqlite3 *db;
    int rc = sqlite3_open(path, &db);
    CHECK(rc == SQLITE_OK, "open fsync db");
    if (rc != SQLITE_OK) { sqlite3_close(db); return; }

    exec_sql(db, "PRAGMA synchronous=FULL");
    exec_sql(db, "CREATE TABLE fs(id INTEGER PRIMARY KEY, v TEXT)");

    exec_sql(db, "BEGIN");
    for (int i = 0; i < 200; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql), "INSERT INTO fs(v) VALUES('fsync-%d')", i);
        exec_sql(db, sql);
    }
    rc = exec_sql(db, "COMMIT");
    CHECK(rc == SQLITE_OK, "COMMIT 200 rows with synchronous=FULL");

    exec_sql(db, "PRAGMA synchronous=NORMAL");
    exec_sql(db, "BEGIN");
    for (int i = 200; i < 400; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql), "INSERT INTO fs(v) VALUES('fsync-%d')", i);
        exec_sql(db, sql);
    }
    rc = exec_sql(db, "COMMIT");
    CHECK(rc == SQLITE_OK, "COMMIT 200 rows with synchronous=NORMAL");

    exec_sql(db, "PRAGMA synchronous=OFF");
    exec_sql(db, "BEGIN");
    for (int i = 400; i < 600; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql), "INSERT INTO fs(v) VALUES('fsync-%d')", i);
        exec_sql(db, sql);
    }
    rc = exec_sql(db, "COMMIT");
    CHECK(rc == SQLITE_OK, "COMMIT 200 rows with synchronous=OFF");

    int cnt = 0;
    char *err = NULL;
    sqlite3_exec(db, "SELECT * FROM fs", callback_count, &cnt, &err);
    if (err) sqlite3_free(err);
    CHECK(cnt == 600, "total rows = 600 (got %d)", cnt);

    sqlite3_close(db);
}

static void test_attach_crossdb(void) {
    printf("\n=== T7: ATTACH cross-DB ===\n");
    const char *path1 = "/tmp/att1.db";
    const char *path2 = "/tmp/att2.db";
    unlink(path1);
    unlink(path2);

    sqlite3 *db1;
    int rc = sqlite3_open(path1, &db1);
    CHECK(rc == SQLITE_OK, "open att1.db");
    if (rc != SQLITE_OK) { sqlite3_close(db1); return; }

    exec_sql(db1, "CREATE TABLE a1(x INTEGER)");
    exec_sql(db1, "INSERT INTO a1(x) VALUES(10),(20),(30)");

    rc = exec_sql(db1, "ATTACH DATABASE '/tmp/att2.db' AS db2");
    CHECK(rc == SQLITE_OK, "ATTACH");

    exec_sql(db1, "CREATE TABLE db2.b1(y TEXT)");
    exec_sql(db1, "INSERT INTO db2.b1(y) SELECT 'val-'||x FROM a1");

    int cnt = 0;
    char *err = NULL;
    sqlite3_exec(db1, "SELECT y FROM db2.b1", callback_count, &cnt, &err);
    if (err) sqlite3_free(err);
    CHECK(cnt == 3, "cross-DB select count = 3 (got %d)", cnt);

    exec_sql(db1, "DETACH db2");
    sqlite3_close(db1);
}

static void test_blob_large(void) {
    printf("\n=== T8: Large BLOB (1MB) ===\n");
    const char *path = "/tmp/blob_test.db";
    unlink(path);

    sqlite3 *db;
    int rc = sqlite3_open(path, &db);
    CHECK(rc == SQLITE_OK, "open blob db");
    if (rc != SQLITE_OK) { sqlite3_close(db); return; }

    exec_sql(db, "CREATE TABLE blobs(id INTEGER PRIMARY KEY, data BLOB)");
    rc = exec_sql(db, "INSERT INTO blobs(data) VALUES(zeroblob(1048576))");
    CHECK(rc == SQLITE_OK, "INSERT 1MB zeroblob");

    sqlite3_stmt *stmt;
    rc = sqlite3_prepare_v2(db, "SELECT length(data) FROM blobs", -1, &stmt, NULL);
    CHECK(rc == SQLITE_OK, "prepare SELECT length");

    if (rc == SQLITE_OK) {
        rc = sqlite3_step(stmt);
        if (rc == SQLITE_ROW) {
            int len = sqlite3_column_int(stmt, 0);
            CHECK(len == 1048576, "blob length = 1048576 (got %d)", len);
        }
        sqlite3_finalize(stmt);
    }

    sqlite3_close(db);
}

int main(void) {
    printf("=== SQLite Deep Test Suite ===\n");

    test_memory_db();
    test_file_db_basic();
    test_rollback();
    test_wal_mode();
    test_fsync_behavior();
    test_attach_crossdb();
    test_blob_large();
    test_integrity();

    printf("\n=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed > 0) {
        printf("SOME TESTS FAILED\n");
        return 1;
    }
    printf("All tests passed!\n");
    return 0;
}
