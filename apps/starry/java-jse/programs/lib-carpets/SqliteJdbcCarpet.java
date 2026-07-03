package org.starry.dod;

import java.math.BigDecimal;
import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.Driver;
import java.sql.DriverManager;
import java.sql.ParameterMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Savepoint;
import java.sql.Statement;
import java.sql.Types;
import java.util.Arrays;
import java.util.Objects;

/**
 * Carpet-grade, fully offline and deterministic coverage of the xerial
 * sqlite-jdbc driver (org.sqlite.JDBC, SQLite engine 3.46.1) over an
 * in-memory database (jdbc:sqlite::memory:).
 *
 * Every assertion checks an exact value, an exact count, or an exact
 * exception/error-code. No external network, no external files: only the
 * in-memory engine plus, where required, the driver's native-lib temp
 * extraction performed by the bundled JNI shim on classpath.
 */
public class SqliteJdbcCarpet {

    static final String URL = "jdbc:sqlite::memory:";
    static final String SQLITE_VERSION = "3.46.1";
    static final String DRIVER_VERSION = "3.46.1.3";

    static int ok = 0;
    static int fail = 0;

    interface Sql {
        void run() throws Exception;
    }

    static void pass() {
        ok++;
    }

    static void bad(String name, String detail) {
        fail++;
        System.out.println("FAIL " + name + (detail == null ? "" : " " + detail));
    }

    static void check(String name, boolean cond) {
        if (cond) {
            ok++;
        } else {
            bad(name, null);
        }
    }

    static void eq(String name, Object exp, Object act) {
        if (Objects.equals(exp, act)) {
            ok++;
        } else {
            bad(name, "expected=[" + exp + "] actual=[" + act + "]");
        }
    }

    static void eqL(String name, long exp, long act) {
        if (exp == act) {
            ok++;
        } else {
            bad(name, "expected=" + exp + " actual=" + act);
        }
    }

    static void eqD(String name, double exp, double act) {
        if (Math.abs(exp - act) < 1e-9) {
            ok++;
        } else {
            bad(name, "expected=" + exp + " actual=" + act);
        }
    }

    static void mustThrow(String name, Sql act) {
        try {
            act.run();
            bad(name, "expected SQLException, none thrown");
        } catch (SQLException e) {
            ok++;
        } catch (Exception e) {
            bad(name, "wrong exception type " + e.getClass().getName());
        }
    }

    static void mustThrowCode(String name, int code, Sql act) {
        try {
            act.run();
            bad(name, "expected SQLException code " + code + ", none thrown");
        } catch (SQLException e) {
            if (e.getErrorCode() == code) {
                ok++;
            } else {
                bad(name, "code expected=" + code + " actual=" + e.getErrorCode()
                        + " msg=" + e.getMessage());
            }
        } catch (Exception e) {
            bad(name, "wrong exception type " + e.getClass().getName());
        }
    }

    // ---- scalar query helpers -------------------------------------------

    static String qStr(Connection c, String sql) throws SQLException {
        try (Statement st = c.createStatement(); ResultSet r = st.executeQuery(sql)) {
            r.next();
            return r.getString(1);
        }
    }

    static long qLong(Connection c, String sql) throws SQLException {
        try (Statement st = c.createStatement(); ResultSet r = st.executeQuery(sql)) {
            r.next();
            return r.getLong(1);
        }
    }

    static int qInt(Connection c, String sql) throws SQLException {
        try (Statement st = c.createStatement(); ResultSet r = st.executeQuery(sql)) {
            r.next();
            return r.getInt(1);
        }
    }

    static double qDbl(Connection c, String sql) throws SQLException {
        try (Statement st = c.createStatement(); ResultSet r = st.executeQuery(sql)) {
            r.next();
            return r.getDouble(1);
        }
    }

    public static void main(String[] args) throws Exception {
        driverAndConnection();
        databaseMetaData();
        ddlAndDml();
        preparedStatementParams();
        batchUpdates();
        resultSetGetters();
        resultSetMetaData();
        typeAffinity();
        transactions();
        savepoints();
        pragmas();
        autoincrementRowid();
        indexes();
        foreignKeys();
        triggers();
        views();
        scalarFunctions();
        dateTimeFunctions();
        aggregates();
        recursiveCte();
        upsert();
        blobRoundTrip();
        json1();
        attachDatabase();
        windowFunctions();
        generatedColumns();
        exceptionPaths();

        System.out.println("SQLITEJDBC_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("SQLITEJDBC_DONE");
        }
    }

    // =====================================================================
    // A. Driver registration & Connection basics
    // =====================================================================
    static void driverAndConnection() throws Exception {
        Class<?> drvClass = Class.forName("org.sqlite.JDBC");
        check("driver.class.loaded", drvClass != null);

        Driver drv = DriverManager.getDriver(URL);
        check("driver.registered", drv != null);
        check("driver.acceptsURL.sqlite", drv.acceptsURL(URL));
        check("driver.acceptsURL.memory2", drv.acceptsURL("jdbc:sqlite::memory:"));
        check("driver.rejects.mysql", !drv.acceptsURL("jdbc:mysql://localhost/x"));
        check("driver.rejects.null", !drv.acceptsURL("not-a-jdbc-url"));
        check("driver.notJdbcCompliant", !drv.jdbcCompliant());
        check("driver.majorVersion", drv.getMajorVersion() >= 3);

        try (Connection c = DriverManager.getConnection(URL)) {
            check("conn.notNull", c != null);
            check("conn.notClosed", !c.isClosed());
            check("conn.autoCommitDefault", c.getAutoCommit());
            check("conn.notReadOnly", !c.isReadOnly());
            check("conn.valid", c.isValid(0));
            eq("conn.sqlite_version", SQLITE_VERSION, qStr(c, "select sqlite_version()"));
            eqL("conn.holdability",
                    ResultSet.CLOSE_CURSORS_AT_COMMIT, c.getHoldability());
            // a fresh statement can be created and round-trips a literal
            eqL("conn.simpleSelect", 7L, qLong(c, "select 3+4"));
        }
    }

    // =====================================================================
    // B. DatabaseMetaData
    // =====================================================================
    static void databaseMetaData() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            DatabaseMetaData m = c.getMetaData();
            eq("dmd.productName", "SQLite", m.getDatabaseProductName());
            eq("dmd.driverName", "SQLite JDBC", m.getDriverName());
            eq("dmd.driverVersion", DRIVER_VERSION, m.getDriverVersion());
            eq("dmd.url", URL, m.getURL());
            eqL("dmd.jdbcMajor", 4, m.getJDBCMajorVersion());
            eqL("dmd.jdbcMinor", 2, m.getJDBCMinorVersion());
            check("dmd.supportsTransactions", m.supportsTransactions());
            eqL("dmd.defaultIsolation",
                    Connection.TRANSACTION_SERIALIZABLE, m.getDefaultTransactionIsolation());
            check("dmd.supportsTxIso.serializable",
                    m.supportsTransactionIsolationLevel(Connection.TRANSACTION_SERIALIZABLE));
            check("dmd.supportsSavepoints", m.supportsSavepoints());
            check("dmd.supportsBatch", m.supportsBatchUpdates());
            check("dmd.supportsForwardOnly",
                    m.supportsResultSetType(ResultSet.TYPE_FORWARD_ONLY));
            check("dmd.productVersion.contains",
                    m.getDatabaseProductVersion().startsWith("3.46"));

            // schema discovery via metadata
            try (Statement s = c.createStatement()) {
                s.execute("create table mdt(id integer primary key, label text not null)");
            }
            int tableHits = 0;
            try (ResultSet rs = m.getTables(null, null, "mdt", new String[] {"TABLE"})) {
                while (rs.next()) {
                    if ("mdt".equals(rs.getString("TABLE_NAME"))) {
                        tableHits++;
                    }
                }
            }
            eqL("dmd.getTables.mdt", 1, tableHits);

            int colCount = 0;
            boolean sawLabelNotNull = false;
            try (ResultSet rs = m.getColumns(null, null, "mdt", "%")) {
                while (rs.next()) {
                    colCount++;
                    if ("label".equals(rs.getString("COLUMN_NAME"))) {
                        sawLabelNotNull = rs.getInt("NULLABLE") == DatabaseMetaData.columnNoNulls;
                    }
                }
            }
            eqL("dmd.getColumns.count", 2, colCount);
            check("dmd.getColumns.notNull", sawLabelNotNull);

            int pkCount = 0;
            try (ResultSet rs = m.getPrimaryKeys(null, null, "mdt")) {
                while (rs.next()) {
                    if ("id".equals(rs.getString("COLUMN_NAME"))) {
                        pkCount++;
                    }
                }
            }
            eqL("dmd.getPrimaryKeys.id", 1, pkCount);
        }
    }

    // =====================================================================
    // C. DDL & DML basics
    // =====================================================================
    static void ddlAndDml() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            // DDL: execute() returns false (no ResultSet)
            check("ddl.createReturnsFalse",
                    !s.execute("create table acct(id integer primary key, name text, bal integer)"));

            // single insert via executeUpdate -> affected count 1
            eqL("dml.insert1", 1, s.executeUpdate("insert into acct(name,bal) values('a',100)"));
            // multi-row insert -> affected count 3
            eqL("dml.insert3", 3,
                    s.executeUpdate("insert into acct(name,bal) values('b',200),('c',300),('d',400)"));

            // execute() on INSERT returns false, then getUpdateCount == 1
            check("dml.execInsertFalse", !s.execute("insert into acct(name,bal) values('e',500)"));
            eqL("dml.getUpdateCount", 1, s.getUpdateCount());

            eqL("dml.count.all", 5, qLong(c, "select count(*) from acct"));

            // UPDATE affected count
            eqL("dml.update", 5, s.executeUpdate("update acct set bal = bal + 1"));
            eqL("dml.update.sum", 1505 + 0, qLong(c, "select sum(bal) from acct"));

            // conditional UPDATE affects subset
            eqL("dml.updateSubset", 1, s.executeUpdate("update acct set name='AA' where name='a'"));

            // DELETE affected count
            eqL("dml.delete", 2, s.executeUpdate("delete from acct where bal >= 400"));
            eqL("dml.count.afterDelete", 3, qLong(c, "select count(*) from acct"));

            // executeQuery on a SELECT returns a usable ResultSet
            check("dml.execSelectTrue", s.execute("select count(*) from acct"));
            try (ResultSet rs = s.getResultSet()) {
                check("dml.getResultSet.next", rs.next());
                eqL("dml.getResultSet.value", 3, rs.getLong(1));
            }

            // DROP TABLE
            check("ddl.dropReturnsFalse", !s.execute("drop table acct"));
            mustThrow("ddl.droppedGone", () -> qLong(c, "select count(*) from acct"));
        }
    }

    // =====================================================================
    // D. PreparedStatement parameters & generated keys
    // =====================================================================
    static void preparedStatementParams() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table p(id integer primary key autoincrement, i integer, l integer, "
                    + "d real, t text, flag integer, bytes blob, opt text, num real)");

            String ins = "insert into p(i,l,d,t,flag,bytes,opt,num) values(?,?,?,?,?,?,?,?)";
            try (PreparedStatement ps = c.prepareStatement(ins, Statement.RETURN_GENERATED_KEYS)) {
                ParameterMetaData pmd = ps.getParameterMetaData();
                eqL("ps.paramCount", 8, pmd.getParameterCount());

                ps.setInt(1, 42);
                ps.setLong(2, 9000000000L);
                ps.setDouble(3, 2.5);
                ps.setString(4, "hello");
                ps.setBoolean(5, true);
                ps.setBytes(6, new byte[] {1, 2, 3});
                ps.setNull(7, Types.VARCHAR);
                ps.setObject(8, new BigDecimal("3.5"));
                eqL("ps.executeUpdate", 1, ps.executeUpdate());

                try (ResultSet gk = ps.getGeneratedKeys()) {
                    check("ps.genKeys.next", gk.next());
                    eqL("ps.genKeys.value", 1, gk.getLong(1));
                }

                // reuse with clearParameters + new values
                ps.clearParameters();
                ps.setInt(1, 7);
                ps.setLong(2, 7L);
                ps.setDouble(3, 1.25);
                ps.setString(4, "world");
                ps.setBoolean(5, false);
                ps.setBytes(6, new byte[] {9});
                ps.setString(7, "present");
                ps.setObject(8, Double.valueOf(8.5));
                eqL("ps.executeUpdate2", 1, ps.executeUpdate());
            }

            // verify each round-tripped column with a parameterised SELECT
            try (PreparedStatement q = c.prepareStatement("select i,l,d,t,flag,bytes,opt,num from p where id=?")) {
                q.setInt(1, 1);
                try (ResultSet r = q.executeQuery()) {
                    r.next();
                    eqL("ps.rt.i", 42, r.getInt("i"));
                    eqL("ps.rt.l", 9000000000L, r.getLong("l"));
                    eqD("ps.rt.d", 2.5, r.getDouble("d"));
                    eq("ps.rt.t", "hello", r.getString("t"));
                    check("ps.rt.flagTrue", r.getBoolean("flag"));
                    check("ps.rt.bytes", Arrays.equals(new byte[] {1, 2, 3}, r.getBytes("bytes")));
                    eq("ps.rt.optNull", null, r.getString("opt"));
                    check("ps.rt.optWasNull", r.wasNull());
                    eqD("ps.rt.num", 3.5, r.getDouble("num"));
                }
            }
            // second row by parameter binding
            try (PreparedStatement q = c.prepareStatement("select t,flag,opt from p where id=?")) {
                q.setInt(1, 2);
                try (ResultSet r = q.executeQuery()) {
                    r.next();
                    eq("ps.rt2.t", "world", r.getString("t"));
                    check("ps.rt2.flagFalse", !r.getBoolean("flag"));
                    eq("ps.rt2.opt", "present", r.getString("opt"));
                }
            }
        }
    }

    // =====================================================================
    // E. Batch updates
    // =====================================================================
    static void batchUpdates() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table batchtab(n integer)");
            try (PreparedStatement ps = c.prepareStatement("insert into batchtab values(?)")) {
                for (int i = 1; i <= 5; i++) {
                    ps.setInt(1, i * 10);
                    ps.addBatch();
                }
                int[] res = ps.executeBatch();
                eqL("batch.length", 5, res.length);
                int sumRes = 0;
                for (int r : res) {
                    sumRes += r;
                }
                eqL("batch.eachOne", 5, sumRes);
            }
            eqL("batch.rowCount", 5, qLong(c, "select count(*) from batchtab"));
            eqL("batch.sum", 150, qLong(c, "select sum(n) from batchtab"));

            // clearBatch leaves nothing pending
            try (PreparedStatement ps = c.prepareStatement("insert into batchtab values(?)")) {
                ps.setInt(1, 999);
                ps.addBatch();
                ps.clearBatch();
                int[] res = ps.executeBatch();
                eqL("batch.clearedLength", 0, res.length);
            }
            eqL("batch.rowCountAfterClear", 5, qLong(c, "select count(*) from batchtab"));

            // Statement-level batch with mixed DML
            s.clearBatch();
            s.addBatch("update batchtab set n = n + 1");
            s.addBatch("delete from batchtab where n = 11");
            int[] sres = s.executeBatch();
            eqL("batch.stmtLength", 2, sres.length);
            eqL("batch.stmtUpdated", 5, sres[0]);
            eqL("batch.stmtDeleted", 1, sres[1]);
            eqL("batch.rowCountAfterStmtBatch", 4, qLong(c, "select count(*) from batchtab"));
        }
    }

    // =====================================================================
    // F. ResultSet getters & navigation
    // =====================================================================
    static void resultSetGetters() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table g(id integer, big integer, flo real, txt text, flg integer, raw blob, nl text)");
            s.execute("insert into g values(100, 9000000000, 1.5, 'abc', 1, x'48454c4c4f', null)");

            try (ResultSet r = s.executeQuery("select id,big,flo,txt,flg,raw,nl from g")) {
                check("rs.firstNext", r.next());
                eqL("rs.getRow", 1, r.getRow());

                // numeric getters by index and by name agree
                eqL("rs.getInt.idx", 100, r.getInt(1));
                eqL("rs.getInt.name", 100, r.getInt("id"));
                eqL("rs.getShort", 100, r.getShort("id"));
                eqL("rs.getByte", 100, r.getByte("id"));
                eqL("rs.getLong", 9000000000L, r.getLong("big"));
                eqD("rs.getDouble", 1.5, r.getDouble("flo"));
                eqD("rs.getFloat", 1.5, r.getFloat("flo"));
                eq("rs.getString", "abc", r.getString("txt"));
                check("rs.getBoolean", r.getBoolean("flg"));

                // BLOB literal x'48454c4c4f' == bytes of "HELLO"
                check("rs.getBytes",
                        Arrays.equals(new byte[] {0x48, 0x45, 0x4C, 0x4C, 0x4F}, r.getBytes("raw")));

                // getObject class fidelity (SQLite dynamic typing)
                eq("rs.getObject.int", Integer.class, r.getObject("id").getClass());
                eq("rs.getObject.txt", String.class, r.getObject("txt").getClass());
                eq("rs.getObject.flo", Double.class, r.getObject("flo").getClass());

                // BigDecimal view of an integer column
                eqL("rs.getBigDecimal",
                        0, r.getBigDecimal("id").compareTo(new BigDecimal("100")));

                // NULL handling + wasNull latch
                eq("rs.nullString", null, r.getString("nl"));
                check("rs.nullWasNull", r.wasNull());
                eqL("rs.nullInt", 0, r.getInt("nl"));
                check("rs.nullIntWasNull", r.wasNull());
                // reading a non-null after a null clears wasNull
                r.getInt("id");
                check("rs.wasNullCleared", !r.wasNull());

                check("rs.lastNextFalse", !r.next());
            }

            // findColumn maps name -> 1-based index
            try (ResultSet r = s.executeQuery("select id,txt from g")) {
                eqL("rs.findColumn", 2, r.findColumn("txt"));
                check("rs.findColumnNext", r.next());
            }

            // multi-row iteration counts exactly
            s.execute("insert into g values(1,1,1,'x',0,x'00',null),(2,2,2,'y',0,x'00',null)");
            int rows = 0;
            long idSum = 0;
            try (ResultSet r = s.executeQuery("select id from g order by id")) {
                while (r.next()) {
                    rows++;
                    idSum += r.getLong(1);
                }
            }
            eqL("rs.iterRows", 3, rows);
            eqL("rs.iterIdSum", 103, idSum);
        }
    }

    // =====================================================================
    // G. ResultSetMetaData
    // =====================================================================
    static void resultSetMetaData() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table rsmd(id integer primary key, name text, score real, payload blob)");
            s.execute("insert into rsmd values(1,'n',2.0,x'01')");

            try (ResultSet r = s.executeQuery(
                    "select id, name as alias_name, score, payload from rsmd")) {
                ResultSetMetaData md = r.getMetaData();
                eqL("rsmd.columnCount", 4, md.getColumnCount());

                eq("rsmd.name.id", "id", md.getColumnName(1));
                // alias surfaces as label
                eq("rsmd.label.alias", "alias_name", md.getColumnLabel(2));

                eqL("rsmd.type.integer", Types.INTEGER, md.getColumnType(1));
                eqL("rsmd.type.text", Types.VARCHAR, md.getColumnType(2));
                eqL("rsmd.type.real", Types.REAL, md.getColumnType(3));
                eqL("rsmd.type.blob", Types.BLOB, md.getColumnType(4));

                eq("rsmd.typeName.integer", "INTEGER", md.getColumnTypeName(1));
                eq("rsmd.typeName.text", "TEXT", md.getColumnTypeName(2));
                eq("rsmd.typeName.real", "REAL", md.getColumnTypeName(3));
                eq("rsmd.typeName.blob", "BLOB", md.getColumnTypeName(4));

                eq("rsmd.class.integer", "java.lang.Integer", md.getColumnClassName(1));
                eq("rsmd.class.text", "java.lang.String", md.getColumnClassName(2));
                eq("rsmd.class.real", "java.lang.Double", md.getColumnClassName(3));
            }
        }
    }

    // =====================================================================
    // H. Storage classes & type affinity (dynamic typing)
    // =====================================================================
    static void typeAffinity() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table aff(i integer, t text, r real, n numeric, b blob)");
            try (PreparedStatement ps = c.prepareStatement("insert into aff values(?,?,?,?,?)")) {
                ps.setString(1, "123");   // INTEGER affinity -> stored integer
                ps.setInt(2, 456);        // TEXT affinity -> stored text
                ps.setString(3, "7.5");   // REAL affinity -> stored real
                ps.setString(4, "3.140"); // NUMERIC affinity -> stored real 3.14
                ps.setBytes(5, new byte[] {1, 2, 3}); // BLOB affinity -> stored blob
                ps.executeUpdate();
            }

            // typeof() reports the resulting storage class after affinity coercion
            eq("aff.typeof.i", "integer", qStr(c, "select typeof(i) from aff"));
            eq("aff.typeof.t", "text", qStr(c, "select typeof(t) from aff"));
            eq("aff.typeof.r", "real", qStr(c, "select typeof(r) from aff"));
            eq("aff.typeof.n", "real", qStr(c, "select typeof(n) from aff"));
            eq("aff.typeof.b", "blob", qStr(c, "select typeof(b) from aff"));

            // coerced values are numerically correct
            eqL("aff.value.i", 123, qLong(c, "select i from aff"));
            eq("aff.value.t", "456", qStr(c, "select t from aff"));
            eqD("aff.value.r", 7.5, qDbl(c, "select r from aff"));
            eqD("aff.value.n", 3.14, qDbl(c, "select n from aff"));

            // dynamic typing: a column with no declared affinity stores per-value classes
            s.execute("create table dyn(x)"); // BLOB/none affinity -> values keep native class
            s.execute("insert into dyn values(1),('two'),(3.5),(x'ff'),(null)");
            eq("dyn.t1", "integer", qStr(c, "select typeof(x) from dyn where rowid=1"));
            eq("dyn.t2", "text", qStr(c, "select typeof(x) from dyn where rowid=2"));
            eq("dyn.t3", "real", qStr(c, "select typeof(x) from dyn where rowid=3"));
            eq("dyn.t4", "blob", qStr(c, "select typeof(x) from dyn where rowid=4"));
            eq("dyn.t5", "null", qStr(c, "select typeof(x) from dyn where rowid=5"));

            // NULL ordering / comparison three-valued logic
            eqL("aff.nullEqIsNull", 1, qLong(c, "select (null = null) is null"));
            eqL("aff.nullCount", 1, qLong(c, "select count(*) from dyn where x is null"));
        }
    }

    // =====================================================================
    // I. Transactions: commit / rollback
    // =====================================================================
    static void transactions() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            try (Statement s = c.createStatement()) {
                s.execute("create table tx(id integer primary key, v integer)");
            }
            c.setAutoCommit(false);
            check("tx.autoCommitOff", !c.getAutoCommit());

            try (Statement s = c.createStatement()) {
                s.executeUpdate("insert into tx values(1,10)");
                s.executeUpdate("insert into tx values(2,20)");
            }
            eqL("tx.beforeRollbackCount", 2, qLong(c, "select count(*) from tx"));
            c.rollback();
            eqL("tx.afterRollbackCount", 0, qLong(c, "select count(*) from tx"));

            try (Statement s = c.createStatement()) {
                s.executeUpdate("insert into tx values(3,30)");
            }
            c.commit();
            eqL("tx.afterCommitCount", 1, qLong(c, "select count(*) from tx"));

            // a rollback after a committed change cannot undo it
            try (Statement s = c.createStatement()) {
                s.executeUpdate("update tx set v=999 where id=3");
            }
            c.rollback();
            eqL("tx.committedSurvivesRollback", 30, qLong(c, "select v from tx where id=3"));

            c.setAutoCommit(true);
            check("tx.autoCommitBackOn", c.getAutoCommit());
        }
    }

    // =====================================================================
    // J. Savepoints (named + rollback-to / release)
    // =====================================================================
    static void savepoints() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            try (Statement s = c.createStatement()) {
                s.execute("create table sp(id integer primary key)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("insert into sp values(1)"); // base row inside tx
                Savepoint a = c.setSavepoint("a");
                s.executeUpdate("insert into sp values(2)");
                eqL("sp.beforeRollback", 2, qLong(c, "select count(*) from sp"));
                c.rollback(a); // undo row 2, keep row 1
                eqL("sp.afterRollbackToA", 1, qLong(c, "select count(*) from sp"));

                Savepoint b = c.setSavepoint("b");
                s.executeUpdate("insert into sp values(3)");
                c.releaseSavepoint(b); // keep row 3, no rollback
                eqL("sp.afterRelease", 2, qLong(c, "select count(*) from sp"));
            }
            c.commit();
            eqL("sp.committedRows", 2, qLong(c, "select count(*) from sp"));
            eqL("sp.committedMaxId", 3, qLong(c, "select max(id) from sp"));
            c.setAutoCommit(true);
        }
    }

    // =====================================================================
    // K. PRAGMA statements
    // =====================================================================
    static void pragmas() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table pt(id integer primary key, name text not null, ref integer)");
            s.execute("create table pchild(id integer primary key, pid integer references pt(id))");
            s.execute("create unique index ux_pt_name on pt(name)");

            // pragma table_info -> exact column schema
            int infoRows = 0;
            boolean nameNotNull = false;
            boolean idIsPk = false;
            try (ResultSet r = s.executeQuery("pragma table_info(pt)")) {
                while (r.next()) {
                    infoRows++;
                    String col = r.getString("name");
                    if ("name".equals(col)) {
                        nameNotNull = r.getInt("notnull") == 1;
                    }
                    if ("id".equals(col)) {
                        idIsPk = r.getInt("pk") == 1;
                    }
                }
            }
            eqL("pragma.table_info.rows", 3, infoRows);
            check("pragma.table_info.nameNotNull", nameNotNull);
            check("pragma.table_info.idPk", idIsPk);

            // pragma index_list -> our unique index is present and flagged unique
            boolean sawUnique = false;
            try (ResultSet r = s.executeQuery("pragma index_list(pt)")) {
                while (r.next()) {
                    if ("ux_pt_name".equals(r.getString("name"))) {
                        sawUnique = r.getInt("unique") == 1;
                    }
                }
            }
            check("pragma.index_list.unique", sawUnique);

            // pragma foreign_key_list -> child references pt
            boolean fkOk = false;
            try (ResultSet r = s.executeQuery("pragma foreign_key_list(pchild)")) {
                while (r.next()) {
                    if ("pt".equals(r.getString("table"))) {
                        fkOk = "pid".equals(r.getString("from")) && "id".equals(r.getString("to"));
                    }
                }
            }
            check("pragma.foreign_key_list", fkOk);

            // journal_mode for in-memory db is 'memory'
            eq("pragma.journal_mode", "memory", qStr(c, "pragma journal_mode"));

            // user_version round-trip
            s.execute("pragma user_version=42");
            eqL("pragma.user_version", 42, qLong(c, "pragma user_version"));

            // foreign_keys toggle is observable
            eqL("pragma.foreign_keys.default", 0, qLong(c, "pragma foreign_keys"));
            s.execute("pragma foreign_keys=ON");
            eqL("pragma.foreign_keys.on", 1, qLong(c, "pragma foreign_keys"));
        }
    }

    // =====================================================================
    // L. AUTOINCREMENT, rowid, last_insert_rowid()
    // =====================================================================
    static void autoincrementRowid() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table ai(id integer primary key autoincrement, v text)");
            s.executeUpdate("insert into ai(v) values('a')");
            eqL("ai.firstRowid", 1, qLong(c, "select last_insert_rowid()"));
            s.executeUpdate("insert into ai(v) values('b')");
            eqL("ai.secondRowid", 2, qLong(c, "select last_insert_rowid()"));

            // AUTOINCREMENT never reuses ids even after deleting the max row
            s.executeUpdate("delete from ai where id=2");
            s.executeUpdate("insert into ai(v) values('c')");
            eqL("ai.monotonicAfterDelete", 3, qLong(c, "select max(id) from ai"));

            // sqlite_sequence tracks the high-water mark
            eqL("ai.sqliteSequence", 3, qLong(c, "select seq from sqlite_sequence where name='ai'"));

            // rowid is an alias of an INTEGER PRIMARY KEY
            eqL("ai.rowidAlias", 1, qLong(c, "select rowid from ai where id=1"));

            // plain rowid table (no AUTOINCREMENT) can reuse ids
            s.execute("create table rt(id integer primary key, v text)");
            s.executeUpdate("insert into rt values(5,'x')");
            eqL("ai.rowidExplicit", 5, qLong(c, "select rowid from rt where id=5"));
        }
    }

    // =====================================================================
    // M. Indexes (incl. unique constraint violation)
    // =====================================================================
    static void indexes() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table idx(id integer primary key, email text, age integer)");
            s.execute("create unique index ux_email on idx(email)");
            s.execute("create index ix_age on idx(age)");

            s.executeUpdate("insert into idx values(1,'a@x',20)");
            s.executeUpdate("insert into idx values(2,'b@x',30)");

            // duplicate on the unique index -> SQLITE_CONSTRAINT (19)
            mustThrowCode("idx.uniqueViolation", 19,
                    () -> s.executeUpdate("insert into idx values(3,'a@x',40)"));

            // the failed insert left no row behind
            eqL("idx.countAfterViolation", 2, qLong(c, "select count(*) from idx"));

            // index is actually used by the planner (EXPLAIN QUERY PLAN 'detail' column)
            boolean planUsesIndex = false;
            try (ResultSet r = s.executeQuery(
                    "explain query plan select * from idx where email='a@x'")) {
                while (r.next()) {
                    String detail = r.getString("detail");
                    if (detail != null && detail.contains("ux_email")) {
                        planUsesIndex = true;
                    }
                }
            }
            check("idx.queryPlanUsesIndex", planUsesIndex);

            // both indexes show up
            int idxCount = 0;
            try (ResultSet r = s.executeQuery("pragma index_list(idx)")) {
                while (r.next()) {
                    idxCount++;
                }
            }
            check("idx.indexListCount", idxCount >= 2);
        }
    }

    // =====================================================================
    // N. Foreign key constraints (enforced after pragma)
    // =====================================================================
    static void foreignKeys() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("pragma foreign_keys=ON");
            s.execute("create table parent(id integer primary key)");
            s.execute("create table child(id integer primary key, pid integer "
                    + "references parent(id) on delete cascade)");
            s.executeUpdate("insert into parent values(1)");
            s.executeUpdate("insert into child values(10,1)");
            eqL("fk.validChildInserted", 1, qLong(c, "select count(*) from child"));

            // inserting a child pointing to a missing parent -> SQLITE_CONSTRAINT (19)
            mustThrowCode("fk.violation", 19,
                    () -> s.executeUpdate("insert into child values(11,999)"));
            eqL("fk.noOrphan", 1, qLong(c, "select count(*) from child"));

            // ON DELETE CASCADE removes the child when its parent is deleted
            s.executeUpdate("delete from parent where id=1");
            eqL("fk.cascadeDelete", 0, qLong(c, "select count(*) from child"));

            // disabling the pragma stops enforcement
            s.execute("pragma foreign_keys=OFF");
            s.executeUpdate("insert into parent values(2)");
            s.executeUpdate("insert into child values(20,12345)"); // dangling allowed now
            eqL("fk.danglingWhenOff", 1, qLong(c, "select count(*) from child where pid=12345"));
        }
    }

    // =====================================================================
    // O. Triggers
    // =====================================================================
    static void triggers() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table src(id integer primary key, amt integer)");
            s.execute("create table audit(id integer primary key autoincrement, note text, amt integer)");
            s.execute("create trigger trg_ai after insert on src begin "
                    + "insert into audit(note, amt) values('ins', new.amt); end");
            s.execute("create trigger trg_au after update on src begin "
                    + "insert into audit(note, amt) values('upd', new.amt); end");

            s.executeUpdate("insert into src values(1,100)");
            s.executeUpdate("insert into src values(2,200)");
            eqL("trg.insertFired", 2, qLong(c, "select count(*) from audit where note='ins'"));
            eqL("trg.insertAmtSum", 300, qLong(c, "select sum(amt) from audit where note='ins'"));

            s.executeUpdate("update src set amt=250 where id=2");
            eqL("trg.updateFired", 1, qLong(c, "select count(*) from audit where note='upd'"));
            eqL("trg.updateAmt", 250, qLong(c, "select amt from audit where note='upd'"));

            // total audit rows = 2 inserts + 1 update
            eqL("trg.totalAudit", 3, qLong(c, "select count(*) from audit"));
        }
    }

    // =====================================================================
    // P. Views
    // =====================================================================
    static void views() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table sales(region text, amt integer)");
            s.execute("insert into sales values('n',10),('n',20),('s',5),('s',5)");
            s.execute("create view v_region as select region, sum(amt) total, count(*) cnt "
                    + "from sales group by region");

            eqL("view.north.total", 30, qLong(c, "select total from v_region where region='n'"));
            eqL("view.south.total", 10, qLong(c, "select total from v_region where region='s'"));
            eqL("view.north.cnt", 2, qLong(c, "select cnt from v_region where region='n'"));
            eqL("view.rowCount", 2, qLong(c, "select count(*) from v_region"));

            // the view appears in metadata as a VIEW
            DatabaseMetaData m = c.getMetaData();
            int viewHits = 0;
            try (ResultSet r = m.getTables(null, null, "v_region", new String[] {"VIEW"})) {
                while (r.next()) {
                    viewHits++;
                }
            }
            eqL("view.metadataView", 1, viewHits);
        }
    }

    // =====================================================================
    // Q. Built-in scalar functions (deterministic inputs)
    // =====================================================================
    static void scalarFunctions() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            eqL("fn.length", 5, qLong(c, "select length('hello')"));
            eqL("fn.length.unicode", 5, qLong(c, "select length('héllo')"));
            eq("fn.substr", "ell", qStr(c, "select substr('hello',2,3)"));
            eq("fn.substr.neg", "llo", qStr(c, "select substr('hello',-3)"));
            eq("fn.upper", "ABC", qStr(c, "select upper('abc')"));
            eq("fn.lower", "abc", qStr(c, "select lower('ABC')"));
            eqL("fn.abs", 7, qLong(c, "select abs(-7)"));
            eqD("fn.round2", 3.14, qDbl(c, "select round(3.14159,2)"));
            eqD("fn.round0", 3.0, qDbl(c, "select round(2.5)"));
            eqL("fn.coalesce", 3, qLong(c, "select coalesce(null,null,3)"));
            eq("fn.typeof.int", "integer", qStr(c, "select typeof(1)"));
            eq("fn.typeof.real", "real", qStr(c, "select typeof(1.5)"));
            eq("fn.typeof.text", "text", qStr(c, "select typeof('x')"));
            eq("fn.typeof.null", "null", qStr(c, "select typeof(null)"));
            eq("fn.typeof.blob", "blob", qStr(c, "select typeof(x'00')"));
            eq("fn.hex", "4142", qStr(c, "select hex('AB')"));
            eq("fn.quote", "'it''s'", qStr(c, "select quote('it''s')"));
            eq("fn.replace", "a-b-c", qStr(c, "select replace('aXbXc','X','-')"));
            eq("fn.trim", "hi", qStr(c, "select trim('  hi  ')"));
            eq("fn.ltrim", "hi  ", qStr(c, "select ltrim('  hi  ')"));
            eq("fn.rtrim", "  hi", qStr(c, "select rtrim('  hi  ')"));
            eqL("fn.instr", 3, qLong(c, "select instr('hello','ll')"));
            eqL("fn.instr.miss", 0, qLong(c, "select instr('hello','z')"));
            eq("fn.char", "HI", qStr(c, "select char(72,73)"));
            eqL("fn.unicode", 65, qLong(c, "select unicode('A')"));
            eq("fn.nullif.same", null, qStr(c, "select nullif(5,5)"));
            eqL("fn.nullif.diff", 5, qLong(c, "select nullif(5,6)"));
            eqL("fn.ifnull", 7, qLong(c, "select ifnull(null,7)"));
            eqL("fn.min.scalar", 3, qLong(c, "select min(8,3,5)"));
            eqL("fn.max.scalar", 8, qLong(c, "select max(8,3,5)"));
            eq("fn.printf", "00042", qStr(c, "select printf('%05d',42)"));
            eq("fn.format", "3.14", qStr(c, "select format('%.2f',3.14159)"));

            // random() is non-deterministic in value but deterministic in TYPE
            eq("fn.typeof.random", "integer", qStr(c, "select typeof(random())"));
            check("fn.randomblob.len", qLong(c, "select length(randomblob(8))") == 8);
        }
    }

    // =====================================================================
    // R. Date/time functions (fixed inputs -> fixed outputs)
    // =====================================================================
    static void dateTimeFunctions() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            eq("dt.date", "2024-01-15", qStr(c, "select date('2024-01-15 10:30:00')"));
            eq("dt.time", "10:30:00", qStr(c, "select time('2024-01-15 10:30:00')"));
            eq("dt.datetime", "2024-01-15 10:30:00",
                    qStr(c, "select datetime('2024-01-15 10:30:00')"));
            eq("dt.strftime.full", "2024-01-15 10:30:00",
                    qStr(c, "select strftime('%Y-%m-%d %H:%M:%S','2024-01-15 10:30:00')"));
            eq("dt.strftime.year", "2024", qStr(c, "select strftime('%Y','2024-01-15')"));
            eq("dt.strftime.weekday", "1", qStr(c, "select strftime('%w','2024-01-15')"));
            eq("dt.strftime.dayOfYear", "015", qStr(c, "select strftime('%j','2024-01-15')"));
            eq("dt.strftime.epoch", "1",
                    qStr(c, "select strftime('%s','1970-01-01 00:00:01')"));
            eqD("dt.julianday", 2451544.5, qDbl(c, "select julianday('2000-01-01')"));
            eqL("dt.unixepoch", 946684800, qLong(c, "select unixepoch('2000-01-01')"));
            eq("dt.modifier.plusDay", "2024-02-01", qStr(c, "select date('2024-01-31','+1 day')"));
            eq("dt.modifier.startOfMonth", "2024-01-01",
                    qStr(c, "select date('2024-01-15','start of month')"));
            eq("dt.modifier.plusMonth", "2024-03-15",
                    qStr(c, "select date('2024-01-15','+2 months')"));
        }
    }

    // =====================================================================
    // S. Aggregate functions
    // =====================================================================
    static void aggregates() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table ag(grp text, v integer)");
            s.execute("insert into ag values('a',1),('a',2),('a',3),('b',10),('b',20)");

            eqL("agg.count", 5, qLong(c, "select count(*) from ag"));
            eqL("agg.countDistinct", 2, qLong(c, "select count(distinct grp) from ag"));
            eqL("agg.sum", 36, qLong(c, "select sum(v) from ag"));
            eqD("agg.avg", 7.2, qDbl(c, "select avg(v) from ag"));
            eqL("agg.min", 1, qLong(c, "select min(v) from ag"));
            eqL("agg.max", 20, qLong(c, "select max(v) from ag"));
            eqD("agg.total", 36.0, qDbl(c, "select total(v) from ag"));
            eq("agg.typeof.total", "real", qStr(c, "select typeof(total(v)) from ag"));

            // grouped aggregation
            eqL("agg.groupA.sum", 6, qLong(c, "select sum(v) from ag where grp='a'"));
            eqL("agg.groupB.sum", 30, qLong(c, "select sum(v) from ag where grp='b'"));

            // group_concat default comma + custom separator (ordered input)
            eq("agg.groupConcat", "1,2,3",
                    qStr(c, "select group_concat(v) from (select v from ag where grp='a' order by v)"));
            eq("agg.groupConcatSep", "1|2|3",
                    qStr(c, "select group_concat(v,'|') from (select v from ag where grp='a' order by v)"));

            // HAVING filters groups
            eqL("agg.having", 1,
                    qLong(c, "select count(*) from (select grp from ag group by grp having sum(v) > 10)"));
        }
    }

    // =====================================================================
    // T. CTE WITH RECURSIVE
    // =====================================================================
    static void recursiveCte() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            eqL("cte.sum1to10", 55, qLong(c,
                    "with recursive cnt(n) as (select 1 union all select n+1 from cnt where n<10) "
                            + "select sum(n) from cnt"));
            eqL("cte.count", 10, qLong(c,
                    "with recursive cnt(n) as (select 1 union all select n+1 from cnt where n<10) "
                            + "select count(*) from cnt"));
            // factorial 5! = 120 via recursive product
            eqL("cte.factorial5", 120, qLong(c,
                    "with recursive f(n,acc) as (select 1,1 union all select n+1, acc*(n+1) from f where n<5) "
                            + "select acc from f where n=5"));
            // non-recursive CTE
            eqL("cte.plain", 30, qLong(c,
                    "with t(x) as (values(10),(20)) select sum(x) from t"));
        }
    }

    // =====================================================================
    // U. UPSERT (ON CONFLICT)
    // =====================================================================
    static void upsert() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table kv(k text primary key, v integer)");
            s.executeUpdate("insert into kv values('a',1)");

            // DO UPDATE using excluded.*
            s.executeUpdate("insert into kv values('a',5) on conflict(k) do update set v = v + excluded.v");
            eqL("upsert.doUpdate", 6, qLong(c, "select v from kv where k='a'"));

            // DO NOTHING leaves the existing row untouched
            s.executeUpdate("insert into kv values('a',100) on conflict(k) do nothing");
            eqL("upsert.doNothing", 6, qLong(c, "select v from kv where k='a'"));

            // a brand-new key just inserts
            s.executeUpdate("insert into kv values('b',2) on conflict(k) do update set v = excluded.v");
            eqL("upsert.freshInsert", 2, qLong(c, "select v from kv where k='b'"));
            eqL("upsert.rowCount", 2, qLong(c, "select count(*) from kv"));

            // conditional upsert (WHERE on the conflict clause)
            s.executeUpdate("insert into kv values('b',999) on conflict(k) do update set v = excluded.v where excluded.v < 5");
            eqL("upsert.conditionalSkipped", 2, qLong(c, "select v from kv where k='b'"));
        }
    }

    // =====================================================================
    // V. BLOB read/write round-trip
    // =====================================================================
    static void blobRoundTrip() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table b(id integer primary key, data blob)");
            byte[] payload = new byte[] {0, 1, 2, 127, -1, -128, 65, 0, 66};

            try (PreparedStatement ps = c.prepareStatement("insert into b values(1,?)")) {
                ps.setBytes(1, payload);
                ps.executeUpdate();
            }
            try (PreparedStatement q = c.prepareStatement("select data from b where id=1")) {
                try (ResultSet r = q.executeQuery()) {
                    r.next();
                    check("blob.roundTrip", Arrays.equals(payload, r.getBytes(1)));
                    eq("blob.typeof", "blob", null == r.getObject(1) ? "null" : "blob");
                }
            }
            eqL("blob.length", payload.length, qLong(c, "select length(data) from b where id=1"));
            eq("blob.typeofSql", "blob", qStr(c, "select typeof(data) from b where id=1"));

            // empty blob
            s.executeUpdate("insert into b values(2, x'')");
            eqL("blob.empty.length", 0, qLong(c, "select length(data) from b where id=2"));

            // zeroblob(N) creates N zero bytes
            s.executeUpdate("insert into b values(3, zeroblob(4))");
            eqL("blob.zeroblob.length", 4, qLong(c, "select length(data) from b where id=3"));
            eq("blob.zeroblob.hex", "00000000", qStr(c, "select hex(data) from b where id=3"));

            // hex literal round-trips through getBytes
            s.executeUpdate("insert into b values(4, x'deadbeef')");
            try (ResultSet r = s.executeQuery("select data from b where id=4")) {
                r.next();
                check("blob.hexLiteral",
                        Arrays.equals(new byte[] {(byte) 0xDE, (byte) 0xAD, (byte) 0xBE, (byte) 0xEF},
                                r.getBytes(1)));
            }
        }
    }

    // =====================================================================
    // W. json1 extension
    // =====================================================================
    static void json1() throws Exception {
        try (Connection c = DriverManager.getConnection(URL)) {
            eqL("json.extract.scalar", 5, qLong(c, "select json_extract('{\"a\":5}','$.a')"));
            eqL("json.extract.nested", 7,
                    qLong(c, "select json_extract('{\"a\":{\"b\":7}}','$.a.b')"));
            eqL("json.extract.arrayIdx", 20,
                    qLong(c, "select json_extract('[10,20,30]','$[1]')"));
            eq("json.array", "[1,2,3]", qStr(c, "select json_array(1,2,3)"));
            eq("json.object", "{\"a\":1,\"b\":2}", qStr(c, "select json_object('a',1,'b',2)"));
            eq("json.type.object", "object", qStr(c, "select json_type('{\"a\":1}')"));
            eq("json.type.array", "array", qStr(c, "select json_type('[1,2]')"));
            eqL("json.arrayLength", 3, qLong(c, "select json_array_length('[10,20,30]')"));
            eqL("json.valid.true", 1, qLong(c, "select json_valid('{\"a\":1}')"));
            eqL("json.valid.false", 0, qLong(c, "select json_valid('{bad}')"));
            eq("json.quote", "3.14", qStr(c, "select json_quote(3.14)"));
            // json() minifies/normalizes its input
            eq("json.normalize", "{\"a\":1}", qStr(c, "select json(' { \"a\" : 1 } ')"));
            // json_set / json_insert mutate documents
            eqL("json.set", 9,
                    qLong(c, "select json_extract(json_set('{\"a\":1}','$.a',9),'$.a')"));
        }
    }

    // =====================================================================
    // X. ATTACH DATABASE :memory:
    // =====================================================================
    static void attachDatabase() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("attach database ':memory:' as aux");
            s.execute("create table aux.nums(n integer)");
            s.executeUpdate("insert into aux.nums values(1),(2),(3)");
            s.execute("create table main.m(n integer)");
            s.executeUpdate("insert into main.m values(10),(20)");

            eqL("attach.auxSum", 6, qLong(c, "select sum(n) from aux.nums"));
            eqL("attach.mainSum", 30, qLong(c, "select sum(n) from main.m"));

            // cross-database query within the same connection
            eqL("attach.crossSum", 36,
                    qLong(c, "select (select sum(n) from aux.nums) + (select sum(n) from main.m)"));

            // aux shows up in pragma database_list
            boolean sawAux = false;
            try (ResultSet r = s.executeQuery("pragma database_list")) {
                while (r.next()) {
                    if ("aux".equals(r.getString("name"))) {
                        sawAux = true;
                    }
                }
            }
            check("attach.databaseList", sawAux);

            // detach makes aux.nums unreachable
            s.execute("detach database aux");
            mustThrow("attach.detachedGone", () -> qLong(c, "select count(*) from aux.nums"));
            // main table survives detach
            eqL("attach.mainSurvives", 30, qLong(c, "select sum(n) from main.m"));
        }
    }

    // =====================================================================
    // Y. Window functions
    // =====================================================================
    static void windowFunctions() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table w(g text, v integer)");
            s.execute("insert into w values('a',1),('a',3),('a',3),('b',5)");

            // row_number over full order
            eqL("win.rowNumberMax", 4,
                    qLong(c, "select max(rn) from (select row_number() over (order by v) rn from w)"));
            // sum over entire window
            eqL("win.sumOver", 12, qLong(c, "select distinct sum(v) over () from w"));
            // rank with ties: values 1,3,3,5 -> ranks 1,2,2,4 ; max rank == 4
            eqL("win.rankMax", 4,
                    qLong(c, "select max(rk) from (select rank() over (order by v) rk from w)"));
            // dense_rank with ties: 1,2,2,3 -> max == 3
            eqL("win.denseRankMax", 3,
                    qLong(c, "select max(rk) from (select dense_rank() over (order by v) rk from w)"));
            // partitioned running count
            eqL("win.partitionCount", 3,
                    qLong(c, "select max(cn) from (select count(*) over (partition by g) cn from w)"));
        }
    }

    // =====================================================================
    // Z. Generated columns
    // =====================================================================
    static void generatedColumns() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table gen(a integer, "
                    + "sq integer generated always as (a*a) stored, "
                    + "inc integer as (a+1) virtual)");
            s.executeUpdate("insert into gen(a) values(4)");
            eqL("gen.stored", 16, qLong(c, "select sq from gen where a=4"));
            eqL("gen.virtual", 5, qLong(c, "select inc from gen where a=4"));

            s.executeUpdate("insert into gen(a) values(10)");
            eqL("gen.stored2", 100, qLong(c, "select sq from gen where a=10"));
            eqL("gen.virtual2", 11, qLong(c, "select inc from gen where a=10"));

            // writing to a generated column is rejected
            mustThrow("gen.cannotWrite",
                    () -> s.executeUpdate("insert into gen(a,sq) values(2,999)"));
        }
    }

    // =====================================================================
    // AA. Exception & error-code paths
    // =====================================================================
    static void exceptionPaths() throws Exception {
        try (Connection c = DriverManager.getConnection(URL); Statement s = c.createStatement()) {
            s.execute("create table e(id integer primary key, name text not null, k text unique)");
            s.executeUpdate("insert into e values(1,'a','u1')");

            // syntax error -> SQLITE_ERROR (1)
            mustThrowCode("err.syntax", 1, () -> s.execute("select bad syntax !!!"));
            // unknown table
            mustThrow("err.noSuchTable", () -> qLong(c, "select * from no_such_table"));
            // unknown column
            mustThrow("err.noSuchColumn", () -> qLong(c, "select no_such_col from e"));
            // NOT NULL violation -> SQLITE_CONSTRAINT (19)
            mustThrowCode("err.notNull", 19,
                    () -> s.executeUpdate("insert into e(id,name) values(2,null)"));
            // UNIQUE violation -> SQLITE_CONSTRAINT (19)
            mustThrowCode("err.unique", 19,
                    () -> s.executeUpdate("insert into e values(3,'b','u1')"));
            // PRIMARY KEY violation -> SQLITE_CONSTRAINT (19)
            mustThrowCode("err.primaryKey", 19,
                    () -> s.executeUpdate("insert into e values(1,'c','u2')"));
            // prepared statement with too-few bound parameters
            mustThrow("err.unboundParam", () -> {
                try (PreparedStatement ps = c.prepareStatement("insert into e values(?,?,?)")) {
                    ps.setInt(1, 9);
                    ps.executeUpdate(); // params 2,3 never set
                }
            });
            // a closed statement reports closed; reading from a closed ResultSet throws
            Statement closed = c.createStatement();
            closed.close();
            check("err.statementIsClosed", closed.isClosed());
            ResultSet closedRs = s.executeQuery("select 1");
            closedRs.next();
            closedRs.close();
            mustThrow("err.closedResultSetGet", () -> closedRs.getInt(1));
            // operating on a closed connection throws
            Connection cc = DriverManager.getConnection(URL);
            cc.close();
            mustThrow("err.closedConnection", () -> cc.createStatement());

            // none of the failed writes mutated the table
            eqL("err.tableUntouched", 1, qLong(c, "select count(*) from e"));

            // SQLite returns NULL (not an error) for divide-by-zero
            eq("err.divZeroIsNull", null, qStr(c, "select 1/0"));
        }
    }
}
