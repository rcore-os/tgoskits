import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import java.math.BigDecimal;
import java.sql.Connection;
import java.sql.Date;
import java.sql.DatabaseMetaData;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;
import java.util.ArrayList;
import java.util.List;

import org.junit.Test;

/**
 * Java-8 backward-compatibility carpet for the "Sql" library group:
 *   - H2     2.1.214 (jdbc:h2:mem:)
 *   - HSQLDB 2.5.2   (jdbc:hsqldb:mem:)
 *
 * Every test opens a fresh, private in-memory database, builds fixed schema +
 * fixed rows, and asserts EXACT results. No NOW()/CURRENT_TIMESTAMP/RANDOM, so
 * the suite is fully deterministic and identical across JDK 17/21/23/25.
 * Source uses only Java 8 APIs (compiled with --release 8, bytecode 52).
 */
public class SqlBackCompatTest {

    // ----------------------------------------------------------------------
    // URL helpers. Each test uses a UNIQUE named in-memory DB so tests are
    // independent and order-insensitive (deterministic regardless of runner).
    // DB_CLOSE_DELAY=-1 keeps H2 mem DB alive for the connection's lifetime.
    // ----------------------------------------------------------------------
    private static int seq = 0;

    private static synchronized String h2Url() {
        return "jdbc:h2:mem:bc_" + (seq++) + ";DB_CLOSE_DELAY=-1";
    }

    private static synchronized String hsqlUrl() {
        return "jdbc:hsqldb:mem:bc_" + (seq++);
    }

    private Connection h2() throws SQLException {
        return DriverManager.getConnection(h2Url(), "sa", "");
    }

    private Connection hsql() throws SQLException {
        return DriverManager.getConnection(hsqlUrl(), "SA", "");
    }

    /** CREATE TABLE emp + 5 fixed rows used by many tests. Portable DDL. */
    private void seedEmp(Connection c) throws SQLException {
        try (Statement s = c.createStatement()) {
            s.execute("CREATE TABLE emp ("
                    + "id INTEGER PRIMARY KEY, "
                    + "name VARCHAR(40), "
                    + "dept VARCHAR(20), "
                    + "salary DECIMAL(10,2))");
            s.execute("INSERT INTO emp VALUES (1,'Alice','ENG',9000.00)");
            s.execute("INSERT INTO emp VALUES (2,'Bob','ENG',7500.50)");
            s.execute("INSERT INTO emp VALUES (3,'Carol','SALES',8000.00)");
            s.execute("INSERT INTO emp VALUES (4,'Dave','SALES',6000.25)");
            s.execute("INSERT INTO emp VALUES (5,'Eve','HR',5000.00)");
        }
    }

    private void seedDept(Connection c) throws SQLException {
        try (Statement s = c.createStatement()) {
            s.execute("CREATE TABLE dept ("
                    + "code VARCHAR(20) PRIMARY KEY, "
                    + "label VARCHAR(40))");
            s.execute("INSERT INTO dept VALUES ('ENG','Engineering')");
            s.execute("INSERT INTO dept VALUES ('SALES','Sales')");
            s.execute("INSERT INTO dept VALUES ('HR','Human Resources')");
        }
    }

    // ======================================================================
    // H2 driver registration + connection metadata
    // ======================================================================
    @Test
    public void h2_driverLoadsAndConnects() throws Exception {
        Class.forName("org.h2.Driver");
        try (Connection c = h2()) {
            assertNotNull(c);
            assertFalse(c.isClosed());
            assertTrue(c.isValid(2));
        }
    }

    @Test
    public void h2_databaseMetaData() throws Exception {
        try (Connection c = h2()) {
            DatabaseMetaData md = c.getMetaData();
            assertEquals("H2", md.getDatabaseProductName());
            assertTrue(md.supportsTransactions());
            assertTrue(md.supportsResultSetType(ResultSet.TYPE_SCROLL_INSENSITIVE));
        }
    }

    // ======================================================================
    // H2 basic CRUD via Statement
    // ======================================================================
    @Test
    public void h2_selectCount() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT COUNT(*) FROM emp")) {
                assertTrue(rs.next());
                assertEquals(5, rs.getInt(1));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void h2_whereFilter() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT name FROM emp WHERE dept='ENG' ORDER BY id")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString("name"));
                assertEquals(java.util.Arrays.asList("Alice", "Bob"), got);
            }
        }
    }

    @Test
    public void h2_orderByDesc() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT id FROM emp ORDER BY salary DESC")) {
                int[] order = new int[5];
                int i = 0;
                while (rs.next()) order[i++] = rs.getInt(1);
                assertArrayEquals(new int[] {1, 3, 2, 4, 5}, order);
            }
        }
    }

    @Test
    public void h2_aggregateGroupBy() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT dept, COUNT(*), SUM(salary) FROM emp "
                   + "GROUP BY dept ORDER BY dept")) {
                assertTrue(rs.next());
                assertEquals("ENG", rs.getString(1));
                assertEquals(2, rs.getInt(2));
                assertEquals(0, new BigDecimal("16500.50").compareTo(rs.getBigDecimal(3)));
                assertTrue(rs.next());
                assertEquals("HR", rs.getString(1));
                assertEquals(1, rs.getInt(2));
                assertTrue(rs.next());
                assertEquals("SALES", rs.getString(1));
                assertEquals(2, rs.getInt(2));
                assertEquals(0, new BigDecimal("14000.25").compareTo(rs.getBigDecimal(3)));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void h2_havingClause() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT dept FROM emp GROUP BY dept "
                   + "HAVING COUNT(*) > 1 ORDER BY dept")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString(1));
                assertEquals(java.util.Arrays.asList("ENG", "SALES"), got);
            }
        }
    }

    @Test
    public void h2_minMaxAvg() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT MIN(salary), MAX(salary) FROM emp")) {
                assertTrue(rs.next());
                assertEquals(0, new BigDecimal("5000.00").compareTo(rs.getBigDecimal(1)));
                assertEquals(0, new BigDecimal("9000.00").compareTo(rs.getBigDecimal(2)));
            }
        }
    }

    @Test
    public void h2_innerJoin() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            seedDept(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT e.name, d.label FROM emp e "
                   + "JOIN dept d ON e.dept = d.code "
                   + "WHERE e.dept='HR'")) {
                assertTrue(rs.next());
                assertEquals("Eve", rs.getString(1));
                assertEquals("Human Resources", rs.getString(2));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void h2_leftJoinNull() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE a (id INT)");
                s.execute("CREATE TABLE b (id INT, v VARCHAR(10))");
                s.execute("INSERT INTO a VALUES (1),(2)");
                s.execute("INSERT INTO b VALUES (1,'one')");
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT a.id, b.v FROM a LEFT JOIN b ON a.id=b.id ORDER BY a.id")) {
                assertTrue(rs.next());
                assertEquals(1, rs.getInt(1));
                assertEquals("one", rs.getString(2));
                assertTrue(rs.next());
                assertEquals(2, rs.getInt(1));
                assertNull(rs.getString(2));
                assertTrue(rs.wasNull());
                assertFalse(rs.next());
            }
        }
    }

    // ======================================================================
    // H2 PreparedStatement
    // ======================================================================
    @Test
    public void h2_preparedQuery() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (PreparedStatement ps = c.prepareStatement(
                     "SELECT name FROM emp WHERE salary > ? ORDER BY salary")) {
                ps.setBigDecimal(1, new BigDecimal("7000.00"));
                try (ResultSet rs = ps.executeQuery()) {
                    List<String> got = new ArrayList<>();
                    while (rs.next()) got.add(rs.getString(1));
                    assertEquals(java.util.Arrays.asList("Bob", "Carol", "Alice"), got);
                }
            }
        }
    }

    @Test
    public void h2_preparedInsertUpdateDelete() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(20))");
            }
            try (PreparedStatement ps = c.prepareStatement(
                     "INSERT INTO t VALUES (?,?)")) {
                ps.setInt(1, 10);
                ps.setString(2, "x");
                assertEquals(1, ps.executeUpdate());
            }
            try (PreparedStatement ps = c.prepareStatement(
                     "UPDATE t SET v=? WHERE id=?")) {
                ps.setString(1, "y");
                ps.setInt(2, 10);
                assertEquals(1, ps.executeUpdate());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT v FROM t WHERE id=10")) {
                assertTrue(rs.next());
                assertEquals("y", rs.getString(1));
            }
            try (PreparedStatement ps = c.prepareStatement("DELETE FROM t WHERE id=?")) {
                ps.setInt(1, 10);
                assertEquals(1, ps.executeUpdate());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT COUNT(*) FROM t")) {
                assertTrue(rs.next());
                assertEquals(0, rs.getInt(1));
            }
        }
    }

    @Test
    public void h2_preparedBatch() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE nums (n INT)");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO nums VALUES (?)")) {
                for (int i = 1; i <= 5; i++) {
                    ps.setInt(1, i * 10);
                    ps.addBatch();
                }
                int[] counts = ps.executeBatch();
                assertEquals(5, counts.length);
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT SUM(n) FROM nums")) {
                assertTrue(rs.next());
                assertEquals(150, rs.getInt(1));
            }
        }
    }

    @Test
    public void h2_typedParameters() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE types_t (i INT, l BIGINT, d DOUBLE, "
                        + "b BOOLEAN, str VARCHAR(20), dec DECIMAL(8,3))");
            }
            try (PreparedStatement ps = c.prepareStatement(
                     "INSERT INTO types_t VALUES (?,?,?,?,?,?)")) {
                ps.setInt(1, 42);
                ps.setLong(2, 9000000000L);
                ps.setDouble(3, 3.5);
                ps.setBoolean(4, true);
                ps.setString(5, "hello");
                ps.setBigDecimal(6, new BigDecimal("1.250"));
                assertEquals(1, ps.executeUpdate());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT * FROM types_t")) {
                assertTrue(rs.next());
                assertEquals(42, rs.getInt("i"));
                assertEquals(9000000000L, rs.getLong("l"));
                assertEquals(3.5, rs.getDouble("d"), 0.0);
                assertTrue(rs.getBoolean("b"));
                assertEquals("hello", rs.getString("str"));
                assertEquals(0, new BigDecimal("1.250").compareTo(rs.getBigDecimal("dec")));
            }
        }
    }

    @Test
    public void h2_setNull() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE nt (id INT, v VARCHAR(10))");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO nt VALUES (?,?)")) {
                ps.setInt(1, 1);
                ps.setNull(2, Types.VARCHAR);
                ps.executeUpdate();
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT v FROM nt WHERE id=1")) {
                assertTrue(rs.next());
                assertNull(rs.getString(1));
                assertTrue(rs.wasNull());
            }
        }
    }

    // ======================================================================
    // H2 transactions: commit / rollback / savepoint
    // ======================================================================
    @Test
    public void h2_transactionCommit() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE acc (id INT, bal INT)");
                s.execute("INSERT INTO acc VALUES (1,100)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("UPDATE acc SET bal=bal+50 WHERE id=1");
            }
            c.commit();
            c.setAutoCommit(true);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT bal FROM acc WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals(150, rs.getInt(1));
            }
        }
    }

    @Test
    public void h2_transactionRollback() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE acc (id INT, bal INT)");
                s.execute("INSERT INTO acc VALUES (1,100)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("UPDATE acc SET bal=bal-30 WHERE id=1");
            }
            c.rollback();
            c.setAutoCommit(true);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT bal FROM acc WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals(100, rs.getInt(1));
            }
        }
    }

    @Test
    public void h2_savepointRollback() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE sp (id INT)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("INSERT INTO sp VALUES (1)");
                java.sql.Savepoint sv = c.setSavepoint("mid");
                s.executeUpdate("INSERT INTO sp VALUES (2)");
                c.rollback(sv);
            }
            c.commit();
            c.setAutoCommit(true);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT COUNT(*) FROM sp")) {
                assertTrue(rs.next());
                assertEquals(1, rs.getInt(1));
            }
        }
    }

    // ======================================================================
    // H2 ResultSetMetaData, generated keys, scrollable rs
    // ======================================================================
    @Test
    public void h2_resultSetMetaData() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT id, name, salary FROM emp")) {
                ResultSetMetaData m = rs.getMetaData();
                assertEquals(3, m.getColumnCount());
                assertEquals("ID", m.getColumnName(1).toUpperCase());
                assertEquals("NAME", m.getColumnName(2).toUpperCase());
                assertEquals("SALARY", m.getColumnName(3).toUpperCase());
            }
        }
    }

    @Test
    public void h2_generatedKeys() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE gk (id INT AUTO_INCREMENT PRIMARY KEY, v VARCHAR(10))");
            }
            try (PreparedStatement ps = c.prepareStatement(
                     "INSERT INTO gk (v) VALUES (?)", Statement.RETURN_GENERATED_KEYS)) {
                ps.setString(1, "a");
                ps.executeUpdate();
                try (ResultSet keys = ps.getGeneratedKeys()) {
                    assertTrue(keys.next());
                    assertEquals(1, keys.getInt(1));
                }
            }
        }
    }

    @Test
    public void h2_scrollableResultSet() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement(
                     ResultSet.TYPE_SCROLL_INSENSITIVE, ResultSet.CONCUR_READ_ONLY);
                 ResultSet rs = s.executeQuery("SELECT id FROM emp ORDER BY id")) {
                assertTrue(rs.last());
                assertEquals(5, rs.getRow());
                assertEquals(5, rs.getInt(1));
                assertTrue(rs.first());
                assertEquals(1, rs.getInt(1));
                assertTrue(rs.absolute(3));
                assertEquals(3, rs.getInt(1));
            }
        }
    }

    // ======================================================================
    // H2 SQL functions, expressions, subqueries
    // ======================================================================
    @Test
    public void h2_stringFunctions() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT UPPER('abc'), LOWER('XYZ'), LENGTH('hello'), "
                   + "SUBSTRING('abcdef',2,3), CONCAT('foo','bar')")) {
                assertTrue(rs.next());
                assertEquals("ABC", rs.getString(1));
                assertEquals("xyz", rs.getString(2));
                assertEquals(5, rs.getInt(3));
                assertEquals("bcd", rs.getString(4));
                assertEquals("foobar", rs.getString(5));
            }
        }
    }

    @Test
    public void h2_arithmeticAndCase() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT name, CASE WHEN salary >= 8000 THEN 'HIGH' ELSE 'LOW' END "
                   + "FROM emp WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals("Alice", rs.getString(1));
                assertEquals("HIGH", rs.getString(2));
            }
        }
    }

    @Test
    public void h2_subqueryIn() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT COUNT(*) FROM emp WHERE dept IN "
                   + "(SELECT dept FROM emp WHERE salary > 7000 GROUP BY dept)")) {
                assertTrue(rs.next());
                // qualifying depts ENG (Alice9000,Bob7500) + SALES (Carol8000);
                // emp in those depts: Alice,Bob,Carol,Dave = 4
                assertEquals(4, rs.getInt(1));
            }
        }
    }

    @Test
    public void h2_distinctAndLike() throws Exception {
        try (Connection c = h2()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT DISTINCT dept FROM emp ORDER BY dept")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString(1));
                assertEquals(java.util.Arrays.asList("ENG", "HR", "SALES"), got);
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT name FROM emp WHERE name LIKE 'A%'")) {
                assertTrue(rs.next());
                assertEquals("Alice", rs.getString(1));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void h2_dateLiteral() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE dt (id INT, d DATE)");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO dt VALUES (?,?)")) {
                ps.setInt(1, 1);
                ps.setDate(2, Date.valueOf("2020-01-15"));
                ps.executeUpdate();
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT d FROM dt WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals(Date.valueOf("2020-01-15"), rs.getDate(1));
            }
        }
    }

    @Test
    public void h2_uniqueConstraintViolation() throws Exception {
        try (Connection c = h2()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE u (id INT PRIMARY KEY)");
                s.execute("INSERT INTO u VALUES (1)");
            }
            try (Statement s = c.createStatement()) {
                s.execute("INSERT INTO u VALUES (1)");
                fail("expected SQLException for duplicate PK");
            } catch (SQLException expected) {
                assertNotNull(expected.getMessage());
            }
        }
    }

    // ======================================================================
    // HSQLDB driver registration + metadata
    // ======================================================================
    @Test
    public void hsql_driverLoadsAndConnects() throws Exception {
        Class.forName("org.hsqldb.jdbc.JDBCDriver");
        try (Connection c = hsql()) {
            assertNotNull(c);
            assertFalse(c.isClosed());
        }
    }

    @Test
    public void hsql_databaseMetaData() throws Exception {
        try (Connection c = hsql()) {
            DatabaseMetaData md = c.getMetaData();
            assertEquals("HSQL Database Engine", md.getDatabaseProductName());
            assertTrue(md.supportsTransactions());
        }
    }

    // ======================================================================
    // HSQLDB basic CRUD via Statement
    // ======================================================================
    @Test
    public void hsql_selectCount() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT COUNT(*) FROM emp")) {
                assertTrue(rs.next());
                assertEquals(5, rs.getInt(1));
            }
        }
    }

    @Test
    public void hsql_whereFilter() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT name FROM emp WHERE dept='SALES' ORDER BY id")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString(1));
                assertEquals(java.util.Arrays.asList("Carol", "Dave"), got);
            }
        }
    }

    @Test
    public void hsql_orderByDesc() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT id FROM emp ORDER BY salary DESC")) {
                int[] order = new int[5];
                int i = 0;
                while (rs.next()) order[i++] = rs.getInt(1);
                assertArrayEquals(new int[] {1, 3, 2, 4, 5}, order);
            }
        }
    }

    @Test
    public void hsql_aggregateGroupBy() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT dept, COUNT(*), SUM(salary) FROM emp "
                   + "GROUP BY dept ORDER BY dept")) {
                assertTrue(rs.next());
                assertEquals("ENG", rs.getString(1));
                assertEquals(2, rs.getInt(2));
                assertEquals(0, new BigDecimal("16500.50").compareTo(rs.getBigDecimal(3)));
                assertTrue(rs.next());
                assertEquals("HR", rs.getString(1));
                assertTrue(rs.next());
                assertEquals("SALES", rs.getString(1));
                assertEquals(0, new BigDecimal("14000.25").compareTo(rs.getBigDecimal(3)));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void hsql_havingClause() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT dept FROM emp GROUP BY dept HAVING COUNT(*) > 1 ORDER BY dept")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString(1));
                assertEquals(java.util.Arrays.asList("ENG", "SALES"), got);
            }
        }
    }

    @Test
    public void hsql_innerJoin() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            seedDept(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT e.name, d.label FROM emp e "
                   + "JOIN dept d ON e.dept = d.code WHERE e.dept='ENG' ORDER BY e.id")) {
                assertTrue(rs.next());
                assertEquals("Alice", rs.getString(1));
                assertEquals("Engineering", rs.getString(2));
                assertTrue(rs.next());
                assertEquals("Bob", rs.getString(1));
                assertEquals("Engineering", rs.getString(2));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void hsql_leftJoinNull() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE a (id INT)");
                s.execute("CREATE TABLE b (id INT, v VARCHAR(10))");
                s.execute("INSERT INTO a VALUES (1)");
                s.execute("INSERT INTO a VALUES (2)");
                s.execute("INSERT INTO b VALUES (1,'one')");
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT a.id, b.v FROM a LEFT JOIN b ON a.id=b.id ORDER BY a.id")) {
                assertTrue(rs.next());
                assertEquals(1, rs.getInt(1));
                assertEquals("one", rs.getString(2));
                assertTrue(rs.next());
                assertEquals(2, rs.getInt(1));
                assertNull(rs.getString(2));
                assertTrue(rs.wasNull());
            }
        }
    }

    // ======================================================================
    // HSQLDB PreparedStatement
    // ======================================================================
    @Test
    public void hsql_preparedQuery() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (PreparedStatement ps = c.prepareStatement(
                     "SELECT name FROM emp WHERE salary > ? ORDER BY salary")) {
                ps.setBigDecimal(1, new BigDecimal("7000.00"));
                try (ResultSet rs = ps.executeQuery()) {
                    List<String> got = new ArrayList<>();
                    while (rs.next()) got.add(rs.getString(1));
                    assertEquals(java.util.Arrays.asList("Bob", "Carol", "Alice"), got);
                }
            }
        }
    }

    @Test
    public void hsql_preparedInsertUpdateDelete() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(20))");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO t VALUES (?,?)")) {
                ps.setInt(1, 10);
                ps.setString(2, "x");
                assertEquals(1, ps.executeUpdate());
            }
            try (PreparedStatement ps = c.prepareStatement("UPDATE t SET v=? WHERE id=?")) {
                ps.setString(1, "y");
                ps.setInt(2, 10);
                assertEquals(1, ps.executeUpdate());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT v FROM t WHERE id=10")) {
                assertTrue(rs.next());
                assertEquals("y", rs.getString(1));
            }
            try (PreparedStatement ps = c.prepareStatement("DELETE FROM t WHERE id=?")) {
                ps.setInt(1, 10);
                assertEquals(1, ps.executeUpdate());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT COUNT(*) FROM t")) {
                assertTrue(rs.next());
                assertEquals(0, rs.getInt(1));
            }
        }
    }

    @Test
    public void hsql_preparedBatch() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE nums (n INT)");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO nums VALUES (?)")) {
                for (int i = 1; i <= 5; i++) {
                    ps.setInt(1, i * 10);
                    ps.addBatch();
                }
                int[] counts = ps.executeBatch();
                assertEquals(5, counts.length);
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT SUM(n) FROM nums")) {
                assertTrue(rs.next());
                assertEquals(150, rs.getInt(1));
            }
        }
    }

    @Test
    public void hsql_typedParameters() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE types_t (i INT, l BIGINT, d DOUBLE, "
                        + "b BOOLEAN, str VARCHAR(20), dec DECIMAL(8,3))");
            }
            try (PreparedStatement ps = c.prepareStatement(
                     "INSERT INTO types_t VALUES (?,?,?,?,?,?)")) {
                ps.setInt(1, 42);
                ps.setLong(2, 9000000000L);
                ps.setDouble(3, 3.5);
                ps.setBoolean(4, true);
                ps.setString(5, "hello");
                ps.setBigDecimal(6, new BigDecimal("1.250"));
                assertEquals(1, ps.executeUpdate());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT * FROM types_t")) {
                assertTrue(rs.next());
                assertEquals(42, rs.getInt("i"));
                assertEquals(9000000000L, rs.getLong("l"));
                assertEquals(3.5, rs.getDouble("d"), 0.0);
                assertTrue(rs.getBoolean("b"));
                assertEquals("hello", rs.getString("str"));
                assertEquals(0, new BigDecimal("1.250").compareTo(rs.getBigDecimal("dec")));
            }
        }
    }

    @Test
    public void hsql_setNull() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE nt (id INT, v VARCHAR(10))");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO nt VALUES (?,?)")) {
                ps.setInt(1, 1);
                ps.setNull(2, Types.VARCHAR);
                ps.executeUpdate();
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT v FROM nt WHERE id=1")) {
                assertTrue(rs.next());
                assertNull(rs.getString(1));
                assertTrue(rs.wasNull());
            }
        }
    }

    // ======================================================================
    // HSQLDB transactions
    // ======================================================================
    @Test
    public void hsql_transactionCommit() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE acc (id INT, bal INT)");
                s.execute("INSERT INTO acc VALUES (1,100)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("UPDATE acc SET bal=bal+50 WHERE id=1");
            }
            c.commit();
            c.setAutoCommit(true);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT bal FROM acc WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals(150, rs.getInt(1));
            }
        }
    }

    @Test
    public void hsql_transactionRollback() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE acc (id INT, bal INT)");
                s.execute("INSERT INTO acc VALUES (1,100)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("UPDATE acc SET bal=bal-30 WHERE id=1");
            }
            c.rollback();
            c.setAutoCommit(true);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT bal FROM acc WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals(100, rs.getInt(1));
            }
        }
    }

    @Test
    public void hsql_savepointRollback() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE sp (id INT)");
            }
            c.setAutoCommit(false);
            try (Statement s = c.createStatement()) {
                s.executeUpdate("INSERT INTO sp VALUES (1)");
                java.sql.Savepoint sv = c.setSavepoint("mid");
                s.executeUpdate("INSERT INTO sp VALUES (2)");
                c.rollback(sv);
            }
            c.commit();
            c.setAutoCommit(true);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT COUNT(*) FROM sp")) {
                assertTrue(rs.next());
                assertEquals(1, rs.getInt(1));
            }
        }
    }

    // ======================================================================
    // HSQLDB metadata, generated keys, scrollable rs, functions
    // ======================================================================
    @Test
    public void hsql_resultSetMetaData() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT id, name, salary FROM emp")) {
                ResultSetMetaData m = rs.getMetaData();
                assertEquals(3, m.getColumnCount());
                assertEquals("ID", m.getColumnName(1).toUpperCase());
                assertEquals("NAME", m.getColumnName(2).toUpperCase());
                assertEquals("SALARY", m.getColumnName(3).toUpperCase());
            }
        }
    }

    @Test
    public void hsql_generatedKeys() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE gk ("
                        + "id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, "
                        + "v VARCHAR(10))");
            }
            try (PreparedStatement ps = c.prepareStatement(
                     "INSERT INTO gk (v) VALUES (?)", Statement.RETURN_GENERATED_KEYS)) {
                ps.setString(1, "a");
                ps.executeUpdate();
                try (ResultSet keys = ps.getGeneratedKeys()) {
                    assertTrue(keys.next());
                    assertEquals(0, keys.getInt(1));
                }
            }
        }
    }

    @Test
    public void hsql_scrollableResultSet() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement(
                     ResultSet.TYPE_SCROLL_INSENSITIVE, ResultSet.CONCUR_READ_ONLY);
                 ResultSet rs = s.executeQuery("SELECT id FROM emp ORDER BY id")) {
                assertTrue(rs.last());
                assertEquals(5, rs.getRow());
                assertEquals(5, rs.getInt(1));
                assertTrue(rs.first());
                assertEquals(1, rs.getInt(1));
                assertTrue(rs.absolute(3));
                assertEquals(3, rs.getInt(1));
            }
        }
    }

    @Test
    public void hsql_stringFunctions() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT UPPER('abc'), LOWER('XYZ'), CHAR_LENGTH('hello'), "
                   + "SUBSTRING('abcdef' FROM 2 FOR 3) FROM (VALUES(0))")) {
                assertTrue(rs.next());
                assertEquals("ABC", rs.getString(1));
                assertEquals("xyz", rs.getString(2));
                assertEquals(5, rs.getInt(3));
                assertEquals("bcd", rs.getString(4));
            }
        }
    }

    @Test
    public void hsql_caseExpression() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT name, CASE WHEN salary >= 8000 THEN 'HIGH' ELSE 'LOW' END "
                   + "FROM emp WHERE id=5")) {
                assertTrue(rs.next());
                assertEquals("Eve", rs.getString(1));
                // HSQLDB types CASE result as CHAR(width of widest branch=4 'HIGH')
                // and right-pads the shorter 'LOW' with a space -> trim to compare.
                assertEquals("LOW", rs.getString(2).trim());
            }
        }
    }

    @Test
    public void hsql_distinctAndLike() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT DISTINCT dept FROM emp ORDER BY dept")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString(1));
                assertEquals(java.util.Arrays.asList("ENG", "HR", "SALES"), got);
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT name FROM emp WHERE name LIKE 'C%'")) {
                assertTrue(rs.next());
                assertEquals("Carol", rs.getString(1));
                assertFalse(rs.next());
            }
        }
    }

    @Test
    public void hsql_dateValue() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE dt (id INT, d DATE)");
            }
            try (PreparedStatement ps = c.prepareStatement("INSERT INTO dt VALUES (?,?)")) {
                ps.setInt(1, 1);
                ps.setDate(2, Date.valueOf("2020-01-15"));
                ps.executeUpdate();
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery("SELECT d FROM dt WHERE id=1")) {
                assertTrue(rs.next());
                assertEquals(Date.valueOf("2020-01-15"), rs.getDate(1));
            }
        }
    }

    @Test
    public void hsql_uniqueConstraintViolation() throws Exception {
        try (Connection c = hsql()) {
            try (Statement s = c.createStatement()) {
                s.execute("CREATE TABLE u (id INT PRIMARY KEY)");
                s.execute("INSERT INTO u VALUES (1)");
            }
            try (Statement s = c.createStatement()) {
                s.execute("INSERT INTO u VALUES (1)");
                fail("expected SQLException for duplicate PK");
            } catch (SQLException expected) {
                assertNotNull(expected.getMessage());
            }
        }
    }

    @Test
    public void hsql_subqueryAndUnion() throws Exception {
        try (Connection c = hsql()) {
            seedEmp(c);
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT name FROM emp WHERE salary = (SELECT MAX(salary) FROM emp)")) {
                assertTrue(rs.next());
                assertEquals("Alice", rs.getString(1));
                assertFalse(rs.next());
            }
            try (Statement s = c.createStatement();
                 ResultSet rs = s.executeQuery(
                     "SELECT dept FROM emp WHERE dept='ENG' "
                   + "UNION SELECT dept FROM emp WHERE dept='HR' ORDER BY dept")) {
                List<String> got = new ArrayList<>();
                while (rs.next()) got.add(rs.getString(1));
                assertEquals(java.util.Arrays.asList("ENG", "HR"), got);
            }
        }
    }
}
