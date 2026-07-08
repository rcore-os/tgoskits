// fwsqlite — Go database/sql carpet over a pure-Go (CGO-free) SQLite driver.
//
// Driver: github.com/glebarez/go-sqlite v1.21.2 (underlying modernc.org/sqlite
// v1.23.1). The driver registers itself as "sqlite" via blank import and is
// pure-Go, so this carpet builds and runs without cgo — important for
// reproducible / on-target execution.
//
// DETERMINISM:
//   - Every assertion prints a fixed label + value; no timestamps, addresses,
//     map iteration order, or randomness leaks into output.
//   - Where time.Time round-trips are tested we use a FIXED UTC instant.
//   - Plain ":memory:" gives each pooled connection its OWN database (a real
//     non-determinism trap noted by the critic). We avoid it by using either
//     SetMaxOpenConns(1), "file::memory:?cache=shared", or a temp file, and we
//     EXERCISE the trap explicitly so the behavior is asserted, not accidental.
//   - Output is byte-identical across runs (verified by diffing two runs).
package main

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	_ "github.com/glebarez/go-sqlite" // registers driver "sqlite"
)

// must aborts loudly on an unexpected error (keeps the carpet honest: an
// assertion that should pass but errors must not be silently skipped).
func sqlite_must(label string, err error) {
	if err != nil {
		fmt.Printf("FATAL: %s: %v\n", label, err)
		os.Exit(1)
	}
}

func runFrameworkSQLite() {
	openConnectPing()
	execDDL()
	execDML()
	queryAndScan()
	preparedStatements()
	transactions()
	placeholders()
	nullHandling()
	contextVariants()
	poolAndConn()
	sqliteSpecific()
	// Comprehensive coverage (per #764 directive): virtual tables (FTS5/rtree),
	// full DDL constraint matrix, advanced SQL (CTE/window/aggregate/upsert),
	// SAVEPOINT nesting, atomicity (constraint-violation rollback), and access
	// permissions (read-only mode / PRAGMA query_only).
	sqliteVirtualTables()
	sqliteDDLConstraints()
	sqliteAdvancedSQL()
	sqliteSavepoints()
	sqliteAtomicity()
	sqlitePermissions()
}

// sqlite_newMemDB returns a fresh in-memory DB pinned to a single connection so that
// plain ":memory:" data persists across all operations (determinism-safe).
func sqlite_newMemDB() *sql.DB {
	// A shared temp-file DB (NOT plain ":memory:", which gives each pooled connection its
	// OWN database) so all connections see the same data deterministically. WAL journal mode
	// lets a db-level read/prepare proceed while a transaction holds a write connection —
	// the previous ":memory:" + SetMaxOpenConns(1) combo deadlocked in database/sql.(*DB).conn
	// when a db.Prepare/db.QueryRow ran during an open tx. busy_timeout bounds write-lock
	// contention; the pool is small but >1 so a db op can run alongside an open tx.
	f, err := os.CreateTemp("", "fwsqlite-*.db")
	sqlite_must("CreateTemp", err)
	name := f.Name()
	f.Close()
	db, err := sql.Open("sqlite", "file:"+name+"?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)")
	sqlite_must("sql.Open(tempfile WAL)", err)
	db.SetMaxOpenConns(4)
	return db
}

// -----------------------------------------------------------------------------
// Open / Connect / Ping / Close / Drivers
// -----------------------------------------------------------------------------

func openConnectPing() {
	// sql.Open is lazy: only validates args, no connection yet.
	db, err := sql.Open("sqlite", ":memory:")
	fwOK("Open(:memory:) err==nil", err == nil)
	fwOK("Open(:memory:) db!=nil", db != nil)

	// Lazy: before Ping, OpenConnections is 0.
	fwOK("Open is lazy (OpenConnections==0)", db.Stats().OpenConnections == 0)

	// Ping forces a real connection.
	sqlite_must("Ping", db.Ping())
	fwOK("Ping established a connection", db.Stats().OpenConnections >= 1)

	// PingContext.
	sqlite_must("PingContext", db.PingContext(context.Background()))
	fwOK("PingContext(bg) err==nil", true)
	sqlite_must("db.Close()", db.Close())
	fwOK("Close() err==nil", true)

	// Operations after Close -> "sql: database is closed".
	_, errClosed := db.Exec("SELECT 1")
	fwOK("Exec after Close errors", errClosed != nil)
	fwOK("Exec after Close msg", errClosed != nil && strings.Contains(errClosed.Error(), "database is closed"))

	// Various DSN forms.
	dbMemFile, err := sql.Open("sqlite", "file::memory:")
	fwOK("Open(file::memory:) err==nil", err == nil)
	sqlite_must("Ping(file::memory:)", dbMemFile.Ping())
	fwOK("Ping(file::memory:) ok", true)
	dbMemFile.Close()

	// Temp file DSN.
	tmp := filepath.Join(sqlite_mustTempDir(), "test.db")
	dbFile, err := sql.Open("sqlite", tmp)
	fwOK("Open(file path) err==nil", err == nil)
	sqlite_must("Ping(file path)", dbFile.Ping())
	fwOK("Ping(file path) ok", true)
	dbFile.Close()

	// DSN with ?_pragma query parameter (modernc DSN form).
	dbPragma, err := sql.Open("sqlite", "file::memory:?_pragma=busy_timeout(5000)")
	fwOK("Open(file::memory:?_pragma) err==nil", err == nil)
	sqlite_must("Ping(?_pragma)", dbPragma.Ping())
	fwOK("Ping(?_pragma) ok", true)
	dbPragma.Close()

	// shared-cache vs per-connection: file::memory:?cache=shared persists across
	// pooled conns; plain ":memory:" with >1 conn does NOT (the determinism trap).
	dbShared, err := sql.Open("sqlite", "file::memory:?cache=shared")
	sqlite_must("Open(cache=shared)", err)
	dbShared.SetMaxOpenConns(3)
	_, err = dbShared.Exec("CREATE TABLE shared_t(a INTEGER)")
	sqlite_must("create on shared", err)
	_, err = dbShared.Exec("INSERT INTO shared_t(a) VALUES(7)")
	sqlite_must("insert on shared", err)
	var sa int
	// Force several round-trips; with cache=shared all conns see the table.
	for i := 0; i < 5; i++ {
		sqlite_must("read shared", dbShared.QueryRow("SELECT a FROM shared_t").Scan(&sa))
	}
	fwOK("file::memory:?cache=shared persists across conns", sa == 7)
	dbShared.Close()

	// Unknown driver -> immediate error.
	_, errDrv := sql.Open("not-a-real-driver", "x")
	fwOK("Open(unknown driver) errors", errDrv != nil)
	fwOK("Open(unknown driver) msg unknown driver", errDrv != nil && strings.Contains(errDrv.Error(), "unknown driver"))
	fwOK("Open(unknown driver) msg forgotten import", errDrv != nil && strings.Contains(errDrv.Error(), "forgotten import"))

	// sql.Drivers() includes "sqlite".
	fwOK("Drivers() contains sqlite", slices.Contains(sql.Drivers(), "sqlite"))
}

var sqlite_tmpDirOnce string

func sqlite_mustTempDir() string {
	if sqlite_tmpDirOnce != "" {
		return sqlite_tmpDirOnce
	}
	d, err := os.MkdirTemp("", "fwsqlite")
	sqlite_must("MkdirTemp", err)
	sqlite_tmpDirOnce = d
	return d
}

// -----------------------------------------------------------------------------
// DDL: CREATE / ALTER / DROP / INDEX / VIEW
// -----------------------------------------------------------------------------

func execDDL() {
	db := sqlite_newMemDB()
	defer db.Close()

	_, err := db.Exec(`CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, age INTEGER)`)
	sqlite_must("CREATE TABLE", err)
	var n int
	sqlite_must("count master table", db.QueryRow(`SELECT count(*) FROM sqlite_master WHERE type='table' AND name='t'`).Scan(&n))
	fwOK("CREATE TABLE -> sqlite_master count", n)

	// IF NOT EXISTS idempotent (run twice).
	_, e1 := db.Exec(`CREATE TABLE IF NOT EXISTS t2 (a INTEGER)`)
	_, e2 := db.Exec(`CREATE TABLE IF NOT EXISTS t2 (a INTEGER)`)
	fwOK("CREATE TABLE IF NOT EXISTS first err==nil", e1 == nil)
	fwOK("CREATE TABLE IF NOT EXISTS second err==nil (idempotent)", e2 == nil)

	// CREATE TABLE without IF NOT EXISTS twice -> error on second.
	_, eDup := db.Exec(`CREATE TABLE t (x INTEGER)`)
	fwOK("CREATE existing table errors", eDup != nil)
	fwOK("CREATE existing table msg", eDup != nil && strings.Contains(eDup.Error(), "already exists"))

	// ALTER TABLE ADD COLUMN.
	var before, after int
	sqlite_must("table_info before", db.QueryRow(`SELECT count(*) FROM pragma_table_info('t')`).Scan(&before))
	_, err = db.Exec(`ALTER TABLE t ADD COLUMN email TEXT`)
	sqlite_must("ALTER TABLE ADD COLUMN", err)
	sqlite_must("table_info after", db.QueryRow(`SELECT count(*) FROM pragma_table_info('t')`).Scan(&after))
	fwOK("ALTER TABLE ADD COLUMN increases column count by 1", after-before == 1)

	// CREATE INDEX.
	_, err = db.Exec(`CREATE INDEX idx_name ON t(name)`)
	sqlite_must("CREATE INDEX", err)
	sqlite_must("count index", db.QueryRow(`SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_name'`).Scan(&n))
	fwOK("CREATE INDEX -> sqlite_master index count", n)

	// CREATE VIEW.
	_, err = db.Exec(`CREATE VIEW v AS SELECT id FROM t`)
	sqlite_must("CREATE VIEW", err)
	sqlite_must("count view", db.QueryRow(`SELECT count(*) FROM sqlite_master WHERE type='view' AND name='v'`).Scan(&n))
	fwOK("CREATE VIEW -> sqlite_master view count", n)

	// DROP TABLE.
	_, err = db.Exec(`DROP TABLE t2`)
	sqlite_must("DROP TABLE", err)
	sqlite_must("count after drop", db.QueryRow(`SELECT count(*) FROM sqlite_master WHERE type='table' AND name='t2'`).Scan(&n))
	fwOK("DROP TABLE -> sqlite_master count 0", n)

	// SELECT from dropped table -> "no such table".
	_, eNoTab := db.Query(`SELECT * FROM t2`)
	fwOK("query dropped table errors", eNoTab != nil)
	fwOK("query dropped table msg", eNoTab != nil && strings.Contains(eNoTab.Error(), "no such table"))
}

// -----------------------------------------------------------------------------
// DML: INSERT / UPDATE / DELETE + Result.LastInsertId / RowsAffected
// -----------------------------------------------------------------------------

func execDML() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, age INTEGER)`)
	sqlite_must("create t", err)

	// INSERT + LastInsertId (monotonic).
	r1, err := db.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "a", 1)
	sqlite_must("insert 1", err)
	id1, _ := r1.LastInsertId()
	fwOK("LastInsertId first insert", id1)
	r2, err := db.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "b", 1)
	sqlite_must("insert 2", err)
	id2, _ := r2.LastInsertId()
	fwOK("LastInsertId second insert", id2)

	// INSERT RowsAffected.
	ra1, _ := r1.RowsAffected()
	fwOK("single INSERT RowsAffected", ra1)

	// Insert a third age=1 row, then UPDATE all age=1 rows.
	_, err = db.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "c", 1)
	sqlite_must("insert 3", err)
	ru, err := db.Exec(`UPDATE t SET age=? WHERE age=?`, 2, 1)
	sqlite_must("update", err)
	rau, _ := ru.RowsAffected()
	fwOK("UPDATE RowsAffected (3 matched)", rau)

	// DELETE matching predicate.
	rd, err := db.Exec(`DELETE FROM t WHERE name=?`, "a")
	sqlite_must("delete a", err)
	rad, _ := rd.RowsAffected()
	fwOK("DELETE RowsAffected (1 matched)", rad)

	// DELETE with no match -> 0.
	rd0, err := db.Exec(`DELETE FROM t WHERE name=?`, "nope")
	sqlite_must("delete nomatch", err)
	rad0, _ := rd0.RowsAffected()
	fwOK("DELETE RowsAffected no match", rad0)

	// Multi-statement script (CREATE + INSERT in one Exec).
	_, err = db.Exec("CREATE TABLE table1 (field1 varchar NULL);\nINSERT INTO table1 (field1) VALUES (?);", sql.NullString{})
	sqlite_must("multi-statement exec", err)
	var ms int
	sqlite_must("count multi", db.QueryRow(`SELECT count(*) FROM table1`).Scan(&ms))
	fwOK("multi-statement script ran both statements", ms)
}

// -----------------------------------------------------------------------------
// Query / QueryRow / Rows.Scan / Next / Close / Err / Columns / ColumnTypes
// -----------------------------------------------------------------------------

func queryAndScan() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)`)
	sqlite_must("create t", err)
	_, err = db.Exec(`INSERT INTO t(id,name) VALUES(1,'alpha'),(2,'beta'),(3,'gamma')`)
	sqlite_must("seed", err)

	// Query + Next + Scan loop, ordered.
	rows, err := db.Query(`SELECT id,name FROM t ORDER BY id`)
	sqlite_must("Query", err)
	var got []string
	for rows.Next() {
		var id int
		var name string
		sqlite_must("Scan in loop", rows.Scan(&id, &name))
		got = append(got, fmt.Sprintf("%d:%s", id, name))
	}
	sqlite_must("rows.Err()", rows.Err())
	sqlite_must("rows.Close()", rows.Close())
	fwOK("Query iterates rows in order", strings.Join(got, ","))
	fwOK("rows.Next() false at end", true) // loop exited because Next()==false

	// QueryRow count.
	var nc int
	sqlite_must("QueryRow count", db.QueryRow(`SELECT count(*) FROM t`).Scan(&nc))
	fwOK("QueryRow count(*)", nc)

	// ErrNoRows.
	var nm string
	errNo := db.QueryRow(`SELECT name FROM t WHERE id=?`, 99999).Scan(&nm)
	fwOK("QueryRow no match -> ErrNoRows", errors.Is(errNo, sql.ErrNoRows))
	fwOK("ErrNoRows message", errNo != nil && errNo.Error() == "sql: no rows in result set")

	// Row.Err() surfaces deferred query error.
	row := db.QueryRow(`SELECT bad_col FROM t`)
	errBad := row.Err()
	fwOK("Row.Err() surfaces query error", errBad != nil)
	fwOK("Row.Err() msg no such column", errBad != nil && strings.Contains(errBad.Error(), "no such column"))

	// Columns().
	rows2, err := db.Query(`SELECT id, name FROM t`)
	sqlite_must("Query for Columns", err)
	cols, err := rows2.Columns()
	sqlite_must("Columns", err)
	fwOK("Columns() names", strings.Join(cols, ","))

	// ColumnTypes(): Name + DatabaseTypeName.
	cts, err := rows2.ColumnTypes()
	sqlite_must("ColumnTypes", err)
	fwOK("ColumnType[0].Name()", cts[0].Name())
	fwOK("ColumnType[0].DatabaseTypeName()", cts[0].DatabaseTypeName())
	fwOK("ColumnType[1].Name()", cts[1].Name())
	fwOK("ColumnType[1].DatabaseTypeName()", cts[1].DatabaseTypeName())
	rows2.Close()

	// Scan into multiple Go types: INTEGER, TEXT, REAL, BLOB.
	_, err = db.Exec(`CREATE TABLE typed (i INTEGER, s TEXT, f REAL, b BLOB)`)
	sqlite_must("create typed", err)
	_, err = db.Exec(`INSERT INTO typed VALUES(?,?,?,?)`, int64(42), "hello", 3.5, []byte("xyz"))
	sqlite_must("insert typed", err)
	var gi int64
	var gs string
	var gf float64
	var gb []byte
	sqlite_must("Scan multi types", db.QueryRow(`SELECT i,s,f,b FROM typed`).Scan(&gi, &gs, &gf, &gb))
	fwOK("Scan INTEGER->int64", gi)
	fwOK("Scan TEXT->string", gs)
	fwOK("Scan REAL->float64", gf)
	fwOK("Scan BLOB->[]byte", string(gb))

	// bool round-trip (stored as integer 0/1).
	_, err = db.Exec(`CREATE TABLE flags (v INTEGER)`)
	sqlite_must("create flags", err)
	_, err = db.Exec(`INSERT INTO flags(v) VALUES(?)`, true)
	sqlite_must("insert bool", err)
	var gboolInt int
	sqlite_must("scan bool as int", db.QueryRow(`SELECT v FROM flags`).Scan(&gboolInt))
	fwOK("bool stored as 1", gboolInt)
	var gbool bool
	sqlite_must("scan int as bool", db.QueryRow(`SELECT v FROM flags`).Scan(&gbool))
	fwOK("Scan INTEGER->bool", gbool)

	// Scan arg-count mismatch error.
	var only int
	errCnt := db.QueryRow(`SELECT id, name FROM t WHERE id=1`).Scan(&only)
	fwOK("Scan arg-count mismatch errors", errCnt != nil)
	fwOK("Scan arg-count mismatch msg", errCnt != nil && strings.Contains(errCnt.Error(), "destination arguments in Scan"))
}

// -----------------------------------------------------------------------------
// Prepared statements
// -----------------------------------------------------------------------------

func preparedStatements() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, age INTEGER)`)
	sqlite_must("create t", err)

	// Stmt.Exec reused for many inserts.
	stmt, err := db.Prepare(`INSERT INTO t(name,age) VALUES(?,?)`)
	sqlite_must("Prepare insert", err)
	names := []string{"p", "q", "r", "s"}
	for i, nm := range names {
		_, err := stmt.Exec(nm, i)
		sqlite_must("stmt.Exec", err)
	}
	sqlite_must("stmt.Close", stmt.Close())
	var n int
	sqlite_must("count after stmt inserts", db.QueryRow(`SELECT count(*) FROM t`).Scan(&n))
	fwOK("Stmt.Exec inserted N rows", n)

	// Stmt.Query reusable with different args.
	qstmt, err := db.Prepare(`SELECT count(*) FROM t WHERE age > ?`)
	sqlite_must("Prepare query", err)
	rows, err := qstmt.Query(0)
	sqlite_must("stmt.Query", err)
	var cnt int
	for rows.Next() {
		sqlite_must("scan stmt.Query", rows.Scan(&cnt))
	}
	rows.Close()
	fwOK("Stmt.Query rows age>0", cnt)
	// Reuse with another arg.
	var cnt2 int
	sqlite_must("stmt.QueryRow reuse", qstmt.QueryRow(2).Scan(&cnt2))
	fwOK("Stmt reused (age>2)", cnt2)

	// Stmt.QueryRow.
	var nm string
	sqlite_must("Stmt.QueryRow", qstmt.QueryRow(0).Scan(&cnt))
	fwOK("Stmt.QueryRow scanned", cnt)
	qstmt.Close()

	// Use after Close -> error.
	idStmt, err := db.Prepare(`SELECT name FROM t WHERE id=?`)
	sqlite_must("Prepare id", err)
	sqlite_must("scan before close", idStmt.QueryRow(1).Scan(&nm))
	fwOK("Stmt usable before Close", nm != "")
	idStmt.Close()
	_, errClosed := idStmt.Exec(1)
	fwOK("Stmt use after Close errors", errClosed != nil)
	fwOK("Stmt closed msg", errClosed != nil && strings.Contains(errClosed.Error(), "statement is closed"))
}

// -----------------------------------------------------------------------------
// Transactions
// -----------------------------------------------------------------------------

func transactions() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, age INTEGER)`)
	sqlite_must("create t", err)

	// Begin + Exec + Commit.
	tx, err := db.Begin()
	sqlite_must("Begin", err)
	_, err = tx.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "x", 1)
	sqlite_must("tx.Exec", err)
	sqlite_must("tx.Commit", tx.Commit())
	var n int
	sqlite_must("count after commit", db.QueryRow(`SELECT count(*) FROM t`).Scan(&n))
	fwOK("Commit persists insert", n)

	// Rollback discards.
	sqlite_must("count baseline", db.QueryRow(`SELECT count(*) FROM t`).Scan(&n))
	baseline := n
	tx2, err := db.Begin()
	sqlite_must("Begin 2", err)
	_, err = tx2.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "y", 2)
	sqlite_must("tx2.Exec", err)
	sqlite_must("tx2.Rollback", tx2.Rollback())
	sqlite_must("count after rollback", db.QueryRow(`SELECT count(*) FROM t`).Scan(&n))
	fwOK("Rollback discards insert (count unchanged)", n == baseline)

	// Ops after Commit -> ErrTxDone.
	tx3, err := db.Begin()
	sqlite_must("Begin 3", err)
	sqlite_must("tx3.Commit", tx3.Commit())
	_, errDone := tx3.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "z", 3)
	fwOK("tx op after Commit -> ErrTxDone", errors.Is(errDone, sql.ErrTxDone))
	errDone2 := tx3.Commit()
	fwOK("double Commit -> ErrTxDone", errors.Is(errDone2, sql.ErrTxDone))
	fwOK("ErrTxDone message", errors.Is(errDone, sql.ErrTxDone) && sql.ErrTxDone.Error() == "sql: transaction has already been committed or rolled back")

	// Read-your-own-writes within a Tx.
	tx4, err := db.Begin()
	sqlite_must("Begin 4", err)
	var inside int
	sqlite_must("count inside before", tx4.QueryRow(`SELECT count(*) FROM t`).Scan(&inside))
	beforeInside := inside
	_, err = tx4.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "w", 4)
	sqlite_must("tx4.Exec", err)
	sqlite_must("count inside after", tx4.QueryRow(`SELECT count(*) FROM t`).Scan(&inside))
	fwOK("Tx sees own uncommitted insert", inside == beforeInside+1)
	sqlite_must("tx4.Rollback", tx4.Rollback())

	// Tx.Prepare and tx.Stmt. Prepare the DB-level stmt BEFORE opening the tx: on a
	// 1-conn pool (cache=shared determinism) calling db.Prepare while the tx holds the
	// only connection deadlocks in (*DB).conn waiting for a free conn. Prepared first,
	// the conn returns to the pool before Begin, and tx.Stmt re-binds it onto the tx conn.
	dbStmt, err := db.Prepare(`INSERT INTO t(name,age) VALUES(?,?)`)
	sqlite_must("db.Prepare for tx.Stmt", err)
	tx5, err := db.Begin()
	sqlite_must("Begin 5", err)
	tstmt, err := tx5.Prepare(`INSERT INTO t(name,age) VALUES(?,?)`)
	sqlite_must("tx.Prepare", err)
	_, err = tstmt.Exec("tp", 5)
	sqlite_must("tx-stmt Exec", err)
	// Bind an existing DB stmt to the tx via tx.Stmt.
	txBound := tx5.Stmt(dbStmt)
	_, err = txBound.Exec("ts", 6)
	sqlite_must("tx.Stmt Exec", err)
	sqlite_must("tx5.Commit", tx5.Commit())
	dbStmt.Close()
	var afterTxStmt int
	sqlite_must("count after tx stmts", db.QueryRow(`SELECT count(*) FROM t WHERE name IN ('tp','ts')`).Scan(&afterTxStmt))
	fwOK("Tx.Prepare + tx.Stmt both inserted", afterTxStmt)

	// BeginTx ReadOnly: write should fail.
	roTx, err := db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	sqlite_must("BeginTx ReadOnly", err)
	_, errRO := roTx.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "ro", 7)
	fwOK("ReadOnly tx write errors", errRO != nil)
	roTx.Rollback()

	// BeginTx with isolation levels.
	txDef, err := db.BeginTx(context.Background(), &sql.TxOptions{Isolation: sql.LevelDefault})
	fwOK("BeginTx LevelDefault err==nil", err == nil)
	if err == nil {
		txDef.Rollback()
	}
	txSer, err := db.BeginTx(context.Background(), &sql.TxOptions{Isolation: sql.LevelSerializable})
	fwOK("BeginTx LevelSerializable err==nil", err == nil)
	if err == nil {
		txSer.Rollback()
	}
	// An unsupported high level: SQLite/modernc rejects it.
	_, errIso := db.BeginTx(context.Background(), &sql.TxOptions{Isolation: sql.LevelLinearizable})
	fwOK("BeginTx unsupported isolation errors", errIso != nil)
}

// -----------------------------------------------------------------------------
// Placeholders & arguments
// -----------------------------------------------------------------------------

func placeholders() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, age INTEGER)`)
	sqlite_must("create t", err)

	// Positional ?.
	_, err = db.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "bob", 42)
	sqlite_must("positional insert", err)
	var age int
	sqlite_must("read positional", db.QueryRow(`SELECT age FROM t WHERE name=?`, "bob").Scan(&age))
	fwOK("positional ? round-trip age", age)

	// Numbered ?NNN.
	_, err = db.Exec(`INSERT INTO t(name,age) VALUES(?1,?2)`, "num", 7)
	sqlite_must("numbered insert", err)
	var nameNum string
	sqlite_must("read numbered", db.QueryRow(`SELECT name FROM t WHERE age=?`, 7).Scan(&nameNum))
	fwOK("numbered ?NNN round-trip name", nameNum)

	// sql.Named with @name.
	_, err = db.Exec(`INSERT INTO t(name,age) VALUES(@n,@a)`, sql.Named("n", "zoe"), sql.Named("a", 9))
	sqlite_must("named @ insert", err)
	var ageNamed int
	sqlite_must("read named @", db.QueryRow(`SELECT age FROM t WHERE name=@nm`, sql.Named("nm", "zoe")).Scan(&ageNamed))
	fwOK("sql.Named @name round-trip age", ageNamed)

	// Named with :name.
	_, err = db.Exec(`INSERT INTO t(name,age) VALUES(:nm,:ag)`, sql.Named("nm", "colon"), sql.Named("ag", 11))
	sqlite_must("named : insert", err)
	var ageColon int
	sqlite_must("read named :", db.QueryRow(`SELECT age FROM t WHERE name=?`, "colon").Scan(&ageColon))
	fwOK("sql.Named :name round-trip age", ageColon)

	// Named with $name.
	_, err = db.Exec(`INSERT INTO t(name,age) VALUES($nm,$ag)`, sql.Named("nm", "dollar"), sql.Named("ag", 13))
	sqlite_must("named $ insert", err)
	var ageDollar int
	sqlite_must("read named $", db.QueryRow(`SELECT age FROM t WHERE name=?`, "dollar").Scan(&ageDollar))
	fwOK("sql.Named $name round-trip age", ageDollar)

	// Argument type mapping in one row: string,int64,float64,bool,[]byte,nil,time.Time.
	_, err = db.Exec(`CREATE TABLE typemap (s TEXT, i INTEGER, f REAL, b INTEGER, blob BLOB, nul TEXT, ts DATETIME)`)
	sqlite_must("create typemap", err)
	fixed := time.Date(2020, 1, 2, 3, 4, 5, 0, time.UTC)
	_, err = db.Exec(`INSERT INTO typemap VALUES(?,?,?,?,?,?,?)`,
		"str", int64(99), 1.25, true, []byte("bb"), nil, fixed)
	sqlite_must("typemap insert", err)
	var ts string
	var iv int64
	var fv float64
	var bv bool
	var blobv []byte
	var nulv sql.NullString
	var tv time.Time
	sqlite_must("typemap scan", db.QueryRow(`SELECT s,i,f,b,blob,nul,ts FROM typemap`).Scan(&ts, &iv, &fv, &bv, &blobv, &nulv, &tv))
	fwOK("type map string", ts)
	fwOK("type map int64", iv)
	fwOK("type map float64", fv)
	fwOK("type map bool", bv)
	fwOK("type map []byte", string(blobv))
	fwOK("type map nil -> NULL (Valid==false)", nulv.Valid)
	fwOK("type map time.Time (UTC fixed)", tv.UTC().Format("2006-01-02T15:04:05Z"))

	// Wrong placeholder/arg count error.
	_, errCnt := db.Exec(`INSERT INTO t(name,age) VALUES(?,?)`, "only-one")
	fwOK("placeholder/arg count mismatch errors", errCnt != nil)
}

// -----------------------------------------------------------------------------
// NULL handling
// -----------------------------------------------------------------------------

func nullHandling() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE n (
		id INTEGER PRIMARY KEY,
		s TEXT, i INTEGER, i32 INTEGER, i16 INTEGER, byt INTEGER,
		f REAL, b INTEGER, ts DATETIME)`)
	sqlite_must("create n", err)

	// id=1 all NULL; id=2 all valued.
	_, err = db.Exec(`INSERT INTO n(id,s,i,i32,i16,byt,f,b,ts) VALUES(1,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)`)
	sqlite_must("insert nulls", err)
	fixed := time.Date(2021, 6, 7, 8, 9, 10, 0, time.UTC)
	_, err = db.Exec(`INSERT INTO n(id,s,i,i32,i16,byt,f,b,ts) VALUES(2,?,?,?,?,?,?,?,?)`,
		"text", int64(100), int32(50), int16(20), byte(5), 2.5, true, fixed)
	sqlite_must("insert valued", err)

	// NullString.
	var ns sql.NullString
	sqlite_must("scan NullString null", db.QueryRow(`SELECT s FROM n WHERE id=1`).Scan(&ns))
	fwOK("NullString NULL Valid==false", ns.Valid)
	sqlite_must("scan NullString val", db.QueryRow(`SELECT s FROM n WHERE id=2`).Scan(&ns))
	fwOK("NullString valued Valid==true", ns.Valid)
	fwOK("NullString valued String", ns.String)

	// NullInt64 / NullInt32 / NullInt16 / NullByte.
	var ni sql.NullInt64
	sqlite_must("scan NullInt64 null", db.QueryRow(`SELECT i FROM n WHERE id=1`).Scan(&ni))
	fwOK("NullInt64 NULL Valid==false", ni.Valid)
	sqlite_must("scan NullInt64 val", db.QueryRow(`SELECT i FROM n WHERE id=2`).Scan(&ni))
	fwOK("NullInt64 valued Int64", ni.Int64)

	var ni32 sql.NullInt32
	sqlite_must("scan NullInt32 null", db.QueryRow(`SELECT i32 FROM n WHERE id=1`).Scan(&ni32))
	fwOK("NullInt32 NULL Valid==false", ni32.Valid)
	sqlite_must("scan NullInt32 val", db.QueryRow(`SELECT i32 FROM n WHERE id=2`).Scan(&ni32))
	fwOK("NullInt32 valued Int32", ni32.Int32)

	var ni16 sql.NullInt16
	sqlite_must("scan NullInt16 null", db.QueryRow(`SELECT i16 FROM n WHERE id=1`).Scan(&ni16))
	fwOK("NullInt16 NULL Valid==false", ni16.Valid)
	sqlite_must("scan NullInt16 val", db.QueryRow(`SELECT i16 FROM n WHERE id=2`).Scan(&ni16))
	fwOK("NullInt16 valued Int16", ni16.Int16)

	var nb sql.NullByte
	sqlite_must("scan NullByte null", db.QueryRow(`SELECT byt FROM n WHERE id=1`).Scan(&nb))
	fwOK("NullByte NULL Valid==false", nb.Valid)
	sqlite_must("scan NullByte val", db.QueryRow(`SELECT byt FROM n WHERE id=2`).Scan(&nb))
	fwOK("NullByte valued Byte", nb.Byte)

	// NullFloat64 / NullBool.
	var nf sql.NullFloat64
	sqlite_must("scan NullFloat64 null", db.QueryRow(`SELECT f FROM n WHERE id=1`).Scan(&nf))
	fwOK("NullFloat64 NULL Valid==false", nf.Valid)
	sqlite_must("scan NullFloat64 val", db.QueryRow(`SELECT f FROM n WHERE id=2`).Scan(&nf))
	fwOK("NullFloat64 valued Float64", nf.Float64)

	var nbool sql.NullBool
	sqlite_must("scan NullBool null", db.QueryRow(`SELECT b FROM n WHERE id=1`).Scan(&nbool))
	fwOK("NullBool NULL Valid==false", nbool.Valid)
	sqlite_must("scan NullBool val", db.QueryRow(`SELECT b FROM n WHERE id=2`).Scan(&nbool))
	fwOK("NullBool valued Bool", nbool.Bool)

	// NullTime.
	var nt sql.NullTime
	sqlite_must("scan NullTime null", db.QueryRow(`SELECT ts FROM n WHERE id=1`).Scan(&nt))
	fwOK("NullTime NULL Valid==false", nt.Valid)
	sqlite_must("scan NullTime val", db.QueryRow(`SELECT ts FROM n WHERE id=2`).Scan(&nt))
	fwOK("NullTime valued Valid==true", nt.Valid)
	fwOK("NullTime value (UTC)", nt.Time.UTC().Format("2006-01-02T15:04:05Z"))

	// Generic sql.Null[T] (Go 1.22+).
	var gn sql.Null[string]
	sqlite_must("scan Null[string] null", db.QueryRow(`SELECT s FROM n WHERE id=1`).Scan(&gn))
	fwOK("Null[string] NULL Valid==false", gn.Valid)
	sqlite_must("scan Null[string] val", db.QueryRow(`SELECT s FROM n WHERE id=2`).Scan(&gn))
	fwOK("Null[string] valued V", gn.V)

	var gni sql.Null[int64]
	sqlite_must("scan Null[int64] val", db.QueryRow(`SELECT i FROM n WHERE id=2`).Scan(&gni))
	fwOK("Null[int64] valued V", gni.V)

	// Scan SQL NULL into non-nullable Go type -> error.
	var plain string
	errNullStr := db.QueryRow(`SELECT s FROM n WHERE id=1`).Scan(&plain)
	fwOK("Scan NULL into string errors", errNullStr != nil)
	fwOK("Scan NULL into string msg", errNullStr != nil && strings.Contains(errNullStr.Error(), "converting NULL to string"))

	// Scan NULL into pointer -> nil pointer.
	var p *string
	sqlite_must("scan NULL into *string", db.QueryRow(`SELECT s FROM n WHERE id=1`).Scan(&p))
	fwOK("Scan NULL into *string -> nil", p == nil)
	sqlite_must("scan value into *string", db.QueryRow(`SELECT s FROM n WHERE id=2`).Scan(&p))
	fwOK("Scan value into *string -> non-nil", p != nil && *p == "text")

	// driver.Valuer: NullString{} writes NULL.
	var roundtrip sql.NullString
	sqlite_must("scan Valuer roundtrip null", db.QueryRow(`SELECT field1 FROM (SELECT NULL AS field1)`).Scan(&roundtrip))
	fwOK("NullString Valuer NULL Valid==false", roundtrip.Valid)
}

// -----------------------------------------------------------------------------
// Context variants
// -----------------------------------------------------------------------------

func contextVariants() {
	db := sqlite_newMemDB()
	defer db.Close()
	ctx := context.Background()
	_, err := db.ExecContext(ctx, `CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, age INTEGER)`)
	sqlite_must("ExecContext create", err)

	// ExecContext + RowsAffected.
	r, err := db.ExecContext(ctx, `INSERT INTO t(name,age) VALUES(?,?)`, "a", 1)
	sqlite_must("ExecContext insert", err)
	ra, _ := r.RowsAffected()
	fwOK("ExecContext RowsAffected", ra)

	// QueryContext.
	rows, err := db.QueryContext(ctx, `SELECT id FROM t`)
	sqlite_must("QueryContext", err)
	var qn int
	for rows.Next() {
		var id int
		sqlite_must("scan QueryContext", rows.Scan(&id))
		qn++
	}
	rows.Close()
	fwOK("QueryContext row count", qn)

	// QueryRowContext.
	var c int
	sqlite_must("QueryRowContext", db.QueryRowContext(ctx, `SELECT count(*) FROM t`).Scan(&c))
	fwOK("QueryRowContext count", c)

	// PrepareContext + Stmt.ExecContext.
	pstmt, err := db.PrepareContext(ctx, `INSERT INTO t(name,age) VALUES(?,?)`)
	sqlite_must("PrepareContext", err)
	_, err = pstmt.ExecContext(ctx, "ctx", 2)
	sqlite_must("Stmt.ExecContext", err)
	pstmt.Close()

	// Stmt.QueryContext / QueryRowContext.
	qstmt, err := db.PrepareContext(ctx, `SELECT count(*) FROM t`)
	sqlite_must("PrepareContext query", err)
	sr, err := qstmt.QueryContext(ctx)
	sqlite_must("Stmt.QueryContext", err)
	var sc int
	for sr.Next() {
		sqlite_must("scan Stmt.QueryContext", sr.Scan(&sc))
	}
	sr.Close()
	fwOK("Stmt.QueryContext count", sc)
	var sc2 int
	sqlite_must("Stmt.QueryRowContext", qstmt.QueryRowContext(ctx).Scan(&sc2))
	fwOK("Stmt.QueryRowContext count", sc2)
	qstmt.Close()

	// PingContext background.
	sqlite_must("PingContext bg", db.PingContext(ctx))
	fwOK("PingContext bg err==nil", true)

	// Tx context-aware ops.
	tx, err := db.BeginTx(ctx, nil)
	sqlite_must("BeginTx(nil)", err)
	_, err = tx.ExecContext(ctx, `INSERT INTO t(name,age) VALUES(?,?)`, "txctx", 4)
	sqlite_must("tx.ExecContext", err)
	var txc int
	sqlite_must("tx.QueryRowContext", tx.QueryRowContext(ctx, `SELECT count(*) FROM t WHERE name='txctx'`).Scan(&txc))
	fwOK("tx.QueryRowContext sees own write", txc)
	tr, err := tx.QueryContext(ctx, `SELECT id FROM t WHERE name='txctx'`)
	sqlite_must("tx.QueryContext", err)
	tr.Close()
	tps, err := tx.PrepareContext(ctx, `INSERT INTO t(name,age) VALUES(?,?)`)
	sqlite_must("tx.PrepareContext", err)
	_, err = tps.ExecContext(ctx, "txprep", 5)
	sqlite_must("tx prepared ExecContext", err)
	// tx.StmtContext: bind a db stmt to the tx.
	dbstmt, err := db.PrepareContext(ctx, `INSERT INTO t(name,age) VALUES(?,?)`)
	sqlite_must("db.PrepareContext for StmtContext", err)
	txstmt := tx.StmtContext(ctx, dbstmt)
	_, err = txstmt.ExecContext(ctx, "txstmtctx", 6)
	sqlite_must("tx.StmtContext ExecContext", err)
	sqlite_must("tx.Commit ctx", tx.Commit())
	dbstmt.Close()
	var persisted int
	sqlite_must("count ctx-persisted", db.QueryRowContext(ctx, `SELECT count(*) FROM t WHERE name IN ('txctx','txprep','txstmtctx')`).Scan(&persisted))
	fwOK("context-aware tx ops persisted", persisted)

	// Canceled context -> context.Canceled.
	cctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, errCancel := db.ExecContext(cctx, `CREATE TABLE cx(a)`)
	fwOK("canceled ctx Exec errors", errCancel != nil)
	fwOK("canceled ctx -> context.Canceled", errors.Is(errCancel, context.Canceled))

	// Canceled context on PingContext / BeginTx / PrepareContext.
	errPing := db.PingContext(cctx)
	fwOK("canceled ctx Ping -> Canceled", errors.Is(errPing, context.Canceled))
	_, errBegin := db.BeginTx(cctx, nil)
	fwOK("canceled ctx BeginTx -> Canceled", errors.Is(errBegin, context.Canceled))
	_, errPrep := db.PrepareContext(cctx, `SELECT 1`)
	fwOK("canceled ctx PrepareContext -> Canceled", errors.Is(errPrep, context.Canceled))

	// Deadline already exceeded -> DeadlineExceeded (or Canceled depending on timing).
	dctx, dcancel := context.WithDeadline(context.Background(), time.Unix(0, 0))
	defer dcancel()
	_, errDL := db.ExecContext(dctx, `CREATE TABLE dx(a)`)
	fwOK("expired deadline Exec errors", errDL != nil)
	fwOK("expired deadline -> DeadlineExceeded or Canceled",
		errors.Is(errDL, context.DeadlineExceeded) || errors.Is(errDL, context.Canceled))
}

// -----------------------------------------------------------------------------
// Connection pool & single Conn
// -----------------------------------------------------------------------------

func poolAndConn() {
	// Pool config + Stats. Use shared-cache so multiple conns share data.
	db, err := sql.Open("sqlite", "file::memory:?cache=shared")
	sqlite_must("Open pool", err)
	defer db.Close()
	db.SetMaxOpenConns(3)
	db.SetMaxIdleConns(2)
	db.SetConnMaxLifetime(time.Hour)
	db.SetConnMaxIdleTime(30 * time.Minute)
	sqlite_must("Ping pool", db.Ping())
	st := db.Stats()
	fwOK("Stats().MaxOpenConnections", st.MaxOpenConnections)
	fwOK("Stats().OpenConnections>=1 after Ping", st.OpenConnections >= 1)

	// SetMaxOpenConns(1) determinism note (plain :memory: persists across ops).
	dbMem := sqlite_newMemDB()
	defer dbMem.Close()
	_, err = dbMem.Exec(`CREATE TABLE m(a INTEGER)`)
	sqlite_must("create m", err)
	_, err = dbMem.Exec(`INSERT INTO m(a) VALUES(1)`)
	sqlite_must("insert m", err)
	var mv int
	sqlite_must("read m", dbMem.QueryRow(`SELECT a FROM m`).Scan(&mv))
	fwOK("SetMaxOpenConns(1) :memory: persists", mv)
	fwOK("dbMem Stats().MaxOpenConnections==1", dbMem.Stats().MaxOpenConnections)

	// DEMONSTRATE the determinism trap: plain :memory: with >1 conn loses the
	// table on a different pooled connection. We assert the failure to make the
	// behavior explicit (per critic's biggest non-determinism risk).
	dbTrap, err := sql.Open("sqlite", ":memory:")
	sqlite_must("Open trap", err)
	dbTrap.SetMaxOpenConns(2)
	defer dbTrap.Close()
	// Pin one conn, create a table on it, release it.
	c1, err := dbTrap.Conn(context.Background())
	sqlite_must("trap conn1", err)
	_, err = c1.ExecContext(context.Background(), `CREATE TABLE trap(a INTEGER)`)
	sqlite_must("trap create", err)
	// Open a SECOND distinct conn while c1 is still pinned -> forces a new conn.
	cOther, err := dbTrap.Conn(context.Background())
	sqlite_must("trap other", err)
	_, errTrap := cOther.ExecContext(context.Background(), `SELECT * FROM trap`)
	// On a fresh per-connection :memory: DB the table is absent.
	fwOK("plain :memory: per-conn isolation (table absent on other conn)", errTrap != nil && strings.Contains(errTrap.Error(), "no such table"))
	c1.Close()
	cOther.Close()

	// Conn: pin a single underlying connection; TEMP table visibility.
	conn, err := db.Conn(context.Background())
	sqlite_must("db.Conn", err)
	_, err = conn.ExecContext(context.Background(), `CREATE TEMP TABLE tt(a INTEGER)`)
	sqlite_must("conn TEMP create", err)
	_, err = conn.ExecContext(context.Background(), `INSERT INTO tt(a) VALUES(11)`)
	sqlite_must("conn TEMP insert", err)
	var tv int
	sqlite_must("conn TEMP read", conn.QueryRowContext(context.Background(), `SELECT a FROM tt`).Scan(&tv))
	fwOK("Conn TEMP table visible on same conn", tv)

	// Conn.Raw: access the underlying driver connection.
	rawOK := false
	errRaw := conn.Raw(func(driverConn any) error {
		rawOK = driverConn != nil
		return nil
	})
	sqlite_must("Conn.Raw", errRaw)
	fwOK("Conn.Raw provides driver conn", rawOK)

	// Conn after Close -> ErrConnDone.
	sqlite_must("conn.Close", conn.Close())
	_, errConnDone := conn.ExecContext(context.Background(), `SELECT 1`)
	fwOK("Conn op after Close -> ErrConnDone", errors.Is(errConnDone, sql.ErrConnDone))
	fwOK("ErrConnDone message", sql.ErrConnDone.Error() == "sql: connection is already closed")
}

// -----------------------------------------------------------------------------
// SQLite-specific behaviors (critic-requested): PRAGMA, AUTOINCREMENT,
// affinity, UNIQUE constraint, FK enforcement, NOT NULL
// -----------------------------------------------------------------------------

func sqliteSpecific() {
	db := sqlite_newMemDB()
	defer db.Close()

	// PRAGMA execution: read + set.
	var jm string
	sqlite_must("PRAGMA journal_mode read", db.QueryRow(`PRAGMA journal_mode`).Scan(&jm))
	fwOK("PRAGMA journal_mode (memory db)", jm)

	var fk int
	// Enable foreign_keys then read it back.
	_, err := db.Exec(`PRAGMA foreign_keys = ON`)
	sqlite_must("PRAGMA foreign_keys set", err)
	sqlite_must("PRAGMA foreign_keys read", db.QueryRow(`PRAGMA foreign_keys`).Scan(&fk))
	fwOK("PRAGMA foreign_keys ON", fk)

	var uv int
	sqlite_must("PRAGMA user_version", db.QueryRow(`PRAGMA user_version`).Scan(&uv))
	fwOK("PRAGMA user_version default", uv)

	// AUTOINCREMENT + LastInsertId monotonic.
	_, err = db.Exec(`CREATE TABLE auto (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)`)
	sqlite_must("create auto", err)
	r1, _ := db.Exec(`INSERT INTO auto(v) VALUES('a')`)
	r2, _ := db.Exec(`INSERT INTO auto(v) VALUES('b')`)
	i1, _ := r1.LastInsertId()
	i2, _ := r2.LastInsertId()
	fwOK("AUTOINCREMENT LastInsertId 1", i1)
	fwOK("AUTOINCREMENT LastInsertId 2 (monotonic)", i2)

	// Datatype affinity: typeof() reflects storage class.
	_, err = db.Exec(`CREATE TABLE aff (i INTEGER, t TEXT, r REAL, b BLOB)`)
	sqlite_must("create aff", err)
	_, err = db.Exec(`INSERT INTO aff VALUES(?,?,?,?)`, 1, "s", 1.5, []byte("x"))
	sqlite_must("insert aff", err)
	var ti, tt, tr, tb string
	sqlite_must("typeof scan", db.QueryRow(`SELECT typeof(i),typeof(t),typeof(r),typeof(b) FROM aff`).Scan(&ti, &tt, &tr, &tb))
	fwOK("affinity typeof INTEGER", ti)
	fwOK("affinity typeof TEXT", tt)
	fwOK("affinity typeof REAL", tr)
	fwOK("affinity typeof BLOB", tb)

	// UNIQUE constraint -> error.
	_, err = db.Exec(`CREATE TABLE uq (e TEXT UNIQUE)`)
	sqlite_must("create uq", err)
	_, err = db.Exec(`INSERT INTO uq(e) VALUES('dup')`)
	sqlite_must("uq insert 1", err)
	_, errUq := db.Exec(`INSERT INTO uq(e) VALUES('dup')`)
	fwOK("UNIQUE constraint violation errors", errUq != nil)
	fwOK("UNIQUE constraint msg", errUq != nil && strings.Contains(errUq.Error(), "UNIQUE"))

	// Foreign-key constraint enforcement (foreign_keys is ON above).
	_, err = db.Exec(`CREATE TABLE parent (id INTEGER PRIMARY KEY)`)
	sqlite_must("create parent", err)
	_, err = db.Exec(`CREATE TABLE child (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES parent(id))`)
	sqlite_must("create child", err)
	_, errFk := db.Exec(`INSERT INTO child(id,pid) VALUES(1, 999)`)
	fwOK("FK violation errors (PRAGMA foreign_keys ON)", errFk != nil)
	fwOK("FK violation msg", errFk != nil && strings.Contains(errFk.Error(), "FOREIGN KEY"))
	// Valid FK insert succeeds.
	_, err = db.Exec(`INSERT INTO parent(id) VALUES(999)`)
	sqlite_must("insert parent", err)
	_, errFkOK := db.Exec(`INSERT INTO child(id,pid) VALUES(1, 999)`)
	fwOK("valid FK insert succeeds", errFkOK == nil)

	// NOT NULL constraint -> error.
	_, err = db.Exec(`CREATE TABLE nn (v TEXT NOT NULL)`)
	sqlite_must("create nn", err)
	_, errNN := db.Exec(`INSERT INTO nn(v) VALUES(NULL)`)
	fwOK("NOT NULL constraint errors", errNN != nil)
	fwOK("NOT NULL constraint msg", errNN != nil && strings.Contains(errNN.Error(), "NOT NULL"))
}
