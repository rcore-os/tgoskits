package org.starry.dod;

import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.HashMap;
import java.util.List;
import java.util.Map;


import org.apache.ibatis.annotations.Delete;
import org.apache.ibatis.annotations.Insert;
import org.apache.ibatis.annotations.MapKey;
import org.apache.ibatis.annotations.Options;
import org.apache.ibatis.annotations.Param;
import org.apache.ibatis.annotations.Result;
import org.apache.ibatis.annotations.Results;
import org.apache.ibatis.annotations.Select;
import org.apache.ibatis.annotations.SelectKey;
import org.apache.ibatis.annotations.SelectProvider;
import org.apache.ibatis.annotations.Update;
import org.apache.ibatis.datasource.pooled.PooledDataSource;
import org.apache.ibatis.datasource.unpooled.UnpooledDataSource;
import org.apache.ibatis.executor.BatchResult;
import org.apache.ibatis.jdbc.SQL;
import org.apache.ibatis.logging.nologging.NoLoggingImpl;
import org.apache.ibatis.mapping.Environment;
import org.apache.ibatis.session.Configuration;
import org.apache.ibatis.session.ExecutorType;
import org.apache.ibatis.session.ResultContext;
import org.apache.ibatis.session.ResultHandler;
import org.apache.ibatis.session.RowBounds;
import org.apache.ibatis.session.SqlSession;
import org.apache.ibatis.session.SqlSessionFactory;
import org.apache.ibatis.session.SqlSessionFactoryBuilder;
import org.apache.ibatis.transaction.TransactionFactory;
import org.apache.ibatis.transaction.jdbc.JdbcTransactionFactory;

/**
 * Carpet-level coverage of MyBatis 3.5.16 against the bundled in-memory SQLite
 * database (org.xerial sqlite-jdbc 3.46.1.3, shared-cache memory DB kept alive
 * by a single held connection). Single file, no XML, fully programmatic
 * Configuration. Deterministic: no external network, /tmp only for the SQLite
 * native-lib extraction, memory friendly, single threaded.
 */
public class MyBatisCarpet {

    static final String DRIVER = "org.sqlite.JDBC";
    // Shared-cache in-memory database; survives as long as KEEPALIVE stays open.
    static final String URL = "jdbc:sqlite:file:starrycarpetmem?mode=memory&cache=shared";

    static int ok = 0;
    static int fail = 0;
    static Connection keepAlive;

    static void check(String name, boolean cond) {
        if (cond) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name);
        }
    }

    static void eqI(String name, long expected, long actual) {
        check(name + " (exp=" + expected + " act=" + actual + ")", expected == actual);
    }

    static void eqS(String name, String expected, String actual) {
        check(name + " (exp=" + expected + " act=" + actual + ")",
                expected == null ? actual == null : expected.equals(actual));
    }

    static void contains(String name, String haystack, String needle) {
        check(name + " contains[" + needle + "]", haystack != null && haystack.contains(needle));
    }

    // ---- raw helpers over the keep-alive connection ------------------------

    static void exec(String sql) throws Exception {
        try (Statement st = keepAlive.createStatement()) {
            st.execute(sql);
        }
    }

    static void resetUsers() throws Exception {
        exec("DROP TABLE IF EXISTS users");
        exec("CREATE TABLE users (" +
                "id INTEGER PRIMARY KEY AUTOINCREMENT, " +
                "user_name TEXT NOT NULL, " +
                "email TEXT, " +
                "age INTEGER, " +
                "active INTEGER)");
    }

    static void seed(String name, String email, int age, boolean active) throws Exception {
        try (PreparedStatement ps = keepAlive.prepareStatement(
                "INSERT INTO users(user_name,email,age,active) VALUES(?,?,?,?)")) {
            ps.setString(1, name);
            ps.setString(2, email);
            ps.setInt(3, age);
            ps.setInt(4, active ? 1 : 0);
            ps.executeUpdate();
        }
    }

    static int rawCount() throws Exception {
        try (Statement st = keepAlive.createStatement();
             ResultSet rs = st.executeQuery("SELECT COUNT(*) FROM users")) {
            rs.next();
            return rs.getInt(1);
        }
    }

    static SqlSessionFactory factory;

    public static void main(String[] args) throws Exception {
        Class.forName(DRIVER);
        keepAlive = DriverManager.getConnection(URL);
        try {
            // ---------------------------------------------------------------
            // Phase 1: programmatic Configuration / DataSource / TxFactory /
            //          TypeAlias / Mapper registration + SqlSessionFactoryBuilder
            // ---------------------------------------------------------------
            PooledDataSource pooled = new PooledDataSource(DRIVER, URL, null, null);
            TransactionFactory txf = new JdbcTransactionFactory();
            Environment env = new Environment("dev", txf, pooled);
            Configuration cfg = new Configuration(env);
            cfg.setLogImpl(NoLoggingImpl.class);
            cfg.setCacheEnabled(true);
            cfg.getTypeAliasRegistry().registerAlias("user", User.class);
            cfg.addMapper(UserMapper.class);
            factory = new SqlSessionFactoryBuilder().build(cfg);

            check("factory built", factory != null);
            eqS("env id", "dev", cfg.getEnvironment().getId());
            check("env datasource identity", cfg.getEnvironment().getDataSource() == pooled);
            check("env tx factory type",
                    cfg.getEnvironment().getTransactionFactory() instanceof JdbcTransactionFactory);
            check("hasMapper UserMapper", cfg.hasMapper(UserMapper.class));
            check("typeAlias user resolves",
                    User.class.equals(cfg.getTypeAliasRegistry().resolveAlias("user")));
            check("cache enabled", cfg.isCacheEnabled());
            check("datasource is pooled", cfg.getEnvironment().getDataSource() instanceof PooledDataSource);
            eqS("pooled driver", DRIVER, pooled.getDriver());
            check("mapped statement findById exists",
                    cfg.hasStatement("org.starry.dod.UserMapper.findById"));

            // ---------------------------------------------------------------
            // Phase 2: CRUD via Mapper interface + SqlSession string API
            // ---------------------------------------------------------------
            resetUsers();

            User alice = new User("alice", "alice@x.io", 30, true);
            User bob = new User("bob", "bob@x.io", 25, true);
            User carol = new User("carol", "carol@x.io", 40, false);

            try (SqlSession s = factory.openSession(false)) {
                UserMapper m = s.getMapper(UserMapper.class);
                int r1 = m.insertGenKeys(alice);   // @Options useGeneratedKeys
                int r2 = m.insertSelectKey(bob);    // @SelectKey
                int r3 = s.insert("org.starry.dod.UserMapper.insertPlain", carol);

                eqI("insertGenKeys rows", 1, r1);
                eqI("insertSelectKey rows", 1, r2);
                eqI("session.insert rows", 1, r3);
                check("insertGenKeys populated id", alice.getId() > 0);
                check("insertSelectKey populated id", bob.getId() > 0);
                // carol got its id via plain insert; fetch it back later by name
                s.commit();
            }
            eqI("rawCount after 3 inserts", 3, rawCount());

            // top up to 5 rows with deterministic ages for pagination/search
            seed("dave", "dave@x.io", 35, true);
            seed("eve", "eve@x.io", 20, false);
            eqI("rawCount after seed", 5, rawCount());

            try (SqlSession s = factory.openSession()) {
                UserMapper m = s.getMapper(UserMapper.class);

                // selectOne (mapper) + @Results column->property mapping
                User fa = m.findById(alice.getId());
                check("findById alice not null", fa != null);
                eqS("findById mapped name", "alice", fa.getName());
                eqS("findById email", "alice@x.io", fa.getEmail());
                eqI("findById age", 30, fa.getAge());
                check("findById active=true", fa.isActive());

                User fc = m.findById(carolByName(s));
                check("findById carol active=false", fc != null && !fc.isActive());

                // selectOne via SqlSession string API
                User fbob = s.selectOne("org.starry.dod.UserMapper.findById",
                        java.util.Collections.singletonMap("id", bob.getId()));
                eqS("selectOne string-api name", "bob", fbob.getName());

                // selectList (mapper) + ordering
                List<User> all = m.findAll();
                eqI("findAll size", 5, all.size());
                check("findAll ordered asc",
                        all.get(0).getId() < all.get(all.size() - 1).getId());

                // selectList via SqlSession string API
                List<User> all2 = s.selectList("org.starry.dod.UserMapper.findAll");
                eqI("selectList string-api size", 5, all2.size());

                // selectMap via SqlSession
                Map<Integer, User> byId =
                        s.selectMap("org.starry.dod.UserMapper.findAll", "id");
                eqI("selectMap size", 5, byId.size());
                check("selectMap contains alice id", byId.containsKey(alice.getId()));
                eqS("selectMap alice name", "alice", byId.get(alice.getId()).getName());

                // @MapKey mapper method
                Map<Integer, User> mk = m.findAllAsMap();
                eqI("@MapKey map size", 5, mk.size());
                eqS("@MapKey alice name", "alice", mk.get(alice.getId()).getName());

                // count
                eqI("count() == 5", 5, m.count());

                // ResultHandler
                final int[] visited = {0};
                final StringBuilder names = new StringBuilder();
                s.select("org.starry.dod.UserMapper.findAll", new ResultHandler<User>() {
                    @Override
                    public void handleResult(ResultContext<? extends User> context) {
                        User u = context.getResultObject();
                        visited[0]++;
                        names.append(u.getName()).append(',');
                    }
                });
                eqI("ResultHandler visited", 5, visited[0]);
                contains("ResultHandler names", names.toString(), "alice");
                contains("ResultHandler names dave", names.toString(), "dave");

                // RowBounds pagination via SqlSession string API
                List<User> page = s.selectList("org.starry.dod.UserMapper.findAll",
                        null, new RowBounds(1, 2));
                eqI("RowBounds page size", 2, page.size());
                check("RowBounds skipped first row",
                        page.get(0).getId() == bob.getId());

                // RowBounds as a mapper method parameter
                List<User> page2 = m.findAllPaged(new RowBounds(0, 2));
                eqI("RowBounds mapper page size", 2, page2.size());

                // Dynamic SQL provider: no-arg provider referencing #{name}
                List<User> byName = m.findByNameProvided("alice");
                eqI("provider byName size", 1, byName.size());
                eqS("provider byName name", "alice", byName.get(0).getName());

                // Dynamic SQL provider with optional WHERE assembly (SQL builder)
                eqI("search minAge>=30 size", 3, m.search(30, null).size());
                eqI("search name=bob size", 1, m.search(null, "bob").size());
                eqI("search minAge+name size", 1, m.search(30, "alice").size());
                eqI("search no filter size", 5, m.search(null, null).size());
                eqI("search no-match size", 0, m.search(1000, null).size());

                // boundary: missing row -> selectOne null
                check("findById missing -> null", m.findById(999999) == null);

                // update (mapper) + commit + re-read
                int u1 = m.updateEmail(alice.getId(), "alice2@x.io");
                eqI("updateEmail rows", 1, u1);
                s.commit();
                eqS("update reflected", "alice2@x.io", m.findById(alice.getId()).getEmail());

                // update via SqlSession string API
                Map<String, Object> up = new HashMap<>();
                up.put("id", bob.getId());
                up.put("email", "bob2@x.io");
                int u2 = s.update("org.starry.dod.UserMapper.updateEmail", up);
                eqI("session.update rows", 1, u2);
                s.commit();
                eqS("session.update reflected", "bob2@x.io", m.findById(bob.getId()).getEmail());

                // update non-existent -> 0
                eqI("update missing rows", 0, m.updateEmail(999999, "none"));

                // delete (mapper) + commit
                int d1 = m.deleteById(daveId(s));
                eqI("deleteById rows", 1, d1);
                s.commit();
                eqI("count after delete == 4", 4, m.count());

                // delete via SqlSession string API
                int d2 = s.delete("org.starry.dod.UserMapper.deleteById",
                        java.util.Collections.singletonMap("id", eveId(s)));
                eqI("session.delete rows", 1, d2);
                s.commit();
                eqI("count after 2nd delete == 3", 3, m.count());

                // delete non-existent -> 0
                eqI("delete missing rows", 0, m.deleteById(999999));
            }

            // ---------------------------------------------------------------
            // Phase 3: local (L1) cache identity semantics
            // ---------------------------------------------------------------
            resetUsers();
            seed("solo", "solo@x.io", 50, true);
            int soloId;
            try (SqlSession cs = factory.openSession(false)) {
                UserMapper m = cs.getMapper(UserMapper.class);
                soloId = m.findAll().get(0).getId();
                User a = m.findById(soloId);
                User b = m.findById(soloId);
                check("L1 cache returns same instance", a == b);
                cs.clearCache();
                User c = m.findById(soloId);
                check("clearCache yields new instance", a != c);
                eqS("clearCache value preserved", "solo", c.getName());
            }
            try (SqlSession cs2 = factory.openSession(false)) {
                User d = cs2.getMapper(UserMapper.class).findById(soloId);
                eqS("fresh session value", "solo", d.getName());
            }

            // ---------------------------------------------------------------
            // Phase 4: transaction commit / rollback / autocommit
            // ---------------------------------------------------------------
            resetUsers();
            try (SqlSession tx = factory.openSession(false)) {
                tx.getMapper(UserMapper.class).insertPlain(new User("rb", "rb@x.io", 1, true));
                tx.rollback();
            }
            eqI("rollback leaves 0 rows", 0, rawCount());

            try (SqlSession tx = factory.openSession(false)) {
                tx.getMapper(UserMapper.class).insertPlain(new User("cm", "cm@x.io", 2, true));
                tx.commit();
            }
            eqI("commit leaves 1 row", 1, rawCount());

            try (SqlSession tx = factory.openSession(true)) { // autoCommit
                tx.getMapper(UserMapper.class).insertPlain(new User("ac", "ac@x.io", 3, true));
            }
            eqI("autoCommit makes 2 rows", 2, rawCount());

            // ---------------------------------------------------------------
            // Phase 5: batch executor (ExecutorType.BATCH)
            // ---------------------------------------------------------------
            resetUsers();
            try (SqlSession bs = factory.openSession(ExecutorType.BATCH, false)) {
                UserMapper m = bs.getMapper(UserMapper.class);
                for (int i = 0; i < 10; i++) {
                    m.insertPlain(new User("b" + i, "b" + i + "@x.io", 10 + i, i % 2 == 0));
                }
                List<BatchResult> results = bs.flushStatements();
                check("batch flush produced results", results != null && !results.isEmpty());
                bs.commit();
            }
            eqI("batch inserted 10 rows", 10, rawCount());

            // ---------------------------------------------------------------
            // Phase 6: UnpooledDataSource secondary factory (same memory DB)
            // ---------------------------------------------------------------
            UnpooledDataSource unpooled = new UnpooledDataSource(DRIVER, URL, null, null);
            Environment env2 = new Environment("unpooled", new JdbcTransactionFactory(), unpooled);
            Configuration cfg2 = new Configuration(env2);
            cfg2.setLogImpl(NoLoggingImpl.class);
            cfg2.addMapper(UserMapper.class);
            SqlSessionFactory factory2 = new SqlSessionFactoryBuilder().build(cfg2);
            check("unpooled datasource type",
                    cfg2.getEnvironment().getDataSource() instanceof UnpooledDataSource);
            eqS("unpooled driver", DRIVER, unpooled.getDriver());
            try (SqlSession us = factory2.openSession()) {
                eqI("unpooled sees batch rows", 10, us.getMapper(UserMapper.class).findAll().size());
            }

            // ---------------------------------------------------------------
            // Phase 7: org.apache.ibatis.jdbc.SQL builder (standalone, no DB)
            // ---------------------------------------------------------------
            String sel = new SQL() {{
                SELECT("id");
                SELECT("user_name");
                FROM("users");
                WHERE("age > #{age}");
                ORDER_BY("id DESC");
            }}.toString();
            contains("SQL select SELECT", sel, "SELECT");
            contains("SQL select FROM", sel, "FROM users");
            contains("SQL select WHERE", sel, "WHERE");
            contains("SQL select ORDER BY", sel, "ORDER BY");

            String ins = new SQL() {{
                INSERT_INTO("users");
                VALUES("user_name", "#{name}");
                VALUES("email", "#{email}");
            }}.toString();
            contains("SQL insert INSERT INTO", ins, "INSERT INTO users");
            contains("SQL insert VALUES", ins, "VALUES");

            String upd = new SQL() {{
                UPDATE("users");
                SET("email = #{email}");
                WHERE("id = #{id}");
            }}.toString();
            contains("SQL update UPDATE", upd, "UPDATE users");
            contains("SQL update SET", upd, "SET");
            contains("SQL update WHERE", upd, "WHERE");

            String del = new SQL() {{
                DELETE_FROM("users");
                WHERE("id = #{id}");
            }}.toString();
            contains("SQL delete DELETE FROM", del, "DELETE FROM users");
            contains("SQL delete WHERE", del, "WHERE");

        } finally {
            try {
                if (keepAlive != null) {
                    keepAlive.close();
                }
            } catch (Exception ignore) {
                // ignore
            }
        }

        System.out.println("MYBATIS_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("MYBATIS_DONE");
        }
    }

    // helpers to look up auto-increment ids by name (within an open session)
    static int carolByName(SqlSession s) {
        return idByName(s, "carol");
    }

    static int daveId(SqlSession s) {
        return idByName(s, "dave");
    }

    static int eveId(SqlSession s) {
        return idByName(s, "eve");
    }

    static int idByName(SqlSession s, String name) {
        List<User> all = s.getMapper(UserMapper.class).findAll();
        for (User u : all) {
            if (name.equals(u.getName())) {
                return u.getId();
            }
        }
        return -1;
    }

    /** Dynamic SQL provider built with the fluent {@link SQL} builder. */
    public static class UserSqlProvider {

        public UserSqlProvider() {
        }

        public String byName() {
            return new SQL() {{
                SELECT("id, user_name, email, age, active");
                FROM("users");
                WHERE("user_name = #{name}");
                ORDER_BY("id");
            }}.toString();
        }

        public String search(Map<String, Object> p) {
            return new SQL() {{
                SELECT("id, user_name, email, age, active");
                FROM("users");
                if (p.get("minAge") != null) {
                    WHERE("age >= #{minAge}");
                }
                if (p.get("name") != null) {
                    WHERE("user_name = #{name}");
                }
                ORDER_BY("id");
            }}.toString();
        }
    }
}

/** Plain mapped POJO. */
class User {
    private int id;
    private String name;
    private String email;
    private int age;
    private boolean active;

    public User() {
    }

    public User(String name, String email, int age, boolean active) {
        this.name = name;
        this.email = email;
        this.age = age;
        this.active = active;
    }

    public Map<String, Object> asMap() {
        Map<String, Object> m = new HashMap<>();
        m.put("name", name);
        m.put("email", email);
        m.put("age", age);
        m.put("active", active);
        return m;
    }

    public int getId() {
        return id;
    }

    public void setId(int id) {
        this.id = id;
    }

    public String getName() {
        return name;
    }

    public void setName(String name) {
        this.name = name;
    }

    public String getEmail() {
        return email;
    }

    public void setEmail(String email) {
        this.email = email;
    }

    public int getAge() {
        return age;
    }

    public void setAge(int age) {
        this.age = age;
    }

    public boolean isActive() {
        return active;
    }

    public void setActive(boolean active) {
        this.active = active;
    }
}

/** Annotation-driven mapper interface exercised by the carpet. */
interface UserMapper {

    @Insert("INSERT INTO users(user_name,email,age,active) "
            + "VALUES(#{name},#{email},#{age},#{active})")
    @Options(useGeneratedKeys = true, keyProperty = "id")
    int insertGenKeys(User u);

    @Insert("INSERT INTO users(user_name,email,age,active) "
            + "VALUES(#{name},#{email},#{age},#{active})")
    @SelectKey(statement = "SELECT last_insert_rowid()", keyProperty = "id",
            before = false, resultType = int.class)
    int insertSelectKey(User u);

    @Insert("INSERT INTO users(user_name,email,age,active) "
            + "VALUES(#{name},#{email},#{age},#{active})")
    int insertPlain(User u);

    @Select("SELECT id, user_name, email, age, active FROM users WHERE id = #{id}")
    @Results({
            @Result(id = true, property = "id", column = "id"),
            @Result(property = "name", column = "user_name"),
            @Result(property = "email", column = "email"),
            @Result(property = "age", column = "age"),
            @Result(property = "active", column = "active", javaType = boolean.class)
    })
    User findById(@Param("id") int id);

    @Select("SELECT id, user_name, email, age, active FROM users ORDER BY id")
    @Results({
            @Result(id = true, property = "id", column = "id"),
            @Result(property = "name", column = "user_name"),
            @Result(property = "email", column = "email"),
            @Result(property = "age", column = "age"),
            @Result(property = "active", column = "active", javaType = boolean.class)
    })
    List<User> findAll();

    @Select("SELECT id, user_name, email, age, active FROM users ORDER BY id")
    @Results({
            @Result(id = true, property = "id", column = "id"),
            @Result(property = "name", column = "user_name"),
            @Result(property = "email", column = "email"),
            @Result(property = "age", column = "age"),
            @Result(property = "active", column = "active", javaType = boolean.class)
    })
    @MapKey("id")
    Map<Integer, User> findAllAsMap();

    @Select("SELECT id, user_name, email, age, active FROM users ORDER BY id")
    @Results({
            @Result(id = true, property = "id", column = "id"),
            @Result(property = "name", column = "user_name"),
            @Result(property = "email", column = "email"),
            @Result(property = "age", column = "age"),
            @Result(property = "active", column = "active", javaType = boolean.class)
    })
    List<User> findAllPaged(RowBounds rowBounds);

    @SelectProvider(type = MyBatisCarpet.UserSqlProvider.class, method = "byName")
    @Results({
            @Result(id = true, property = "id", column = "id"),
            @Result(property = "name", column = "user_name"),
            @Result(property = "email", column = "email"),
            @Result(property = "age", column = "age"),
            @Result(property = "active", column = "active", javaType = boolean.class)
    })
    List<User> findByNameProvided(@Param("name") String name);

    @SelectProvider(type = MyBatisCarpet.UserSqlProvider.class, method = "search")
    @Results({
            @Result(id = true, property = "id", column = "id"),
            @Result(property = "name", column = "user_name"),
            @Result(property = "email", column = "email"),
            @Result(property = "age", column = "age"),
            @Result(property = "active", column = "active", javaType = boolean.class)
    })
    List<User> search(@Param("minAge") Integer minAge, @Param("name") String name);

    @Select("SELECT COUNT(*) FROM users")
    long count();

    @Update("UPDATE users SET email = #{email} WHERE id = #{id}")
    int updateEmail(@Param("id") int id, @Param("email") String email);

    @Delete("DELETE FROM users WHERE id = #{id}")
    int deleteById(@Param("id") int id);
}
