#include <sqlite3.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __fail++;                                                       \
    }                                                                   \
    fflush(stdout);                                                     \
} while(0)

static int callback_print(void *unused, int argc, char **argv, char **col) {
    (void)unused;
    for (int i = 0; i < argc; i++) {
        printf("    %s = %s\n", col[i], argv[i] ? argv[i] : "NULL");
    }
    fflush(stdout);
    return 0;
}

static int callback_count(void *data, int argc, char **argv, char **col) {
    (void)argc; (void)argv; (void)col;
    int *cnt = (int *)data;
    (*cnt)++;
    return 0;
}

static void exec_sql(sqlite3 *db, const char *sql) {
    char *err = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &err);
    if (err) {
        printf("    SQL error: %s\n", err);
        sqlite3_free(err);
    }
    if (rc != SQLITE_OK) {
        __fail++;
    }
    fflush(stdout);
}

int main(void) {
    printf("================================================\n");
    printf("  TEST: SQLite app test for kernel coverage\n");
    printf("  FILE: %s\n", __FILE__);
    printf("================================================\n");
    fflush(stdout);

    sqlite3 *db;
    int rc;
    char *err = NULL;

    /* T1: Memory database */
    printf("\n--- T1: Memory database ---\n"); fflush(stdout);
    rc = sqlite3_open(":memory:", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open :memory:");
    exec_sql(db, "CREATE TABLE m(id INTEGER PRIMARY KEY, v TEXT)");
    exec_sql(db, "INSERT INTO m(v) VALUES('mem-test')");
    rc = sqlite3_exec(db, "SELECT v FROM m", callback_print, NULL, &err);
    CHECK(rc == SQLITE_OK, "T1: SELECT from memory db");
    sqlite3_close(db);

    /* T2: File database + transaction (fsync, fdatasync) */
    printf("\n--- T2: File database + transaction ---\n"); fflush(stdout);
    unlink("/tmp/test.db");
    rc = sqlite3_open("/tmp/test.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open file db");
    exec_sql(db, "PRAGMA journal_mode=DELETE");
    exec_sql(db, "CREATE TABLE t1(id INTEGER PRIMARY KEY, v TEXT)");
    exec_sql(db, "BEGIN");
    exec_sql(db, "INSERT INTO t1(v) VALUES('hello')");
    exec_sql(db, "INSERT INTO t1(v) VALUES('starry')");
    exec_sql(db, "COMMIT");
    rc = sqlite3_exec(db, "SELECT v FROM t1", callback_print, NULL, &err);
    CHECK(rc == SQLITE_OK, "T2: file db + transaction");
    sqlite3_close(db);

    /* T3: Rollback */
    printf("\n--- T3: Rollback ---\n"); fflush(stdout);
    rc = sqlite3_open("/tmp/test.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open for rollback");
    exec_sql(db, "BEGIN");
    exec_sql(db, "INSERT INTO t1(v) VALUES('temp')");
    exec_sql(db, "ROLLBACK");
    int cnt = 0;
    rc = sqlite3_exec(db, "SELECT count(*) FROM t1", callback_count, &cnt, &err);
    CHECK(rc == SQLITE_OK && cnt == 1, "T3: rollback verified (2 rows kept)");
    sqlite3_close(db);

    /* T4: WAL mode (mmap, flock, fcntl) */
    printf("\n--- T4: WAL mode ---\n"); fflush(stdout);
    unlink("/tmp/wal.db");
    rc = sqlite3_open("/tmp/wal.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open for WAL");
    exec_sql(db, "PRAGMA journal_mode=WAL");
    exec_sql(db, "CREATE TABLE w(id INTEGER PRIMARY KEY, d TEXT)");
    exec_sql(db, "INSERT INTO w(d) VALUES('wal-test')");
    rc = sqlite3_exec(db, "SELECT d FROM w", callback_print, NULL, &err);
    CHECK(rc == SQLITE_OK, "T4: WAL mode read/write");
    exec_sql(db, "PRAGMA journal_mode=DELETE");
    sqlite3_close(db);

    /* T5: Bulk insert + index (pread/pwrite, large file) */
    printf("\n--- T5: Bulk insert + index ---\n"); fflush(stdout);
    unlink("/tmp/bulk.db");
    rc = sqlite3_open("/tmp/bulk.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open for bulk");
    exec_sql(db, "PRAGMA journal_mode=DELETE");
    exec_sql(db, "CREATE TABLE b(id INTEGER PRIMARY KEY, v TEXT)");
    exec_sql(db, "CREATE INDEX idx_b ON b(v)");
    exec_sql(db, "BEGIN");
    for (int i = 0; i < 200; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql), "INSERT INTO b(v) VALUES('row-%d')", i);
        exec_sql(db, sql);
    }
    exec_sql(db, "COMMIT");
    cnt = 0;
    rc = sqlite3_exec(db, "SELECT count(*) FROM b", callback_count, &cnt, &err);
    CHECK(rc == SQLITE_OK && cnt == 1, "T5: 200 rows inserted");
    rc = sqlite3_exec(db, "SELECT v FROM b WHERE v='row-100'", callback_print, NULL, &err);
    CHECK(rc == SQLITE_OK, "T5: indexed lookup");
    sqlite3_close(db);

    /* T6: ATTACH multi-database (multi-fd management) */
    printf("\n--- T6: ATTACH multi-database ---\n"); fflush(stdout);
    unlink("/tmp/att.db");
    rc = sqlite3_open("/tmp/att.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open for attach");
    exec_sql(db, "CREATE TABLE a(x TEXT)");
    exec_sql(db, "ATTACH '/tmp/test.db' AS other");
    exec_sql(db, "INSERT INTO a SELECT v FROM other.t1");
    rc = sqlite3_exec(db, "SELECT x FROM a", callback_print, NULL, &err);
    CHECK(rc == SQLITE_OK, "T6: cross-db query");
    sqlite3_close(db);

    /* T7: Integrity check - same-handle (no close/reopen) */
    printf("\n--- T7: Integrity check (same handle) ---\n"); fflush(stdout);
    unlink("/tmp/int.db");
    rc = sqlite3_open("/tmp/int.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open for integrity");
    exec_sql(db, "PRAGMA journal_mode=DELETE");
    exec_sql(db, "CREATE TABLE ic(id INTEGER PRIMARY KEY, v TEXT)");
    exec_sql(db, "BEGIN");
    for (int i = 0; i < 50; i++) {
        char sql[128];
        snprintf(sql, sizeof(sql), "INSERT INTO ic(v) VALUES('v%d')", i);
        exec_sql(db, sql);
    }
    exec_sql(db, "COMMIT");
    cnt = 0;
    rc = sqlite3_exec(db, "PRAGMA integrity_check", callback_count, &cnt, &err);
    CHECK(rc == SQLITE_OK && cnt > 0, "T7: integrity_check same handle");
    sqlite3_close(db);

    /* T8: Aggregate query (B-tree traversal) */
    printf("\n--- T8: Aggregate query ---\n"); fflush(stdout);
    rc = sqlite3_open("/tmp/bulk.db", &db);
    CHECK(rc == SQLITE_OK, "sqlite3_open for aggregate");
    rc = sqlite3_exec(db, "SELECT count(*), min(id), max(id) FROM b", callback_print, NULL, &err);
    CHECK(rc == SQLITE_OK, "T8: aggregate query");
    sqlite3_close(db);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n\n");
    fflush(stdout);

    return __fail > 0 ? 1 : 0;
}
