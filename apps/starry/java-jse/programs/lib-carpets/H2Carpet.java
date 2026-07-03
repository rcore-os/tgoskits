package org.starry.dod;

import java.io.File;
import java.io.PrintStream;
import java.io.StringReader;
import java.math.BigDecimal;
import java.nio.charset.StandardCharsets;
import java.sql.Array;
import java.sql.CallableStatement;
import java.sql.Connection;
import java.sql.Date;
import java.sql.DatabaseMetaData;
import java.sql.DriverManager;
import java.sql.ParameterMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.Savepoint;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Time;
import java.sql.Timestamp;
import java.sql.Types;
import java.util.Arrays;
import java.util.Locale;
import java.util.Objects;
import java.util.TimeZone;
import java.util.UUID;

import org.h2.tools.Csv;
import org.h2.tools.RunScript;
import org.h2.tools.Script;
import org.h2.tools.Shell;
import org.h2.tools.SimpleResultSet;

/**
 * Carpet-grade coverage for the H2 database engine (JDBC + SQL surface + command-line tools).
 * Single file, deterministic, offline (in-memory + /tmp only), exact-equality assertions.
 * Verified against H2 2.2.224 on JDK 17.
 */
public class H2Carpet {

    static final String URL = "jdbc:h2:mem:carpet;DB_CLOSE_DELAY=-1";
    static Connection conn;
    static int ok = 0;
    static int fail = 0;

    interface Act {
        void run() throws Exception;
    }

    static void pass() {
        ok++;
    }

    static void bad(String name, String detail) {
        fail++;
        System.out.println("FAIL " + name + " :: " + detail);
    }

    static void check(String name, boolean cond) {
        if (cond) {
            pass();
        } else {
            bad(name, "condition was false");
        }
    }

    static void eqi(String name, long exp, long act) {
        if (exp == act) {
            pass();
        } else {
            bad(name, "exp=" + exp + " act=" + act);
        }
    }

    static void eqs(String name, String exp, String act) {
        if (Objects.equals(exp, act)) {
            pass();
        } else {
            bad(name, "exp=[" + exp + "] act=[" + act + "]");
        }
    }

    static void eqd(String name, double exp, double act, double eps) {
        if (Math.abs(exp - act) <= eps) {
            pass();
        } else {
            bad(name, "exp=" + exp + " act=" + act);
        }
    }

    static void section(String name, Act a) {
        try {
            a.run();
        } catch (Throwable t) {
            bad(name + ".section", "threw " + t);
        }
    }

    static void expectState(String name, String wantState, Act a) {
        try {
            a.run();
            bad(name, "no exception (expected SQLState " + wantState + ")");
        } catch (SQLException e) {
            eqs(name, wantState, e.getSQLState());
        } catch (Throwable t) {
            bad(name, "wrong exception type: " + t);
        }
    }

    static void ddl(String sql) throws SQLException {
        try (Statement s = conn.createStatement()) {
            s.execute(sql);
        }
    }

    static int exec(String sql) throws SQLException {
        try (Statement s = conn.createStatement()) {
            return s.executeUpdate(sql);
        }
    }

    static long qLong(String sql) throws SQLException {
        try (Statement s = conn.createStatement(); ResultSet r = s.executeQuery(sql)) {
            r.next();
            return r.getLong(1);
        }
    }

    static String qStr(String sql) throws SQLException {
        try (Statement s = conn.createStatement(); ResultSet r = s.executeQuery(sql)) {
            r.next();
            return r.getString(1);
        }
    }

    static int rowCount(String sql) throws SQLException {
        try (Statement s = conn.createStatement(); ResultSet r = s.executeQuery(sql)) {
            int n = 0;
            while (r.next()) {
                n++;
            }
            return n;
        }
    }

    public static void main(String[] args) throws Exception {
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"));
        Locale.setDefault(Locale.US);

        Class.forName("org.h2.Driver");
        conn = DriverManager.getConnection(URL, "sa", "");

        section("connection", H2Carpet::testConnection);
        section("ddl", H2Carpet::testDdl);
        section("dml", H2Carpet::testDml);
        section("dql", H2Carpet::testDql);
        section("joins", H2Carpet::testJoins);
        section("aggregates", H2Carpet::testAggregates);
        section("subqueries", H2Carpet::testSubqueries);
        section("setops_cte_window", H2Carpet::testSetOpsCteWindow);
        section("prepared", H2Carpet::testPrepared);
        section("callable", H2Carpet::testCallable);
        section("transactions", H2Carpet::testTransactions);
        section("resultset", H2Carpet::testResultSet);
        section("dbmeta", H2Carpet::testDatabaseMetaData);
        section("types", H2Carpet::testDataTypes);
        section("constraints", H2Carpet::testConstraints);
        section("functions", H2Carpet::testFunctions);
        section("sequences", H2Carpet::testSequences);
        section("merge", H2Carpet::testMerge);
        section("tool_runscript", H2Carpet::testToolRunScript);
        section("tool_script", H2Carpet::testToolScript);
        section("tool_csv", H2Carpet::testToolCsv);
        section("tool_shell", H2Carpet::testToolShell);

        conn.close();

        System.out.println("H2_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("H2_DONE");
        }
    }

    // ---------------------------------------------------------------- connection
    static void testConnection() throws Exception {
        check("conn.notClosed", !conn.isClosed());
        check("conn.isValid", conn.isValid(2));
        check("conn.autocommitDefault", conn.getAutoCommit());
        check("conn.notReadOnly", !conn.isReadOnly());
        eqs("conn.catalogIsUrlName", "CARPET", conn.getCatalog());
        eqs("conn.defaultSchema", "PUBLIC", conn.getSchema());
        DatabaseMetaData m = conn.getMetaData();
        eqs("conn.productName", "H2", m.getDatabaseProductName());
        check("conn.productVersion2x", m.getDatabaseProductVersion().startsWith("2."));
        eqi("conn.dbMajor", 2, m.getDatabaseMajorVersion());
        check("conn.driverNameH2", m.getDriverName().toLowerCase(Locale.US).contains("h2"));
        check("conn.jdbcMajorGe4", m.getJDBCMajorVersion() >= 4);
        eqs("conn.nativeSqlPassthrough", "SELECT 1", conn.nativeSQL("SELECT 1"));
        eqi("conn.holdability", ResultSet.HOLD_CURSORS_OVER_COMMIT, conn.getHoldability());
    }

    // ---------------------------------------------------------------- DDL
    static void testDdl() throws Exception {
        ddl("CREATE TABLE ddl_t (id INT PRIMARY KEY, a INT)");
        check("ddl.tableExists", tableExists("DDL_T"));

        // ALTER ADD COLUMN
        ddl("ALTER TABLE ddl_t ADD COLUMN b VARCHAR(20)");
        check("ddl.addColumn", columnExists("DDL_T", "B"));

        // ALTER DROP COLUMN
        ddl("ALTER TABLE ddl_t DROP COLUMN b");
        check("ddl.dropColumn", !columnExists("DDL_T", "B"));

        // ALTER RENAME COLUMN
        ddl("ALTER TABLE ddl_t ALTER COLUMN a RENAME TO a2");
        check("ddl.renameColumn", columnExists("DDL_T", "A2") && !columnExists("DDL_T", "A"));

        // CREATE INDEX
        ddl("CREATE INDEX ddl_idx ON ddl_t(a2)");
        check("ddl.indexCreated", indexExists("DDL_T", "DDL_IDX"));
        ddl("DROP INDEX ddl_idx");
        check("ddl.indexDropped", !indexExists("DDL_T", "DDL_IDX"));

        // CREATE VIEW
        ddl("INSERT INTO ddl_t(id, a2) VALUES (1, 100), (2, 200)");
        ddl("CREATE VIEW ddl_v AS SELECT id, a2 FROM ddl_t WHERE a2 >= 150");
        eqi("ddl.viewQuery", 1, rowCount("SELECT * FROM ddl_v"));
        eqi("ddl.viewValue", 200, qLong("SELECT a2 FROM ddl_v"));
        ddl("DROP VIEW ddl_v");
        check("ddl.viewDropped", !tableExists("DDL_V"));

        // CREATE SEQUENCE
        ddl("CREATE SEQUENCE ddl_seq START WITH 100 INCREMENT BY 10");
        eqi("ddl.seqFirst", 100, qLong("SELECT NEXT VALUE FOR ddl_seq"));
        ddl("DROP SEQUENCE ddl_seq");

        // CREATE SCHEMA + qualified table
        ddl("CREATE SCHEMA ddl_s");
        ddl("CREATE TABLE ddl_s.t (x INT)");
        ddl("INSERT INTO ddl_s.t VALUES (7)");
        eqi("ddl.schemaQualified", 7, qLong("SELECT x FROM ddl_s.t"));
        ddl("DROP SCHEMA ddl_s CASCADE");
        expectState("ddl.schemaDropped", "90079", () -> qLong("SELECT x FROM ddl_s.t"));

        // DROP TABLE
        ddl("DROP TABLE ddl_t");
        check("ddl.tableDropped", !tableExists("DDL_T"));

        // CREATE TABLE IF NOT EXISTS idempotency
        ddl("CREATE TABLE IF NOT EXISTS ddl_t2 (id INT)");
        ddl("CREATE TABLE IF NOT EXISTS ddl_t2 (id INT)");
        check("ddl.ifNotExists", tableExists("DDL_T2"));
        ddl("DROP TABLE ddl_t2");
    }

    // ---------------------------------------------------------------- DML
    static void testDml() throws Exception {
        ddl("CREATE TABLE dml_t (id INT PRIMARY KEY, n VARCHAR(20), v INT)");
        eqi("dml.insertSingle", 1, exec("INSERT INTO dml_t VALUES (1, 'a', 10)"));
        eqi("dml.insertMulti", 3, exec("INSERT INTO dml_t VALUES (2,'b',20),(3,'c',30),(4,'d',40)"));
        eqi("dml.rowCount", 4, rowCount("SELECT * FROM dml_t"));

        eqi("dml.updateCount", 2, exec("UPDATE dml_t SET v = v + 1 WHERE id <= 2"));
        eqi("dml.updateApplied", 11, qLong("SELECT v FROM dml_t WHERE id = 1"));

        eqi("dml.deleteCount", 1, exec("DELETE FROM dml_t WHERE id = 4"));
        eqi("dml.afterDelete", 3, rowCount("SELECT * FROM dml_t"));

        // INSERT ... SELECT
        ddl("CREATE TABLE dml_copy (id INT, v INT)");
        eqi("dml.insertSelect", 3, exec("INSERT INTO dml_copy SELECT id, v FROM dml_t"));

        // UPDATE with expression and no-match
        eqi("dml.updateNoMatch", 0, exec("UPDATE dml_t SET v = 0 WHERE id = 999"));

        ddl("DROP TABLE dml_t");
        ddl("DROP TABLE dml_copy");
    }

    // ---------------------------------------------------------------- DQL
    static void testDql() throws Exception {
        ddl("CREATE TABLE dq (id INT PRIMARY KEY, g INT, v INT)");
        ddl("INSERT INTO dq VALUES (1,1,5),(2,1,7),(3,2,3),(4,2,9),(5,3,1)");

        eqi("dql.where", 2, rowCount("SELECT * FROM dq WHERE g = 1"));
        eqi("dql.orderByFirst", 5, qLong("SELECT id FROM dq ORDER BY v ASC LIMIT 1"));
        eqi("dql.orderByDescFirst", 4, qLong("SELECT id FROM dq ORDER BY v DESC LIMIT 1"));
        eqi("dql.limit", 2, rowCount("SELECT * FROM dq ORDER BY id LIMIT 2"));
        eqi("dql.limitOffset", 4, qLong("SELECT id FROM dq ORDER BY id LIMIT 1 OFFSET 3"));
        eqi("dql.offsetCount", 2, rowCount("SELECT * FROM dq ORDER BY id LIMIT 5 OFFSET 3"));
        eqi("dql.fetchFirst", 1, rowCount("SELECT * FROM dq ORDER BY id FETCH FIRST 1 ROW ONLY"));
        eqi("dql.distinct", 3, rowCount("SELECT DISTINCT g FROM dq"));
        eqi("dql.betweenCount", 3, rowCount("SELECT * FROM dq WHERE v BETWEEN 3 AND 7"));
        eqi("dql.inList", 2, rowCount("SELECT * FROM dq WHERE id IN (1,5)"));
        eqi("dql.likeCount", 5, rowCount("SELECT * FROM dq WHERE CAST(id AS VARCHAR) LIKE '%'"));
        eqi("dql.isNullNone", 0, rowCount("SELECT * FROM dq WHERE v IS NULL"));

        ddl("DROP TABLE dq");
    }

    // ---------------------------------------------------------------- JOINs
    static void testJoins() throws Exception {
        ddl("CREATE TABLE dept (did INT PRIMARY KEY, dname VARCHAR(20))");
        ddl("INSERT INTO dept VALUES (1,'Eng'),(2,'Sales'),(3,'Empty')");
        ddl("CREATE TABLE emp (eid INT PRIMARY KEY, ename VARCHAR(20), did INT, sal INT)");
        ddl("INSERT INTO emp VALUES (1,'a',1,100),(2,'b',1,200),(3,'c',2,300),(4,'d',NULL,400)");

        eqi("join.inner", 3, rowCount(
                "SELECT * FROM emp e INNER JOIN dept d ON e.did = d.did"));
        eqi("join.left", 4, rowCount(
                "SELECT * FROM emp e LEFT JOIN dept d ON e.did = d.did"));
        eqi("join.right", 4, rowCount(
                "SELECT * FROM emp e RIGHT JOIN dept d ON e.did = d.did"));
        eqi("join.cross", 12, rowCount("SELECT * FROM emp CROSS JOIN dept"));
        eqi("join.leftNullDept", 1, rowCount(
                "SELECT * FROM emp e LEFT JOIN dept d ON e.did = d.did WHERE d.did IS NULL"));
        eqi("join.rightEmptyDept", 1, rowCount(
                "SELECT * FROM emp e RIGHT JOIN dept d ON e.did = d.did WHERE e.eid IS NULL"));
        eqs("join.projected", "Eng", qStr(
                "SELECT d.dname FROM emp e JOIN dept d ON e.did = d.did WHERE e.eid = 1"));
        // tables kept for aggregates/subqueries below
    }

    // ---------------------------------------------------------------- aggregates
    static void testAggregates() throws Exception {
        eqi("agg.count", 4, qLong("SELECT COUNT(*) FROM emp"));
        eqi("agg.countCol", 3, qLong("SELECT COUNT(did) FROM emp"));
        eqi("agg.sum", 1000, qLong("SELECT SUM(sal) FROM emp"));
        eqi("agg.avg", 250, qLong("SELECT AVG(sal) FROM emp"));
        eqi("agg.min", 100, qLong("SELECT MIN(sal) FROM emp"));
        eqi("agg.max", 400, qLong("SELECT MAX(sal) FROM emp"));
        eqi("agg.countDistinct", 2, qLong("SELECT COUNT(DISTINCT did) FROM emp"));
        eqi("agg.groupBy", 2, rowCount(
                "SELECT did, COUNT(*) FROM emp WHERE did IS NOT NULL GROUP BY did"));
        eqi("agg.having", 1, rowCount(
                "SELECT did FROM emp WHERE did IS NOT NULL GROUP BY did HAVING COUNT(*) >= 2"));
        eqi("agg.groupSum", 300, qLong(
                "SELECT SUM(sal) FROM emp WHERE did = 1 GROUP BY did"));
        eqs("agg.listagg", "a,b", qStr(
                "SELECT LISTAGG(ename, ',') WITHIN GROUP (ORDER BY ename) FROM emp WHERE did = 1"));
    }

    // ---------------------------------------------------------------- subqueries
    static void testSubqueries() throws Exception {
        eqs("sub.scalar", "d", qStr(
                "SELECT ename FROM emp WHERE sal = (SELECT MAX(sal) FROM emp)"));
        eqi("sub.inSubquery", 2, qLong(
                "SELECT COUNT(*) FROM emp WHERE did IN (SELECT did FROM dept WHERE dname = 'Eng')"));
        eqi("sub.correlated", 2, qLong(
                "SELECT COUNT(*) FROM emp e WHERE sal > (SELECT AVG(sal) FROM emp)"));
        eqi("sub.exists", 3, qLong(
                "SELECT COUNT(*) FROM emp e WHERE EXISTS (SELECT 1 FROM dept d WHERE d.did = e.did)"));
        eqi("sub.notExists", 1, qLong(
                "SELECT COUNT(*) FROM emp e WHERE NOT EXISTS (SELECT 1 FROM dept d WHERE d.did = e.did)"));
        eqi("sub.fromDerived", 600, qLong(
                "SELECT SUM(s) FROM (SELECT sal AS s FROM emp WHERE sal < 350) t WHERE s >= 100"));

        ddl("DROP TABLE emp");
        ddl("DROP TABLE dept");
    }

    // ---------------------------------------------------------------- set ops / CTE / window
    static void testSetOpsCteWindow() throws Exception {
        eqi("union.distinct", 2, rowCount("SELECT 1 UNION SELECT 2 UNION SELECT 1"));
        eqi("union.all", 2, rowCount("SELECT 1 UNION ALL SELECT 1"));
        eqi("intersect", 1, rowCount(
                "SELECT 1 INTERSECT SELECT 1 UNION SELECT 2 INTERSECT SELECT 9"));
        eqi("except", 1, rowCount("(SELECT 1 UNION SELECT 2) EXCEPT (SELECT 2)"));

        eqi("cte.values", 6, qLong("WITH t(n) AS (VALUES (1),(2),(3)) SELECT SUM(n) FROM t"));
        eqi("cte.recursiveCount", 5, qLong(
                "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n < 5) "
                        + "SELECT COUNT(*) FROM r"));
        eqi("cte.recursiveSum", 15, qLong(
                "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n < 5) "
                        + "SELECT SUM(n) FROM r"));

        ddl("CREATE TABLE win (g INT, v INT)");
        ddl("INSERT INTO win VALUES (1,10),(1,20),(2,30)");
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery(
                        "SELECT v, "
                        + "ROW_NUMBER() OVER (ORDER BY v) rn, "
                        + "RANK() OVER (ORDER BY g) rk, "
                        + "DENSE_RANK() OVER (ORDER BY g) dr, "
                        + "SUM(v) OVER (PARTITION BY g ORDER BY v) running "
                        + "FROM win ORDER BY v")) {
            r.next();
            eqi("win.rn1", 1, r.getInt("rn"));
            eqi("win.rk1", 1, r.getInt("rk"));
            eqi("win.running1", 10, r.getInt("running"));
            r.next();
            eqi("win.rn2", 2, r.getInt("rn"));
            eqi("win.rk2", 1, r.getInt("rk"));
            eqi("win.running2", 30, r.getInt("running"));
            r.next();
            eqi("win.rn3", 3, r.getInt("rn"));
            eqi("win.rk3", 3, r.getInt("rk"));
            eqi("win.dr3", 2, r.getInt("dr"));
        }
        ddl("DROP TABLE win");
    }

    // ---------------------------------------------------------------- PreparedStatement
    static void testPrepared() throws Exception {
        ddl("CREATE TABLE pst (id INT PRIMARY KEY, n VARCHAR(20), v INT)");

        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO pst VALUES (?, ?, ?)")) {
            ParameterMetaData pm = ps.getParameterMetaData();
            eqi("prep.paramCount", 3, pm.getParameterCount());
            ps.setInt(1, 1);
            ps.setString(2, "x");
            ps.setInt(3, 50);
            eqi("prep.execUpdate", 1, ps.executeUpdate());
        }

        // query with params
        try (PreparedStatement ps = conn.prepareStatement(
                "SELECT n FROM pst WHERE id = ? AND v > ?")) {
            ps.setInt(1, 1);
            ps.setInt(2, 10);
            try (ResultSet r = ps.executeQuery()) {
                check("prep.queryHit", r.next());
                eqs("prep.queryValue", "x", r.getString(1));
            }
        }

        // batch insert
        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO pst VALUES (?, ?, ?)")) {
            for (int i = 2; i <= 4; i++) {
                ps.setInt(1, i);
                ps.setString(2, "n" + i);
                ps.setInt(3, i * 10);
                ps.addBatch();
            }
            int[] counts = ps.executeBatch();
            eqi("prep.batchLen", 3, counts.length);
            eqi("prep.batchEach", 1, counts[0] + counts[1] + counts[2] == 3 ? 1 : 0);
            eqi("prep.batchRows", 4, rowCount("SELECT * FROM pst"));
        }

        // null parameter
        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO pst VALUES (?, ?, ?)")) {
            ps.setInt(1, 9);
            ps.setNull(2, Types.VARCHAR);
            ps.setInt(3, 0);
            ps.executeUpdate();
        }
        check("prep.nullStored", null == qStr("SELECT n FROM pst WHERE id = 9"));

        // generated keys (IDENTITY)
        ddl("CREATE TABLE gk (id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, n VARCHAR(10))");
        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO gk(n) VALUES (?)", Statement.RETURN_GENERATED_KEYS)) {
            ps.setString(1, "first");
            ps.executeUpdate();
            try (ResultSet keys = ps.getGeneratedKeys()) {
                check("prep.genKeyNext", keys.next());
                eqi("prep.genKeyValue", 1, keys.getLong(1));
            }
            ps.setString(1, "second");
            ps.executeUpdate();
            try (ResultSet keys = ps.getGeneratedKeys()) {
                keys.next();
                eqi("prep.genKeyValue2", 2, keys.getLong(1));
            }
        }
        // Statement-level generated keys
        try (Statement s = conn.createStatement()) {
            s.executeUpdate("INSERT INTO gk(n) VALUES ('third')", Statement.RETURN_GENERATED_KEYS);
            try (ResultSet keys = s.getGeneratedKeys()) {
                keys.next();
                eqi("prep.stmtGenKey", 3, keys.getLong(1));
            }
        }

        // Statement batch
        try (Statement s = conn.createStatement()) {
            s.addBatch("UPDATE pst SET v = v + 1 WHERE id = 1");
            s.addBatch("UPDATE pst SET v = v + 1 WHERE id = 2");
            int[] c = s.executeBatch();
            eqi("prep.stmtBatchLen", 2, c.length);
        }
        eqi("prep.stmtBatchApplied", 51, qLong("SELECT v FROM pst WHERE id = 1"));

        ddl("DROP TABLE pst");
        ddl("DROP TABLE gk");
    }

    // ---------------------------------------------------------------- CallableStatement
    static void testCallable() throws Exception {
        try (CallableStatement cs = conn.prepareCall("CALL ABS(?)")) {
            cs.setInt(1, -42);
            try (ResultSet r = cs.executeQuery()) {
                r.next();
                eqi("call.absResult", 42, r.getInt(1));
            }
        }
        try (CallableStatement cs = conn.prepareCall("{? = CALL 6 * 7}")) {
            cs.registerOutParameter(1, Types.INTEGER);
            cs.execute();
            eqi("call.outParam", 42, cs.getInt(1));
        }
        try (CallableStatement cs = conn.prepareCall(
                "CALL GREATEST(CAST(? AS INT), CAST(? AS INT), CAST(? AS INT))")) {
            cs.setInt(1, 3);
            cs.setInt(2, 9);
            cs.setInt(3, 5);
            try (ResultSet r = cs.executeQuery()) {
                r.next();
                eqi("call.greatest", 9, r.getInt(1));
            }
        }
    }

    // ---------------------------------------------------------------- transactions
    static void testTransactions() throws Exception {
        ddl("CREATE TABLE tx (id INT PRIMARY KEY, v INT)");
        eqi("tx.isolationDefault", Connection.TRANSACTION_READ_COMMITTED,
                conn.getTransactionIsolation());

        conn.setAutoCommit(false);
        try {
            exec("INSERT INTO tx VALUES (1, 10)");
            conn.rollback();
            eqi("tx.rollbackEmpty", 0, rowCount("SELECT * FROM tx"));

            exec("INSERT INTO tx VALUES (2, 20)");
            conn.commit();
            eqi("tx.commitPersists", 1, rowCount("SELECT * FROM tx"));

            // Savepoint
            exec("INSERT INTO tx VALUES (3, 30)");
            Savepoint sp = conn.setSavepoint("sp1");
            exec("INSERT INTO tx VALUES (4, 40)");
            eqi("tx.beforeSavepointRollback", 3, rowCount("SELECT * FROM tx"));
            conn.rollback(sp);
            eqi("tx.savepointRollback", 2, rowCount("SELECT * FROM tx"));
            conn.releaseSavepoint(conn.setSavepoint());
            conn.commit();
            eqi("tx.afterSavepointCommit", 2, rowCount("SELECT * FROM tx"));

            // isolation level set/get
            conn.setTransactionIsolation(Connection.TRANSACTION_SERIALIZABLE);
            eqi("tx.isolationSet", Connection.TRANSACTION_SERIALIZABLE,
                    conn.getTransactionIsolation());
            conn.setTransactionIsolation(Connection.TRANSACTION_READ_COMMITTED);
            eqi("tx.isolationReset", Connection.TRANSACTION_READ_COMMITTED,
                    conn.getTransactionIsolation());
        } finally {
            conn.setAutoCommit(true);
        }
        check("tx.autocommitRestored", conn.getAutoCommit());
        ddl("DROP TABLE tx");
    }

    // ---------------------------------------------------------------- ResultSet
    static void testResultSet() throws Exception {
        ddl("CREATE TABLE rs (id INT PRIMARY KEY, n VARCHAR(20), v INT)");
        ddl("INSERT INTO rs VALUES (1,'a',10),(2,'b',20),(3,'c',30)");

        // getXxx by index and by label, wasNull
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT id, n, v FROM rs WHERE id = 2")) {
            r.next();
            eqi("rs.getIntIndex", 2, r.getInt(1));
            eqs("rs.getStringLabel", "b", r.getString("n"));
            eqi("rs.getIntLabel", 20, r.getInt("v"));
            check("rs.wasNotNull", !r.wasNull());
            check("rs.findColumn", r.findColumn("V") == 3);
        }

        // wasNull on a NULL
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT CAST(NULL AS INT)")) {
            r.next();
            r.getInt(1);
            check("rs.wasNullTrue", r.wasNull());
        }

        // scrollable cursor
        try (Statement s = conn.createStatement(
                ResultSet.TYPE_SCROLL_INSENSITIVE, ResultSet.CONCUR_READ_ONLY);
                ResultSet r = s.executeQuery("SELECT id FROM rs ORDER BY id")) {
            check("rs.last", r.last());
            eqi("rs.lastRowNum", 3, r.getRow());
            eqi("rs.lastValue", 3, r.getInt(1));
            check("rs.first", r.first());
            eqi("rs.firstValue", 1, r.getInt(1));
            check("rs.absolute2", r.absolute(2));
            eqi("rs.absoluteValue", 2, r.getInt(1));
            check("rs.relativePlus1", r.relative(1));
            eqi("rs.relativeValue", 3, r.getInt(1));
            check("rs.previous", r.previous());
            eqi("rs.previousValue", 2, r.getInt(1));
            r.beforeFirst();
            check("rs.isBeforeFirst", r.isBeforeFirst());
            r.afterLast();
            check("rs.isAfterLast", r.isAfterLast());
        }

        // ResultSetMetaData
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT id, n, v FROM rs")) {
            ResultSetMetaData md = r.getMetaData();
            eqi("rsmd.columnCount", 3, md.getColumnCount());
            eqs("rsmd.col1Label", "ID", md.getColumnLabel(1));
            eqs("rsmd.col2Name", "N", md.getColumnName(2));
            eqi("rsmd.col1Type", Types.INTEGER, md.getColumnType(1));
            eqs("rsmd.col2TypeName", "CHARACTER VARYING", md.getColumnTypeName(2));
            eqi("rsmd.col2Precision", 20, md.getPrecision(2));
            eqi("rsmd.col1Nullable", ResultSetMetaData.columnNoNulls, md.isNullable(1));
            eqi("rsmd.col2Nullable", ResultSetMetaData.columnNullable, md.isNullable(2));
            eqs("rsmd.col1ClassName", "java.lang.Integer", md.getColumnClassName(1));
        }
        ddl("DROP TABLE rs");
    }

    // ---------------------------------------------------------------- DatabaseMetaData
    static void testDatabaseMetaData() throws Exception {
        ddl("CREATE TABLE meta_t (a INT PRIMARY KEY, b VARCHAR(10), c DATE)");
        ddl("CREATE TABLE meta_child (cid INT PRIMARY KEY, pa INT REFERENCES meta_t(a))");
        DatabaseMetaData m = conn.getMetaData();

        eqs("dbmd.quoteString", "\"", m.getIdentifierQuoteString());
        check("dbmd.supportsTransactions", m.supportsTransactions());
        check("dbmd.supportsBatch", m.supportsBatchUpdates());
        check("dbmd.supportsScroll",
                m.supportsResultSetType(ResultSet.TYPE_SCROLL_INSENSITIVE));
        check("dbmd.supportsConcur",
                m.supportsResultSetConcurrency(
                        ResultSet.TYPE_SCROLL_INSENSITIVE, ResultSet.CONCUR_READ_ONLY));
        check("dbmd.supportsOuterJoins", m.supportsOuterJoins());
        check("dbmd.supportsUnion", m.supportsUnion());
        check("dbmd.supportsSubqueries", m.supportsSubqueriesInComparisons());
        check("dbmd.storesUpperUnquoted", m.storesUpperCaseIdentifiers());

        // getTables
        try (ResultSet r = m.getTables(null, "PUBLIC", "META_T", new String[] {"TABLE"})) {
            check("dbmd.getTablesNext", r.next());
            eqs("dbmd.getTablesName", "META_T", r.getString("TABLE_NAME"));
        }
        // getColumns ordered
        try (ResultSet r = m.getColumns(null, "PUBLIC", "META_T", null)) {
            r.next();
            eqs("dbmd.col1", "A", r.getString("COLUMN_NAME"));
            r.next();
            eqs("dbmd.col2", "B", r.getString("COLUMN_NAME"));
            r.next();
            eqs("dbmd.col3", "C", r.getString("COLUMN_NAME"));
            check("dbmd.colsDone", !r.next());
        }
        // getPrimaryKeys
        try (ResultSet r = m.getPrimaryKeys(null, "PUBLIC", "META_T")) {
            check("dbmd.pkNext", r.next());
            eqs("dbmd.pkColumn", "A", r.getString("COLUMN_NAME"));
        }
        // getImportedKeys (FK)
        try (ResultSet r = m.getImportedKeys(null, "PUBLIC", "META_CHILD")) {
            check("dbmd.fkNext", r.next());
            eqs("dbmd.fkPkTable", "META_T", r.getString("PKTABLE_NAME"));
            eqs("dbmd.fkColumn", "PA", r.getString("FKCOLUMN_NAME"));
        }
        // getTypeInfo not empty
        try (ResultSet r = m.getTypeInfo()) {
            check("dbmd.typeInfo", r.next());
        }
        // getSchemas contains PUBLIC
        boolean foundPublic = false;
        try (ResultSet r = m.getSchemas()) {
            while (r.next()) {
                if ("PUBLIC".equals(r.getString("TABLE_SCHEM"))) {
                    foundPublic = true;
                }
            }
        }
        check("dbmd.schemasPublic", foundPublic);

        ddl("DROP TABLE meta_child");
        ddl("DROP TABLE meta_t");
    }

    // ---------------------------------------------------------------- data types
    static void testDataTypes() throws Exception {
        ddl("CREATE TABLE typ ("
                + "c_int INT, c_bigint BIGINT, c_smallint SMALLINT, c_tinyint TINYINT, "
                + "c_varchar VARCHAR(50), c_char CHAR(5), c_decimal DECIMAL(10,2), "
                + "c_double DOUBLE PRECISION, c_real REAL, c_bool BOOLEAN, "
                + "c_date DATE, c_time TIME, c_ts TIMESTAMP, "
                + "c_vbin VARBINARY(16), c_blob BLOB, c_clob CLOB, c_uuid UUID, c_arr INTEGER ARRAY)");

        UUID uuid = UUID.fromString("00000000-0000-0000-0000-0000000000ff");
        byte[] vbin = new byte[] {1, 2, 3, 4};
        byte[] blob = "Hello".getBytes(StandardCharsets.UTF_8);

        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO typ VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")) {
            ps.setInt(1, Integer.MAX_VALUE);
            ps.setLong(2, 9000000000L);
            ps.setShort(3, (short) 32000);
            ps.setByte(4, (byte) 120);
            ps.setString(5, "hello");
            ps.setString(6, "abcde");
            ps.setBigDecimal(7, new BigDecimal("123.45"));
            ps.setDouble(8, 2.5);
            ps.setFloat(9, 1.5f);
            ps.setBoolean(10, true);
            ps.setDate(11, Date.valueOf("2021-03-14"));
            ps.setTime(12, Time.valueOf("13:45:30"));
            ps.setTimestamp(13, Timestamp.valueOf("2021-03-14 13:45:30"));
            ps.setBytes(14, vbin);
            ps.setBytes(15, blob);
            ps.setString(16, "clobtext");
            ps.setObject(17, uuid);
            ps.setArray(18, conn.createArrayOf("INTEGER", new Object[] {10, 20, 30}));
            eqi("typ.insert", 1, ps.executeUpdate());
        }

        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT * FROM typ")) {
            r.next();
            eqi("typ.int", Integer.MAX_VALUE, r.getInt("c_int"));
            eqi("typ.bigint", 9000000000L, r.getLong("c_bigint"));
            eqi("typ.smallint", 32000, r.getShort("c_smallint"));
            eqi("typ.tinyint", 120, r.getByte("c_tinyint"));
            eqs("typ.varchar", "hello", r.getString("c_varchar"));
            eqs("typ.char", "abcde", r.getString("c_char"));
            check("typ.decimal", new BigDecimal("123.45").compareTo(r.getBigDecimal("c_decimal")) == 0);
            eqi("typ.decimalScale", 2, r.getBigDecimal("c_decimal").scale());
            eqd("typ.double", 2.5, r.getDouble("c_double"), 0.0);
            eqd("typ.real", 1.5, r.getFloat("c_real"), 0.0);
            check("typ.bool", r.getBoolean("c_bool"));
            eqs("typ.date", "2021-03-14", r.getDate("c_date").toString());
            eqs("typ.time", "13:45:30", r.getTime("c_time").toString());
            eqs("typ.timestamp", "2021-03-14 13:45:30.0", r.getTimestamp("c_ts").toString());
            check("typ.vbin", Arrays.equals(vbin, r.getBytes("c_vbin")));
            check("typ.blob", Arrays.equals(blob, r.getBytes("c_blob")));
            eqi("typ.blobLength", 5, (int) r.getBlob("c_blob").length());
            eqs("typ.clob", "clobtext", r.getString("c_clob"));
            eqs("typ.clobSub", "lob", r.getClob("c_clob").getSubString(2, 3));
            eqs("typ.uuidString", "00000000-0000-0000-0000-0000000000ff", r.getString("c_uuid"));
            check("typ.uuidObject", uuid.equals(r.getObject("c_uuid")));
            check("typ.uuidObjectType", r.getObject("c_uuid") instanceof UUID);
            Array arr = r.getArray("c_arr");
            Object[] av = (Object[]) arr.getArray();
            eqi("typ.arrayLen", 3, av.length);
            eqi("typ.arrayElem0", 10, ((Integer) av[0]).intValue());
            eqi("typ.arrayElem2", 30, ((Integer) av[2]).intValue());
            check("typ.arrayElemType", av[1] instanceof Integer);
        }

        // JSON type (literal insert -> compact canonical text)
        ddl("CREATE TABLE jtab (id INT, data JSON)");
        ddl("INSERT INTO jtab VALUES (1, JSON '{\"a\":1,\"b\":[2,3]}')");
        eqs("typ.json", "{\"a\":1,\"b\":[2,3]}", qStr("SELECT data FROM jtab"));

        ddl("DROP TABLE jtab");
        ddl("DROP TABLE typ");
    }

    // ---------------------------------------------------------------- constraints
    static void testConstraints() throws Exception {
        ddl("CREATE TABLE con ("
                + "id INT PRIMARY KEY, "
                + "name VARCHAR(10) NOT NULL UNIQUE, "
                + "age INT CHECK (age >= 0), "
                + "kind VARCHAR(5) DEFAULT 'std')");
        ddl("INSERT INTO con(id, name, age) VALUES (1, 'alice', 30)");

        // DEFAULT applied
        eqs("con.default", "std", qStr("SELECT kind FROM con WHERE id = 1"));

        // PRIMARY KEY duplicate -> 23505
        expectState("con.pkDup", "23505",
                () -> exec("INSERT INTO con(id, name, age) VALUES (1, 'bob', 5)"));
        // UNIQUE duplicate -> 23505
        expectState("con.uniqueDup", "23505",
                () -> exec("INSERT INTO con(id, name, age) VALUES (2, 'alice', 5)"));
        // NOT NULL -> 23502
        expectState("con.notNull", "23502",
                () -> exec("INSERT INTO con(id, name, age) VALUES (3, NULL, 5)"));
        // CHECK -> 23513
        expectState("con.check", "23513",
                () -> exec("INSERT INTO con(id, name, age) VALUES (4, 'carol', -1)"));
        // value too long -> 22001
        expectState("con.valueTooLong", "22001",
                () -> exec("INSERT INTO con(id, name, age) VALUES (5, 'thisistoolong', 5)"));

        // FOREIGN KEY
        ddl("CREATE TABLE par (pid INT PRIMARY KEY)");
        ddl("INSERT INTO par VALUES (1)");
        ddl("CREATE TABLE chi (cid INT PRIMARY KEY, pid INT REFERENCES par(pid))");
        eqi("con.fkValidInsert", 1, exec("INSERT INTO chi VALUES (1, 1)"));
        // FK violation on insert -> 23506
        expectState("con.fkInsert", "23506",
                () -> exec("INSERT INTO chi VALUES (2, 99)"));
        // FK violation on delete parent -> 23503
        expectState("con.fkDelete", "23503",
                () -> exec("DELETE FROM par WHERE pid = 1"));

        // syntax / object errors
        expectState("con.tableNotFound", "42S02", () -> qLong("SELECT 1 FROM nosuchtable"));
        expectState("con.columnNotFound", "42S22", () -> qLong("SELECT nosuchcol FROM con"));
        expectState("con.syntax", "42001", () -> ddl("SELORZ bad syntax"));
        expectState("con.alreadyExists", "42S01", () -> ddl("CREATE TABLE con (x INT)"));
        expectState("con.divZero", "22012", () -> qLong("SELECT 1/0"));

        ddl("DROP TABLE chi");
        ddl("DROP TABLE par");
        ddl("DROP TABLE con");
    }

    // ---------------------------------------------------------------- built-in functions
    static void testFunctions() throws Exception {
        eqs("fn.concat", "abc", qStr("SELECT CONCAT('a','b','c')"));
        eqs("fn.substringStd", "ell", qStr("SELECT SUBSTRING('hello' FROM 2 FOR 3)"));
        eqs("fn.substringComma", "ell", qStr("SELECT SUBSTRING('hello', 2, 3)"));
        eqs("fn.upper", "ABC", qStr("SELECT UPPER('abc')"));
        eqs("fn.lower", "abc", qStr("SELECT LOWER('ABC')"));
        eqi("fn.length", 5, qLong("SELECT LENGTH('hello')"));
        eqi("fn.charLength", 5, qLong("SELECT CHAR_LENGTH('hello')"));
        eqs("fn.replace", "aXcaXc", qStr("SELECT REPLACE('abcabc','b','X')"));
        eqs("fn.trim", "hi", qStr("SELECT TRIM('  hi  ')"));
        eqs("fn.lpad", "00007", qStr("SELECT LPAD('7', 5, '0')"));
        eqi("fn.coalesce", 5, qLong("SELECT COALESCE(NULL, 5)"));
        eqi("fn.nullif", 0, rowCount("SELECT * FROM (SELECT NULLIF(3,3) AS x) t WHERE x IS NOT NULL"));
        eqs("fn.caseWhen", "y", qStr("SELECT CASE WHEN 1 = 1 THEN 'y' ELSE 'n' END"));
        eqs("fn.caseSearch", "big", qStr("SELECT CASE WHEN 10 > 5 THEN 'big' ELSE 'small' END"));
        eqi("fn.castStrToInt", 123, qLong("SELECT CAST('123' AS INT)"));
        eqs("fn.castIntToStr", "456", qStr("SELECT CAST(456 AS VARCHAR)"));
        eqi("fn.abs", 7, qLong("SELECT ABS(-7)"));
        eqs("fn.round", "3.14", qStr("SELECT ROUND(3.14159, 2)"));
        eqi("fn.ceil", 4, qLong("SELECT CEIL(3.2)"));
        eqi("fn.floor", 3, qLong("SELECT FLOOR(3.9)"));
        eqi("fn.mod", 1, qLong("SELECT MOD(7, 3)"));
        eqi("fn.power", 8, qLong("SELECT CAST(POWER(2, 3) AS INT)"));
        eqi("fn.greatest", 9, qLong("SELECT GREATEST(3, 9, 5)"));
        eqi("fn.least", 3, qLong("SELECT LEAST(3, 9, 5)"));
        eqs("fn.extractYear", "2021", qStr("SELECT EXTRACT(YEAR FROM DATE '2021-03-14')"));

        // CURRENT_TIMESTAMP is non-deterministic in value, but must be a non-null Timestamp
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT CURRENT_TIMESTAMP")) {
            r.next();
            Object ts = r.getObject(1);
            check("fn.currentTimestampNotNull", ts != null);
            check("fn.currentTimestampType", r.getTimestamp(1) instanceof Timestamp);
        }
    }

    // ---------------------------------------------------------------- sequences
    static void testSequences() throws Exception {
        ddl("CREATE SEQUENCE seq START WITH 10 INCREMENT BY 5");
        eqi("seq.next1", 10, qLong("SELECT NEXT VALUE FOR seq"));
        eqi("seq.next2", 15, qLong("SELECT NEXT VALUE FOR seq"));
        eqi("seq.current", 15, qLong("SELECT CURRENT VALUE FOR seq"));
        eqi("seq.next3", 20, qLong("SELECT NEXT VALUE FOR seq"));
        ddl("DROP SEQUENCE seq");

        // IDENTITY / AUTO_INCREMENT
        ddl("CREATE TABLE auto (id INT AUTO_INCREMENT PRIMARY KEY, n VARCHAR(10))");
        exec("INSERT INTO auto(n) VALUES ('a')");
        exec("INSERT INTO auto(n) VALUES ('b')");
        eqi("seq.autoInc1", 1, qLong("SELECT id FROM auto WHERE n = 'a'"));
        eqi("seq.autoInc2", 2, qLong("SELECT id FROM auto WHERE n = 'b'"));
        ddl("DROP TABLE auto");
    }

    // ---------------------------------------------------------------- MERGE
    static void testMerge() throws Exception {
        // H2 legacy MERGE ... KEY(...) VALUES(...)
        ddl("CREATE TABLE mg (id INT PRIMARY KEY, n VARCHAR(10))");
        ddl("INSERT INTO mg VALUES (1, 'orig')");
        exec("MERGE INTO mg KEY(id) VALUES (1, 'updated')");
        eqs("merge.legacyUpdate", "updated", qStr("SELECT n FROM mg WHERE id = 1"));
        exec("MERGE INTO mg KEY(id) VALUES (2, 'inserted')");
        eqi("merge.legacyInsert", 2, rowCount("SELECT * FROM mg"));
        ddl("DROP TABLE mg");

        // Standard MERGE ... USING ... ON ... WHEN MATCHED / NOT MATCHED
        ddl("CREATE TABLE tgt (id INT PRIMARY KEY, val INT)");
        ddl("INSERT INTO tgt VALUES (1, 10), (2, 20)");
        ddl("CREATE TABLE src (id INT, val INT)");
        ddl("INSERT INTO src VALUES (1, 100), (3, 300)");
        exec("MERGE INTO tgt USING src ON tgt.id = src.id "
                + "WHEN MATCHED THEN UPDATE SET tgt.val = src.val "
                + "WHEN NOT MATCHED THEN INSERT (id, val) VALUES (src.id, src.val)");
        eqi("merge.standardCount", 3, rowCount("SELECT * FROM tgt"));
        eqi("merge.standardSum", 420, qLong("SELECT SUM(val) FROM tgt"));
        eqi("merge.standardUpdated", 100, qLong("SELECT val FROM tgt WHERE id = 1"));
        eqi("merge.standardUntouched", 20, qLong("SELECT val FROM tgt WHERE id = 2"));
        eqi("merge.standardInserted", 300, qLong("SELECT val FROM tgt WHERE id = 3"));
        ddl("DROP TABLE tgt");
        ddl("DROP TABLE src");
    }

    // ---------------------------------------------------------------- tool: RunScript
    static void testToolRunScript() throws Exception {
        // (a) static execute(Connection, Reader)
        String script = "CREATE TABLE rsc (id INT, n VARCHAR(10));\n"
                + "INSERT INTO rsc VALUES (1, 'x'), (2, 'y');\n";
        RunScript.execute(conn, new StringReader(script));
        eqi("tool.runScriptReaderRows", 2, rowCount("SELECT * FROM rsc"));
        eqs("tool.runScriptReaderValue", "y", qStr("SELECT n FROM rsc WHERE id = 2"));
        ddl("DROP TABLE rsc");

        // (b) static execute(url, user, pw, file, charset, continueOnError) from a /tmp .sql file
        File sql = File.createTempFile("h2carpet_runscript", ".sql");
        java.nio.file.Files.write(sql.toPath(),
                ("CREATE TABLE rsc2 (id INT, v INT);\n"
                        + "INSERT INTO rsc2 VALUES (5, 50), (6, 60);\n").getBytes(StandardCharsets.UTF_8));
        RunScript.execute(URL, "sa", "", sql.getAbsolutePath(), StandardCharsets.UTF_8, false);
        eqi("tool.runScriptFileRows", 2, rowCount("SELECT * FROM rsc2"));
        eqi("tool.runScriptFileSum", 110, qLong("SELECT SUM(v) FROM rsc2"));
        sql.delete();

        // (c) instance runTool(...) on a fresh script file
        File sql2 = File.createTempFile("h2carpet_runtool", ".sql");
        java.nio.file.Files.write(sql2.toPath(),
                "INSERT INTO rsc2 VALUES (7, 70);\n".getBytes(StandardCharsets.UTF_8));
        new RunScript().runTool("-url", URL, "-user", "sa", "-password", "",
                "-script", sql2.getAbsolutePath());
        eqi("tool.runToolRows", 3, rowCount("SELECT * FROM rsc2"));
        sql2.delete();
        ddl("DROP TABLE rsc2");
    }

    // ---------------------------------------------------------------- tool: Script (export)
    static void testToolScript() throws Exception {
        ddl("CREATE TABLE expt (id INT PRIMARY KEY, n VARCHAR(10))");
        ddl("INSERT INTO expt VALUES (1, 'alpha'), (2, 'beta')");

        // (a) static process(Connection, file, opt1, opt2) -- does not close the connection
        File out1 = File.createTempFile("h2carpet_export_conn", ".sql");
        Script.process(conn, out1.getAbsolutePath(), "", "");
        String body1 = new String(java.nio.file.Files.readAllBytes(out1.toPath()),
                StandardCharsets.UTF_8);
        check("tool.scriptConnHasCreate",
                body1.contains("CREATE") && body1.toUpperCase(Locale.US).contains("TABLE")
                        && body1.contains("EXPT"));
        check("tool.scriptConnHasInsert", body1.toUpperCase(Locale.US).contains("INSERT INTO")
                && body1.contains("alpha"));
        check("tool.scriptConnNonEmpty", out1.length() > 0);
        out1.delete();

        // (b) instance runTool(-url ... -script file)
        File out2 = File.createTempFile("h2carpet_export_url", ".sql");
        new Script().runTool("-url", URL, "-user", "sa", "-password", "",
                "-script", out2.getAbsolutePath());
        String body2 = new String(java.nio.file.Files.readAllBytes(out2.toPath()),
                StandardCharsets.UTF_8);
        check("tool.scriptUrlHasTable", body2.contains("EXPT"));
        check("tool.scriptUrlHasBeta", body2.contains("beta"));
        out2.delete();

        // (c) round-trip: export then re-import into a fresh table set
        File rt = File.createTempFile("h2carpet_roundtrip", ".sql");
        // build an isolated table, export only it via a dedicated DB
        ddl("DROP TABLE expt");
        ddl("CREATE TABLE rtt (id INT, v INT)");
        ddl("INSERT INTO rtt VALUES (1, 11), (2, 22)");
        Script.process(conn, rt.getAbsolutePath(), "", "");
        ddl("DROP TABLE rtt");
        check("tool.roundTripGone", !tableExists("RTT"));
        RunScript.execute(conn, new java.io.FileReader(rt));
        check("tool.roundTripRestored", tableExists("RTT"));
        eqi("tool.roundTripSum", 33, (int) qLong("SELECT SUM(v) FROM rtt"));
        rt.delete();
        ddl("DROP TABLE rtt");
    }

    // ---------------------------------------------------------------- tool: Csv
    static void testToolCsv() throws Exception {
        // (a) write a ResultSet to CSV, read it back
        ddl("CREATE TABLE csv_src (id INT, n VARCHAR(10))");
        ddl("INSERT INTO csv_src VALUES (1, 'aa'), (2, 'bb'), (3, 'cc')");
        File csv = File.createTempFile("h2carpet_data", ".csv");

        int written;
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT id, n FROM csv_src ORDER BY id")) {
            written = new Csv().write(csv.getAbsolutePath(), r, "UTF-8");
        }
        eqi("tool.csvWriteRows", 3, written);

        try (ResultSet r = new Csv().read(csv.getAbsolutePath(), null, "UTF-8")) {
            ResultSetMetaData md = r.getMetaData();
            eqi("tool.csvColCount", 2, md.getColumnCount());
            eqs("tool.csvColName0", "ID", md.getColumnLabel(1));
            eqs("tool.csvColName1", "N", md.getColumnLabel(2));
            r.next();
            eqs("tool.csvRow0Id", "1", r.getString(1));
            eqs("tool.csvRow0N", "aa", r.getString(2));
            r.next();
            r.next();
            eqs("tool.csvRow2N", "cc", r.getString("N"));
            check("tool.csvExhausted", !r.next());
        }
        csv.delete();
        ddl("DROP TABLE csv_src");

        // (b) SimpleResultSet -> CSV -> read back (exercises SimpleResultSet API)
        SimpleResultSet srs = new SimpleResultSet();
        srs.addColumn("K", Types.INTEGER, 10, 0);
        srs.addColumn("LABEL", Types.VARCHAR, 20, 0);
        srs.addRow(100, "hundred");
        srs.addRow(200, "two-hundred");
        File csv2 = File.createTempFile("h2carpet_simple", ".csv");
        int written2 = new Csv().write(csv2.getAbsolutePath(), srs, "UTF-8");
        eqi("tool.csvSimpleWrite", 2, written2);
        try (ResultSet r = new Csv().read(csv2.getAbsolutePath(), null, "UTF-8")) {
            r.next();
            eqs("tool.csvSimpleK", "100", r.getString("K"));
            eqs("tool.csvSimpleLabel", "hundred", r.getString("LABEL"));
            r.next();
            eqs("tool.csvSimpleK2", "200", r.getString("K"));
        }
        csv2.delete();

        // (c) custom field separator option
        ddl("CREATE TABLE csv3 (a INT, b INT)");
        ddl("INSERT INTO csv3 VALUES (7, 8)");
        File csv3 = File.createTempFile("h2carpet_sep", ".csv");
        Csv writer = new Csv();
        writer.setFieldSeparatorWrite(";");
        try (Statement s = conn.createStatement();
                ResultSet r = s.executeQuery("SELECT a, b FROM csv3")) {
            writer.write(csv3.getAbsolutePath(), r, "UTF-8");
        }
        String content = new String(java.nio.file.Files.readAllBytes(csv3.toPath()),
                StandardCharsets.UTF_8);
        check("tool.csvCustomSep", content.contains("\"7\";\"8\""));
        csv3.delete();
        ddl("DROP TABLE csv3");
    }

    // ---------------------------------------------------------------- tool: Shell (non-interactive)
    static void testToolShell() throws Exception {
        ddl("CREATE TABLE sh (id INT, n VARCHAR(10))");
        ddl("INSERT INTO sh VALUES (1, 'shellval')");

        PrintStream orig = System.out;
        java.io.ByteArrayOutputStream baos = new java.io.ByteArrayOutputStream();
        String out;
        // Use URL form (Shell closes its own connection; main conn untouched thanks to DB_CLOSE_DELAY=-1)
        try {
            System.setOut(new PrintStream(baos, true, "UTF-8"));
            Shell shell = new Shell();
            shell.setIn(new java.io.ByteArrayInputStream(new byte[0]));
            shell.runTool("-url", URL, "-user", "sa", "-password", "",
                    "-sql", "SELECT n FROM sh WHERE id = 1; SELECT COUNT(*) AS C FROM sh;");
        } finally {
            System.setOut(orig);
        }
        out = baos.toString("UTF-8");
        check("tool.shellValuePrinted", out.contains("shellval"));
        check("tool.shellHeaderPrinted", out.contains("N"));
        check("tool.shellSecondQuery", out.contains("1 row") || out.contains("(1 row"));

        // Verify connection still usable after the shell tool ran
        eqi("tool.shellConnAlive", 1, rowCount("SELECT * FROM sh"));
        ddl("DROP TABLE sh");
    }

    // ---------------------------------------------------------------- metadata helpers
    static boolean tableExists(String name) throws SQLException {
        try (ResultSet r = conn.getMetaData().getTables(null, "PUBLIC", name,
                new String[] {"TABLE"})) {
            return r.next();
        }
    }

    static boolean columnExists(String table, String column) throws SQLException {
        try (ResultSet r = conn.getMetaData().getColumns(null, "PUBLIC", table, column)) {
            return r.next();
        }
    }

    static boolean indexExists(String table, String indexName) throws SQLException {
        try (ResultSet r = conn.getMetaData().getIndexInfo(null, "PUBLIC", table, false, false)) {
            while (r.next()) {
                if (indexName.equalsIgnoreCase(r.getString("INDEX_NAME"))) {
                    return true;
                }
            }
            return false;
        }
    }
}
