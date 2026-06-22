// framework_sqlite_comprehensive.go — the comprehensive (#764 完备) SQLite coverage:
// virtual tables (FTS5), the full DDL-constraint matrix, advanced SQL (CTE / window /
// aggregate / upsert), SAVEPOINT nesting, atomicity (constraint-violation rollback +
// foreign-key enforcement), and access permissions (read-only mode / PRAGMA query_only).
// Pure-Go driver "sqlite" (glebarez/go-sqlite → modernc.org/sqlite); deterministic
// (fixed data, no timestamps/random); asserted via the carpet's chk()/chkStr()/chkTrue().
package main

import (
	"database/sql"
	"os"
	"strings"
)

// sqliteVirtualTables — FTS5 full-text-search virtual table. modernc.org/sqlite ships the
// standard amalgamation which includes FTS5; if a build lacks it we document the gap
// honestly rather than fake a pass.
func sqliteVirtualTables() {
	db := sqlite_newMemDB()
	defer db.Close()
	if _, err := db.Exec(`CREATE VIRTUAL TABLE docs USING fts5(title, body)`); err != nil {
		chkTrue("sqlite vtable: fts5 module present", false) // record the real gap
		chkStr("sqlite vtable: fts5 create error class", classifyVtErr(err), "no-such-module")
		return
	}
	chkTrue("sqlite vtable: CREATE VIRTUAL TABLE USING fts5 ok", true)
	_, err := db.Exec(`INSERT INTO docs(title,body) VALUES
		('go','the go programming language is fast'),
		('rust','a systems programming language'),
		('java','write once run anywhere')`)
	sqlite_must("fts5 insert", err)
	var n int
	sqlite_must("fts5 match programming", db.QueryRow(`SELECT count(*) FROM docs WHERE docs MATCH 'programming'`).Scan(&n))
	chk("sqlite vtable: MATCH 'programming' rows", n, 2)
	var title string
	sqlite_must("fts5 match anywhere", db.QueryRow(`SELECT title FROM docs WHERE docs MATCH 'anywhere'`).Scan(&title))
	chkStr("sqlite vtable: MATCH 'anywhere' -> title", title, "java")
	// phrase + prefix queries (deterministic)
	sqlite_must("fts5 phrase", db.QueryRow(`SELECT count(*) FROM docs WHERE docs MATCH '"systems programming"'`).Scan(&n))
	chk("sqlite vtable: phrase match rows", n, 1)
	sqlite_must("fts5 prefix", db.QueryRow(`SELECT count(*) FROM docs WHERE docs MATCH 'lang*'`).Scan(&n))
	chk("sqlite vtable: prefix 'lang*' rows", n, 2)
}

func classifyVtErr(err error) string {
	if err != nil && strings.Contains(err.Error(), "no such module") {
		return "no-such-module"
	}
	return "other"
}

// sqliteDDLConstraints — CREATE TABLE constraint matrix + indexes; assert each constraint
// is actually enforced (UNIQUE/CHECK/NOT NULL/DEFAULT/AUTOINCREMENT) and indexes build.
func sqliteDDLConstraints() {
	db := sqlite_newMemDB()
	defer db.Close()
	_, err := db.Exec(`CREATE TABLE acct (
		id    INTEGER PRIMARY KEY AUTOINCREMENT,
		name  TEXT    NOT NULL UNIQUE,
		email TEXT    UNIQUE,
		age   INTEGER CHECK (age >= 0 AND age < 200),
		bal   INTEGER NOT NULL DEFAULT 100,
		tier  TEXT    NOT NULL DEFAULT 'free'
	)`)
	sqlite_must("ddl create acct", err)
	chkTrue("sqlite ddl: CREATE TABLE w/ constraints ok", true)
	// composite + unique + partial indexes
	_, err = db.Exec(`CREATE UNIQUE INDEX ux_acct_email ON acct(email)`)
	sqlite_must("ddl unique index", err)
	_, err = db.Exec(`CREATE INDEX ix_acct_tier_age ON acct(tier, age)`)
	sqlite_must("ddl composite index", err)
	_, err = db.Exec(`CREATE INDEX ix_acct_active ON acct(name) WHERE bal > 0`)
	sqlite_must("ddl partial index", err)
	chkTrue("sqlite ddl: unique+composite+partial indexes ok", true)
	// DEFAULT applied
	_, err = db.Exec(`INSERT INTO acct(name, age) VALUES('alice', 30)`)
	sqlite_must("ddl insert defaults", err)
	var bal int
	var tier string
	sqlite_must("ddl read defaults", db.QueryRow(`SELECT bal, tier FROM acct WHERE name='alice'`).Scan(&bal, &tier))
	chk("sqlite ddl: DEFAULT bal applied", bal, 100)
	chkStr("sqlite ddl: DEFAULT tier applied", tier, "free")
	// AUTOINCREMENT id == 1
	var id int
	sqlite_must("ddl read id", db.QueryRow(`SELECT id FROM acct WHERE name='alice'`).Scan(&id))
	chk("sqlite ddl: AUTOINCREMENT id", id, 1)
	// NOT NULL enforced
	_, e1 := db.Exec(`INSERT INTO acct(name) VALUES(NULL)`)
	chkTrue("sqlite ddl: NOT NULL rejected", e1 != nil && strings.Contains(strings.ToLower(e1.Error()), "not null"))
	// UNIQUE enforced
	_, e2 := db.Exec(`INSERT INTO acct(name, age) VALUES('alice', 40)`)
	chkTrue("sqlite ddl: UNIQUE rejected", e2 != nil && strings.Contains(strings.ToLower(e2.Error()), "unique"))
	// CHECK enforced
	_, e3 := db.Exec(`INSERT INTO acct(name, age) VALUES('bob', 999)`)
	chkTrue("sqlite ddl: CHECK rejected", e3 != nil && strings.Contains(strings.ToLower(e3.Error()), "check"))
	// ALTER TABLE add column
	_, err = db.Exec(`ALTER TABLE acct ADD COLUMN nick TEXT DEFAULT 'n/a'`)
	sqlite_must("ddl alter add col", err)
	var nick string
	sqlite_must("ddl read alter col", db.QueryRow(`SELECT nick FROM acct WHERE name='alice'`).Scan(&nick))
	chkStr("sqlite ddl: ALTER ADD COLUMN default", nick, "n/a")
}

// sqliteAdvancedSQL — CTE, window functions, aggregates, UPSERT, subqueries, UNION.
func sqliteAdvancedSQL() {
	db := sqlite_newMemDB()
	defer db.Close()
	sqlite_must("adv create", mustExec(db, `CREATE TABLE sales (region TEXT, amount INTEGER)`))
	sqlite_must("adv seed", mustExec(db,
		`INSERT INTO sales VALUES ('east',10),('east',30),('west',20),('west',20),('north',50)`))
	// aggregate + GROUP BY + HAVING
	var region string
	var total int
	sqlite_must("adv agg", db.QueryRow(
		`SELECT region, SUM(amount) s FROM sales GROUP BY region HAVING s >= 40 ORDER BY s DESC LIMIT 1`).Scan(&region, &total))
	chkStr("sqlite adv: HAVING top region", region, "north")
	chk("sqlite adv: HAVING top sum", total, 50)
	// CTE (WITH) + aggregate
	sqlite_must("adv cte", db.QueryRow(
		`WITH per AS (SELECT region, SUM(amount) s FROM sales GROUP BY region) SELECT SUM(s) FROM per`).Scan(&total))
	chk("sqlite adv: CTE total", total, 130)
	// window function ROW_NUMBER / SUM OVER (deterministic ordering)
	rows, err := db.Query(
		`SELECT region, amount, ROW_NUMBER() OVER (ORDER BY amount, region) rn FROM sales ORDER BY rn`)
	sqlite_must("adv window query", err)
	var firstRegion string
	var firstRn int
	if rows.Next() {
		var amt int
		sqlite_must("adv window scan", rows.Scan(&firstRegion, &amt, &firstRn))
	}
	rows.Close()
	chk("sqlite adv: window ROW_NUMBER first rn", firstRn, 1)
	chkStr("sqlite adv: window first region (amount asc)", firstRegion, "east")
	// UPSERT (INSERT ... ON CONFLICT DO UPDATE)
	sqlite_must("adv upsert create", mustExec(db, `CREATE TABLE kv (k TEXT PRIMARY KEY, v INTEGER)`))
	sqlite_must("adv upsert ins", mustExec(db, `INSERT INTO kv VALUES('hits', 1)`))
	sqlite_must("adv upsert", mustExec(db,
		`INSERT INTO kv(k,v) VALUES('hits',1) ON CONFLICT(k) DO UPDATE SET v = v + 10`))
	var v int
	sqlite_must("adv upsert read", db.QueryRow(`SELECT v FROM kv WHERE k='hits'`).Scan(&v))
	chk("sqlite adv: UPSERT ON CONFLICT DO UPDATE", v, 11)
	// subquery + UNION
	sqlite_must("adv union", db.QueryRow(
		`SELECT count(*) FROM (SELECT region FROM sales WHERE amount>=30 UNION SELECT 'extra')`).Scan(&total))
	chk("sqlite adv: UNION distinct count", total, 3) // east(30), north(50), extra
}

// sqliteSavepoints — SAVEPOINT / nested / ROLLBACK TO / RELEASE within a transaction.
func sqliteSavepoints() {
	db := sqlite_newMemDB()
	defer db.Close()
	sqlite_must("sp create", mustExec(db, `CREATE TABLE t (id INTEGER PRIMARY KEY, label TEXT)`))
	tx, err := db.Begin()
	sqlite_must("sp begin", err)
	sqlite_must("sp ins1", mustTxExec(tx, `INSERT INTO t VALUES (1,'a')`))
	sqlite_must("sp savepoint", mustTxExec(tx, `SAVEPOINT s1`))
	sqlite_must("sp ins2", mustTxExec(tx, `INSERT INTO t VALUES (2,'b')`))
	sqlite_must("sp nested savepoint", mustTxExec(tx, `SAVEPOINT s2`))
	sqlite_must("sp ins3", mustTxExec(tx, `INSERT INTO t VALUES (3,'c')`))
	// rollback inner savepoint -> drops row 3, keeps 1,2
	sqlite_must("sp rollback s2", mustTxExec(tx, `ROLLBACK TO s2`))
	sqlite_must("sp release s1", mustTxExec(tx, `RELEASE s1`))
	sqlite_must("sp commit", tx.Commit())
	var n int
	sqlite_must("sp count", db.QueryRow(`SELECT count(*) FROM t`).Scan(&n))
	chk("sqlite savepoint: rows after ROLLBACK TO inner", n, 2)
	var have3 int
	sqlite_must("sp row3 gone", db.QueryRow(`SELECT count(*) FROM t WHERE id=3`).Scan(&have3))
	chk("sqlite savepoint: rolled-back row absent", have3, 0)
}

// sqliteAtomicity — a transaction that violates a constraint mid-way leaves NO partial
// state after rollback; foreign-key enforcement (PRAGMA foreign_keys=ON) rejects orphans.
func sqliteAtomicity() {
	db := sqlite_newMemDB()
	defer db.Close()
	sqlite_must("atom create", mustExec(db, `CREATE TABLE u (id INTEGER PRIMARY KEY, name TEXT UNIQUE)`))
	sqlite_must("atom seed", mustExec(db, `INSERT INTO u VALUES (1,'keep')`))
	// tx: insert a valid row then a UNIQUE-violating row -> rollback -> neither persists
	tx, err := db.Begin()
	sqlite_must("atom begin", err)
	sqlite_must("atom ins ok", mustTxExec(tx, `INSERT INTO u VALUES (2,'new')`))
	_, badErr := tx.Exec(`INSERT INTO u VALUES (3,'keep')`) // UNIQUE violation
	chkTrue("sqlite atom: constraint violation detected", badErr != nil)
	sqlite_must("atom rollback", tx.Rollback())
	var n int
	sqlite_must("atom count", db.QueryRow(`SELECT count(*) FROM u`).Scan(&n))
	chk("sqlite atom: rollback discards ALL tx rows", n, 1) // only the seeded 'keep'
	var has2 int
	sqlite_must("atom row2 gone", db.QueryRow(`SELECT count(*) FROM u WHERE id=2`).Scan(&has2))
	chk("sqlite atom: valid row in failed tx not committed", has2, 0)
	// foreign-key enforcement
	sqlite_must("atom fk pragma", mustExec(db, `PRAGMA foreign_keys=ON`))
	sqlite_must("atom parent", mustExec(db, `CREATE TABLE p (id INTEGER PRIMARY KEY)`))
	sqlite_must("atom child", mustExec(db, `CREATE TABLE c (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES p(id))`))
	sqlite_must("atom parent row", mustExec(db, `INSERT INTO p VALUES (1)`))
	sqlite_must("atom child ok", mustExec(db, `INSERT INTO c VALUES (1,1)`))
	_, fkErr := db.Exec(`INSERT INTO c VALUES (2, 999)`) // orphan
	chkTrue("sqlite atom: FK orphan rejected (PRAGMA foreign_keys=ON)",
		fkErr != nil && strings.Contains(strings.ToLower(fkErr.Error()), "foreign key"))
}

// sqlitePermissions — access control: read-only DB connection (?mode=ro) rejects writes
// and allows reads; PRAGMA query_only=ON makes a writable handle reject writes.
func sqlitePermissions() {
	// Build a file DB, seed it, then reopen read-only.
	f := sqlite_tempPath()
	rw, err := sql.Open("sqlite", "file:"+f)
	sqlite_must("perm open rw", err)
	sqlite_must("perm create", mustExec(rw, `CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)`))
	sqlite_must("perm seed", mustExec(rw, `INSERT INTO t VALUES (1,'ro')`))
	rw.Close()
	// read-only open
	ro, err := sql.Open("sqlite", "file:"+f+"?mode=ro")
	sqlite_must("perm open ro", err)
	defer ro.Close()
	var v string
	sqlite_must("perm ro read", ro.QueryRow(`SELECT v FROM t WHERE id=1`).Scan(&v))
	chkStr("sqlite perm: read-only read ok", v, "ro")
	_, wErr := ro.Exec(`INSERT INTO t VALUES (2,'nope')`)
	chkTrue("sqlite perm: read-only write rejected", wErr != nil &&
		(strings.Contains(strings.ToLower(wErr.Error()), "read") || strings.Contains(strings.ToLower(wErr.Error()), "readonly")))
	// PRAGMA query_only on a writable handle
	db := sqlite_newMemDB()
	defer db.Close()
	sqlite_must("perm qo create", mustExec(db, `CREATE TABLE q (id INTEGER PRIMARY KEY)`))
	sqlite_must("perm query_only on", mustExec(db, `PRAGMA query_only=ON`))
	_, qErr := db.Exec(`INSERT INTO q VALUES (1)`)
	chkTrue("sqlite perm: PRAGMA query_only blocks write", qErr != nil)
	sqlite_must("perm query_only off", mustExec(db, `PRAGMA query_only=OFF`))
	sqlite_must("perm write after off", mustExec(db, `INSERT INTO q VALUES (1)`))
	var n int
	sqlite_must("perm count", db.QueryRow(`SELECT count(*) FROM q`).Scan(&n))
	chk("sqlite perm: write ok after query_only=OFF", n, 1)
}

// small helpers (exec returning only error, for terse must-wrapping)
func mustExec(db *sql.DB, q string, args ...any) error   { _, e := db.Exec(q, args...); return e }
func mustTxExec(tx *sql.Tx, q string, args ...any) error { _, e := tx.Exec(q, args...); return e }

// sqlite_tempPath returns a unique temp .db file path (created + closed) for tests that
// need to reopen the same file (e.g. read-only mode). The file name is NOT printed, so it
// does not affect the deterministic golden.
func sqlite_tempPath() string {
	f, err := os.CreateTemp("", "fwsqlite-perm-*.db")
	sqlite_must("sqlite_tempPath create", err)
	name := f.Name()
	f.Close()
	return name
}
