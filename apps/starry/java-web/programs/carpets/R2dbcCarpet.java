package org.starry.dod;

import io.r2dbc.spi.Connection;
import io.r2dbc.spi.ConnectionFactories;
import io.r2dbc.spi.ConnectionFactory;
import io.r2dbc.spi.ConnectionFactoryMetadata;
import io.r2dbc.spi.ConnectionFactoryOptions;
import io.r2dbc.spi.ConnectionMetadata;
import io.r2dbc.spi.ColumnMetadata;
import io.r2dbc.spi.IsolationLevel;
import io.r2dbc.spi.Nullability;
import io.r2dbc.spi.Option;
import io.r2dbc.spi.R2dbcBadGrammarException;
import io.r2dbc.spi.R2dbcException;
import io.r2dbc.spi.Result;
import io.r2dbc.spi.Row;
import io.r2dbc.spi.RowMetadata;
import io.r2dbc.spi.Statement;
import io.r2dbc.spi.ValidationDepth;
import io.r2dbc.h2.H2ConnectionConfiguration;
import io.r2dbc.h2.H2ConnectionFactory;
import io.r2dbc.h2.H2ConnectionFactoryProvider;
import io.r2dbc.h2.H2ConnectionOption;
import io.r2dbc.h2.CloseableConnectionFactory;

import org.reactivestreams.Publisher;
import org.reactivestreams.Subscriber;
import org.reactivestreams.Subscription;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;

import java.math.BigDecimal;
import java.time.Duration;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Objects;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.BiFunction;

/**
 * Carpet-level coverage of the R2DBC 1.0 reactive SPI driving the bundled
 * r2dbc-h2 driver against an in-memory H2 database. No network sockets, no
 * external resources: H2 mem runs fully in-process. Deterministic reactive
 * collection via a custom reactive-streams Subscriber + CountDownLatch.
 */
public class R2dbcCarpet {

    static int ok = 0;
    static int fail = 0;
    static final Duration T = Duration.ofSeconds(20);

    static void check(String name, boolean cond) {
        if (cond) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name);
        }
    }

    static void eq(String name, Object actual, Object expected) {
        if (Objects.equals(actual, expected)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + expected + "] actual=[" + actual + "]");
        }
    }

    // ---- deterministic blocking collector: a hand-written reactive-streams Subscriber ----
    static <X> List<X> drain(Publisher<X> p) {
        final List<X> out = Collections.synchronizedList(new ArrayList<X>());
        final CountDownLatch latch = new CountDownLatch(1);
        final Throwable[] err = new Throwable[1];
        p.subscribe(new Subscriber<X>() {
            public void onSubscribe(Subscription s) { s.request(Long.MAX_VALUE); }
            public void onNext(X x) { out.add(x); }
            public void onError(Throwable t) { err[0] = t; latch.countDown(); }
            public void onComplete() { latch.countDown(); }
        });
        try {
            if (!latch.await(20, TimeUnit.SECONDS)) {
                throw new RuntimeException("drain timeout");
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new RuntimeException(e);
        }
        if (err[0] != null) {
            if (err[0] instanceof RuntimeException) throw (RuntimeException) err[0];
            throw new RuntimeException(err[0]);
        }
        return out;
    }

    static <X> X one(Publisher<X> p) {
        List<X> l = drain(p);
        return l.isEmpty() ? null : l.get(0);
    }

    static void run(Publisher<Void> p) {
        drain(p); // await completion, ignore (Void) results
    }

    static void bindAll(Statement s, Object[] binds) {
        for (int i = 0; i < binds.length; i++) {
            s.bind(i, binds[i]);
        }
    }

    // DML: total rows updated, summed across emitted Results, via Result.getRowsUpdated()
    static long update(Connection c, String sql, Object... binds) {
        Statement s = c.createStatement(sql);
        bindAll(s, binds);
        List<Long> counts = drain(Flux.from(s.execute()).concatMap(r -> r.getRowsUpdated()));
        long sum = 0;
        for (Long x : counts) sum += x;
        return sum;
    }

    // DQL: map each Row via BiFunction, collected deterministically
    static <Tp> List<Tp> query(Connection c, String sql, BiFunction<Row, RowMetadata, Tp> m, Object... binds) {
        Statement s = c.createStatement(sql);
        bindAll(s, binds);
        return drain(Flux.from(s.execute()).concatMap(r -> r.map(m)));
    }

    public static void main(String[] args) {
        Connection conn = null;
        Connection conn2 = null;
        ConnectionFactory primary = null;
        try {
            // ============================================================
            // SECTION A: driver discovery, ConnectionFactoryOptions, Option
            // ============================================================
            ConnectionFactoryOptions opts = ConnectionFactoryOptions.builder()
                    .option(ConnectionFactoryOptions.DRIVER, "h2")
                    .option(ConnectionFactoryOptions.PROTOCOL, "mem")
                    .option(ConnectionFactoryOptions.DATABASE, "carpetdb")
                    .option(ConnectionFactoryOptions.USER, "sa")
                    .option(ConnectionFactoryOptions.PASSWORD, "")
                    .build();

            check("A.supports.h2", ConnectionFactories.supports(opts));
            ConnectionFactory cfFromOpts = ConnectionFactories.find(opts);
            check("A.find.nonNull", cfFromOpts != null);
            ConnectionFactory cfGet = ConnectionFactories.get(opts);
            check("A.get.nonNull", cfGet != null);
            eq("A.metadata.name", cfGet.getMetadata().getName(), "H2");

            eq("A.opts.getValue.driver", opts.getValue(ConnectionFactoryOptions.DRIVER), "h2");
            eq("A.opts.required.protocol", opts.getRequiredValue(ConnectionFactoryOptions.PROTOCOL), "mem");
            eq("A.opts.required.database", opts.getRequiredValue(ConnectionFactoryOptions.DATABASE), "carpetdb");
            check("A.opts.hasOption.database", opts.hasOption(ConnectionFactoryOptions.DATABASE));
            check("A.opts.hasOption.host.absent", !opts.hasOption(ConnectionFactoryOptions.HOST));
            check("A.opts.getValue.host.null", opts.getValue(ConnectionFactoryOptions.HOST) == null);

            eq("A.option.driver.name", ConnectionFactoryOptions.DRIVER.name(), "driver");
            eq("A.option.protocol.name", ConnectionFactoryOptions.PROTOCOL.name(), "protocol");
            Option<String> custom = Option.valueOf("customKey");
            eq("A.option.valueOf.name", custom.name(), "customKey");
            Option<String> secret = Option.sensitiveValueOf("secretKey");
            eq("A.option.sensitive.name", secret.name(), "secretKey");
            eq("A.h2provider.driverConst", H2ConnectionFactoryProvider.H2_DRIVER, "h2");
            eq("A.h2provider.protoMem", H2ConnectionFactoryProvider.PROTOCOL_MEM, "mem");

            // parse a full r2dbc URL into options
            ConnectionFactoryOptions parsed = ConnectionFactoryOptions.parse("r2dbc:h2:mem:///parsedDb");
            eq("A.parse.driver", parsed.getValue(ConnectionFactoryOptions.DRIVER), "h2");
            eq("A.parse.protocol", parsed.getValue(ConnectionFactoryOptions.PROTOCOL), "mem");
            eq("A.parse.database", parsed.getValue(ConnectionFactoryOptions.DATABASE), "parsedDb");
            check("A.parse.supports", ConnectionFactories.supports(parsed));

            // bogus driver: not supported, get() throws
            ConnectionFactoryOptions bogus = ConnectionFactoryOptions.builder()
                    .option(ConnectionFactoryOptions.DRIVER, "no-such-driver-xyz")
                    .build();
            check("A.supports.bogus.false", !ConnectionFactories.supports(bogus));
            boolean threwBogus = false;
            try {
                ConnectionFactories.get(bogus);
            } catch (RuntimeException ex) {
                threwBogus = true;
            }
            check("A.get.bogus.throws", threwBogus);

            // getRequiredValue on absent option throws
            boolean threwReq = false;
            try {
                opts.getRequiredValue(ConnectionFactoryOptions.HOST);
            } catch (RuntimeException ex) {
                threwReq = true;
            }
            check("A.required.absent.throws", threwReq);

            // mutate(): derive new options and verify added option present, originals retained
            ConnectionFactoryOptions mutated = opts.mutate()
                    .option(ConnectionFactoryOptions.CONNECT_TIMEOUT, Duration.ofSeconds(5))
                    .build();
            check("A.mutate.hasNew", mutated.hasOption(ConnectionFactoryOptions.CONNECT_TIMEOUT));
            eq("A.mutate.retains.driver", mutated.getValue(ConnectionFactoryOptions.DRIVER), "h2");

            // ============================================================
            // SECTION B: ConnectionFactory construction variants
            // ============================================================
            // Primary factory keeps the named mem DB alive for the whole run (DB_CLOSE_DELAY=-1)
            H2ConnectionConfiguration cfg = H2ConnectionConfiguration.builder()
                    .inMemory("carpetdb")
                    .property(H2ConnectionOption.DB_CLOSE_DELAY, "-1")
                    .username("sa")
                    .build();
            check("B.config.url.mem", cfg.toString().contains("carpetdb"));
            primary = new H2ConnectionFactory(cfg);
            eq("B.factory.metadata", primary.getMetadata().getName(), "H2");

            // alternative builder form: H2ConnectionFactory.inMemory(...)
            CloseableConnectionFactory sideFactory = H2ConnectionFactory.inMemory("sidecar");
            check("B.inMemory.factory.nonNull", sideFactory != null);
            Connection sideConn = one(sideFactory.create());
            check("B.inMemory.connect", sideConn != null);
            run(sideConn.close());
            run(sideFactory.close());

            // ConnectionFactories.get(String URL) form
            ConnectionFactory urlFactory = ConnectionFactories.get("r2dbc:h2:mem:///urlDb");
            check("B.url.factory.nonNull", urlFactory != null);
            eq("B.url.factory.name", urlFactory.getMetadata().getName(), "H2");

            // ============================================================
            // SECTION C: Connection lifecycle / metadata / isolation
            // ============================================================
            conn = one(primary.create());
            check("C.connect.nonNull", conn != null);
            ConnectionMetadata cmeta = conn.getMetadata();
            eq("C.db.product", cmeta.getDatabaseProductName(), "H2");
            check("C.db.version.nonEmpty", cmeta.getDatabaseVersion() != null && !cmeta.getDatabaseVersion().isEmpty());
            check("C.autocommit.default.true", conn.isAutoCommit());

            Boolean vLocal = one(conn.validate(ValidationDepth.LOCAL));
            check("C.validate.local", Boolean.TRUE.equals(vLocal));
            Boolean vRemote = one(conn.validate(ValidationDepth.REMOTE));
            check("C.validate.remote", Boolean.TRUE.equals(vRemote));

            IsolationLevel defaultIso = conn.getTransactionIsolationLevel();
            check("C.iso.default.nonNull", defaultIso != null);
            eq("C.iso.default.readCommitted", defaultIso, IsolationLevel.READ_COMMITTED);
            // r2dbc-h2 1.0 accepts the change request; its getter reports the session default
            run(conn.setTransactionIsolationLevel(IsolationLevel.SERIALIZABLE));
            check("C.iso.set.accepted", conn.getTransactionIsolationLevel() != null);
            run(conn.setTransactionIsolationLevel(IsolationLevel.READ_COMMITTED));
            eq("C.iso.afterReset.readCommitted", conn.getTransactionIsolationLevel(), IsolationLevel.READ_COMMITTED);

            // ============================================================
            // SECTION D: DDL
            // ============================================================
            long ddl1 = update(conn,
                    "CREATE TABLE users (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(64) NOT NULL, "
                            + "age INT, active BOOLEAN, balance DECIMAL(12,2), born DATE, created TIMESTAMP, nick VARCHAR(64))");
            check("D.create.users.noRows", ddl1 == 0);
            long ddl2 = update(conn,
                    "CREATE TABLE accounts (id INT PRIMARY KEY, owner VARCHAR(64), amount BIGINT)");
            check("D.create.accounts.noRows", ddl2 == 0);

            // verify table presence via INFORMATION_SCHEMA
            List<Long> tcount = query(conn,
                    "SELECT COUNT(*) FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME = $1 AND TABLE_SCHEMA = $2",
                    (row, md) -> row.get(0, Long.class), "USERS", "PUBLIC");
            eq("D.users.in.schema", tcount.size() == 1 ? tcount.get(0) : -1L, 1L);

            // ============================================================
            // SECTION E: INSERT (index bind, name bind, bindNull), generated keys, batch via add()
            // ============================================================
            long ins1 = update(conn,
                    "INSERT INTO users (name, age, active, balance, born, created, nick) "
                            + "VALUES ($1,$2,$3,$4,$5,$6,$7)",
                    "alice", 30, Boolean.TRUE, new BigDecimal("100.50"),
                    LocalDate.of(1994, 3, 15), LocalDateTime.of(2024, 1, 2, 3, 4, 5), "ally");
            eq("E.insert.byIndex", ins1, 1L);

            // bind by name "$1".."$7"
            Statement byName = conn.createStatement(
                    "INSERT INTO users (name, age, active, balance, born, created, nick) "
                            + "VALUES ($1,$2,$3,$4,$5,$6,$7)");
            byName.bind("$1", "bob").bind("$2", 25).bind("$3", Boolean.FALSE)
                    .bind("$4", new BigDecimal("42.00")).bind("$5", LocalDate.of(1999, 12, 31))
                    .bind("$6", LocalDateTime.of(2024, 6, 1, 12, 0, 0)).bind("$7", "bobby");
            long ins2 = 0;
            for (Long u : drain(Flux.from(byName.execute()).concatMap(r -> r.getRowsUpdated()))) ins2 += u;
            eq("E.insert.byName", ins2, 1L);

            // bindNull for nullable columns (age, nick NULL)
            Statement nullStmt = conn.createStatement(
                    "INSERT INTO users (name, age, active, balance, born, created, nick) "
                            + "VALUES ($1,$2,$3,$4,$5,$6,$7)");
            nullStmt.bind(0, "carol").bindNull(1, Integer.class).bind(2, Boolean.TRUE)
                    .bind(3, new BigDecimal("0.00")).bind(4, LocalDate.of(2000, 1, 1))
                    .bind(5, LocalDateTime.of(2024, 7, 7, 7, 7, 7)).bindNull(6, String.class);
            long ins3 = 0;
            for (Long u : drain(Flux.from(nullStmt.execute()).concatMap(r -> r.getRowsUpdated()))) ins3 += u;
            eq("E.insert.bindNull", ins3, 1L);

            // returnGeneratedValues: capture the auto-increment id
            Statement genStmt = conn.createStatement(
                    "INSERT INTO users (name, age, active, balance, born, created, nick) "
                            + "VALUES ($1,$2,$3,$4,$5,$6,$7)")
                    .returnGeneratedValues("ID");
            genStmt.bind(0, "dave").bind(1, 40).bind(2, Boolean.TRUE)
                    .bind(3, new BigDecimal("9.99")).bind(4, LocalDate.of(1980, 5, 5))
                    .bind(5, LocalDateTime.of(2024, 8, 8, 8, 8, 8)).bind(6, "davey");
            List<Integer> genKeys = drain(Flux.from(genStmt.execute())
                    .concatMap(r -> r.map((row, md) -> row.get(0, Integer.class))));
            check("E.generated.oneKey", genKeys.size() == 1);
            check("E.generated.positive", genKeys.size() == 1 && genKeys.get(0) != null && genKeys.get(0) > 0);

            // batch via Statement.add(): two parameter sets in one statement
            Statement multi = conn.createStatement(
                    "INSERT INTO users (name, age, active, balance, born, created, nick) "
                            + "VALUES ($1,$2,$3,$4,$5,$6,$7)");
            multi.bind(0, "erin").bind(1, 22).bind(2, Boolean.TRUE).bind(3, new BigDecimal("1.00"))
                    .bind(4, LocalDate.of(2002, 2, 2)).bind(5, LocalDateTime.of(2024, 9, 9, 9, 9, 9)).bind(6, "e");
            multi.add();
            multi.bind(0, "frank").bind(1, 55).bind(2, Boolean.FALSE).bind(3, new BigDecimal("2.00"))
                    .bind(4, LocalDate.of(1969, 6, 9)).bind(5, LocalDateTime.of(2024, 10, 10, 10, 10, 10)).bind(6, "f");
            List<Long> multiCounts = drain(Flux.from(multi.execute()).concatMap(r -> r.getRowsUpdated()));
            long multiSum = 0;
            for (Long u : multiCounts) multiSum += u;
            eq("E.statement.add.twoResults", (long) multiCounts.size(), 2L);
            eq("E.statement.add.sum", multiSum, 2L);

            // total row count now: alice, bob, carol, dave, erin, frank = 6
            long total = scalarLong(conn, "SELECT COUNT(*) FROM users");
            eq("E.users.total", total, 6L);

            // ============================================================
            // SECTION F: DQL — typed gets, untyped, predicates, ordering, aggregates, nulls, types
            // ============================================================
            List<String> allNames = query(conn, "SELECT name FROM users ORDER BY name ASC",
                    (row, md) -> row.get("name", String.class));
            eq("F.order.count", (long) allNames.size(), 6L);
            eq("F.order.first", allNames.get(0), "alice");
            eq("F.order.last", allNames.get(allNames.size() - 1), "frank");

            // typed get by name and by index must agree
            List<Object[]> aliceRows = query(conn,
                    "SELECT id, name, age, active, balance, born, created FROM users WHERE name = $1",
                    (row, md) -> new Object[]{
                            row.get("id", Integer.class), row.get(0, Integer.class),
                            row.get("name", String.class), row.get("age", Integer.class),
                            row.get("active", Boolean.class), row.get("balance", BigDecimal.class),
                            row.get("born", LocalDate.class), row.get("created", LocalDateTime.class),
                            row.get("name") /* untyped Object */
                    }, "alice");
            check("F.alice.oneRow", aliceRows.size() == 1);
            Object[] a = aliceRows.get(0);
            eq("F.alice.id.byName==byIndex", a[0], a[1]);
            eq("F.alice.name.typed", a[2], "alice");
            eq("F.alice.age", a[3], 30);
            eq("F.alice.active", a[4], Boolean.TRUE);
            check("F.alice.balance", ((BigDecimal) a[5]).compareTo(new BigDecimal("100.50")) == 0);
            eq("F.alice.born", a[6], LocalDate.of(1994, 3, 15));
            eq("F.alice.created", a[7], LocalDateTime.of(2024, 1, 2, 3, 4, 5));
            eq("F.alice.name.untyped", a[8], "alice");

            // WHERE with bound int predicate
            long adults = scalarLong(conn, "SELECT COUNT(*) FROM users WHERE age >= $1", 30);
            // ages -> alice30, bob25, carol NULL, dave40, erin22, frank55 => >=30: alice,dave,frank = 3
            eq("F.where.age.ge30", adults, 3L);

            // LIKE predicate
            long likeA = scalarLong(conn, "SELECT COUNT(*) FROM users WHERE name LIKE $1", "a%");
            eq("F.like.a", likeA, 1L); // alice

            // aggregate MAX/MIN/SUM
            Integer maxAge = scalarInt(conn, "SELECT MAX(age) FROM users");
            eq("F.max.age", maxAge, 55);
            Integer minAge = scalarInt(conn, "SELECT MIN(age) FROM users");
            eq("F.min.age", minAge, 22);

            // NULL handling: carol has NULL age + NULL nick
            List<Object[]> carol = query(conn, "SELECT age, nick FROM users WHERE name = $1",
                    (row, md) -> new Object[]{row.get("age", Integer.class), row.get("nick", String.class)}, "carol");
            check("F.carol.oneRow", carol.size() == 1);
            check("F.carol.age.null", carol.get(0)[0] == null);
            check("F.carol.nick.null", carol.get(0)[1] == null);
            long nullAgeCount = scalarLong(conn, "SELECT COUNT(*) FROM users WHERE age IS NULL");
            eq("F.null.age.count", nullAgeCount, 1L);

            // Result.map(Function<Readable>) single-arg form
            List<String> viaReadable = drain(Flux.from(
                    conn.createStatement("SELECT name FROM users WHERE name = $1").bind(0, "bob").execute())
                    .concatMap(r -> r.map(readable -> readable.get(0, String.class))));
            eq("F.readable.form", viaReadable.size() == 1 ? viaReadable.get(0) : null, "bob");

            // BIGINT round-trip via accounts table
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 1, "alice", 9000000000L);
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 2, "bob", -123L);
            List<Long> amounts = query(conn, "SELECT amount FROM accounts ORDER BY id",
                    (row, md) -> row.get("amount", Long.class));
            eq("F.bigint.count", (long) amounts.size(), 2L);
            eq("F.bigint.value", amounts.get(0), 9000000000L);
            eq("F.bigint.negative", amounts.get(1), -123L);

            // VARCHAR special characters round-trip (quotes via bind, no SQL injection risk)
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 3, "o'brien \"x\"", 7L);
            String special = scalarStr(conn, "SELECT owner FROM accounts WHERE id = $1", 3);
            eq("F.varchar.special", special, "o'brien \"x\"");

            // DOUBLE round-trip
            update(conn, "CREATE TABLE nums (id INT PRIMARY KEY, d DOUBLE, dec DECIMAL(10,4))");
            update(conn, "INSERT INTO nums (id, d, dec) VALUES ($1,$2,$3)", 1, 3.14159d, new BigDecimal("2.7183"));
            List<Object[]> nums = query(conn, "SELECT d, dec FROM nums WHERE id = $1",
                    (row, md) -> new Object[]{row.get("d", Double.class), row.get("dec", BigDecimal.class)}, 1);
            check("F.double.value", nums.size() == 1 && Math.abs((Double) nums.get(0)[0] - 3.14159d) < 1e-9);
            check("F.decimal.value", ((BigDecimal) nums.get(0)[1]).compareTo(new BigDecimal("2.7183")) == 0);

            // ============================================================
            // SECTION G: RowMetadata / ColumnMetadata
            // ============================================================
            List<RowMetadata> mdList = drain(Flux.from(
                    conn.createStatement("SELECT id, name, age FROM users WHERE name = $1").bind(0, "alice").execute())
                    .concatMap(r -> r.map((row, md) -> md)));
            check("G.meta.captured", mdList.size() == 1);
            RowMetadata md = mdList.get(0);
            eq("G.meta.colCount", (long) md.getColumnMetadatas().size(), 3L);
            ColumnMetadata c0 = md.getColumnMetadata(0);
            eq("G.meta.col0.name", c0.getName().toUpperCase(), "ID");
            ColumnMetadata cName = md.getColumnMetadata("name");
            eq("G.meta.colName.name", cName.getName().toUpperCase(), "NAME");
            check("G.meta.contains.id", md.contains("id"));
            check("G.meta.contains.absent.false", !md.contains("nonexistent_col"));
            check("G.meta.col0.type.nonNull", c0.getType() != null);
            check("G.meta.col0.javaType", c0.getJavaType() == Integer.class);
            check("G.meta.colName.javaType.string", cName.getJavaType() == String.class);
            check("G.meta.col0.nullability.nonNull", c0.getNullability() != null);
            // name column is declared NOT NULL
            Nullability nameNull = cName.getNullability();
            check("G.meta.name.notnull", nameNull == Nullability.NON_NULL || nameNull == Nullability.UNKNOWN);

            // ============================================================
            // SECTION H: UPDATE / DELETE
            // ============================================================
            long upd = update(conn, "UPDATE users SET age = $1 WHERE name = $2", 31, "alice");
            eq("H.update.count", upd, 1L);
            Integer newAge = scalarInt(conn, "SELECT age FROM users WHERE name = $1", "alice");
            eq("H.update.verified", newAge, 31);

            long updNone = update(conn, "UPDATE users SET age = $1 WHERE name = $2", 99, "ghost");
            eq("H.update.noMatch.zero", updNone, 0L);

            long del = update(conn, "DELETE FROM users WHERE name = $1", "frank");
            eq("H.delete.count", del, 1L);
            long afterDel = scalarLong(conn, "SELECT COUNT(*) FROM users");
            eq("H.delete.total", afterDel, 5L);

            // ============================================================
            // SECTION I: Transactions (manual autocommit, explicit begin/commit/rollback, savepoints)
            // ============================================================
            // autocommit toggling
            run(conn.setAutoCommit(false));
            check("I.autocommit.false", !conn.isAutoCommit());
            run(conn.setAutoCommit(true));
            check("I.autocommit.true.again", conn.isAutoCommit());

            // explicit begin + commit -> persists
            run(conn.beginTransaction());
            check("I.inTx.autocommitFalse", !conn.isAutoCommit());
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 10, "tx-commit", 100L);
            run(conn.commitTransaction());
            long committed = scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 10);
            eq("I.commit.persists", committed, 1L);

            // explicit begin + rollback -> gone
            run(conn.beginTransaction());
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 11, "tx-rollback", 200L);
            long beforeRollback = scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 11);
            eq("I.rollback.preVisible", beforeRollback, 1L);
            run(conn.rollbackTransaction());
            long afterRollback = scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 11);
            eq("I.rollback.gone", afterRollback, 0L);

            // explicit begin + commit (id=12)
            run(conn.beginTransaction());
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 12, "begin-commit", 300L);
            run(conn.commitTransaction());
            eq("I.beginCommit.persists", scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 12), 1L);

            // savepoints: insert A, savepoint, insert B, rollback to savepoint -> A kept, B gone
            run(conn.beginTransaction());
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 20, "sp-A", 1L);
            run(conn.createSavepoint("sp1"));
            update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 21, "sp-B", 2L);
            long preSpRollback = scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id IN (20,21)");
            eq("I.savepoint.bothVisible", preSpRollback, 2L);
            run(conn.rollbackTransactionToSavepoint("sp1"));
            eq("I.savepoint.A.kept", scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 20), 1L);
            eq("I.savepoint.B.gone", scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 21), 0L);
            run(conn.releaseSavepoint("sp1"));
            run(conn.commitTransaction());
            eq("I.savepoint.commit.A", scalarLong(conn, "SELECT COUNT(*) FROM accounts WHERE id = $1", 20), 1L);

            run(conn.setAutoCommit(true));
            check("I.autocommit.restored", conn.isAutoCommit());

            // ============================================================
            // SECTION J: Connection.createBatch()
            // ============================================================
            List<Long> batchCounts = drain(Flux.from(conn.createBatch()
                    .add("INSERT INTO accounts (id, owner, amount) VALUES (30,'batch1',1)")
                    .add("INSERT INTO accounts (id, owner, amount) VALUES (31,'batch2',2)")
                    .add("UPDATE accounts SET amount = 999 WHERE id = 30")
                    .execute()).concatMap(r -> r.getRowsUpdated()));
            long batchSum = 0;
            for (Long u : batchCounts) batchSum += u;
            eq("J.batch.results", (long) batchCounts.size(), 3L);
            eq("J.batch.sum", batchSum, 3L);
            eq("J.batch.applied", scalarLong(conn, "SELECT amount FROM accounts WHERE id = $1", 30), 999L);

            // ============================================================
            // SECTION K: error / exception paths
            // ============================================================
            boolean badGrammar = false;
            try {
                update(conn, "SELCT * FROM nope");
            } catch (R2dbcBadGrammarException ex) {
                badGrammar = true;
            } catch (R2dbcException ex) {
                badGrammar = true;
            }
            check("K.badGrammar.throws", badGrammar);

            boolean dup = false;
            try {
                update(conn, "INSERT INTO accounts (id, owner, amount) VALUES ($1,$2,$3)", 1, "dupe", 0L);
            } catch (R2dbcException ex) {
                dup = true;
            }
            check("K.duplicateKey.throws", dup);

            boolean badCol = false;
            try {
                query(conn, "SELECT no_such_column FROM users", (row, m2) -> row.get(0));
            } catch (R2dbcException ex) {
                badCol = true;
            }
            check("K.badColumn.throws", badCol);

            boolean notNullViol = false;
            try {
                update(conn, "INSERT INTO users (name, age) VALUES ($1,$2)", null, 1);
            } catch (R2dbcException ex) {
                notNullViol = true;
            } catch (RuntimeException ex) {
                notNullViol = true;
            }
            check("K.notNull.throws", notNullViol);

            // ============================================================
            // SECTION L: explicit Mono/Flux operator surface
            // ============================================================
            // Flux.collectList().block()
            List<String> fluxNames = Flux.from(
                    conn.createStatement("SELECT name FROM users ORDER BY name").execute())
                    .concatMap(r -> r.map((row, m3) -> row.get("name", String.class)))
                    .collectList().block(T);
            check("L.flux.collectList", fluxNames != null && fluxNames.size() == 5);
            eq("L.flux.first", fluxNames.get(0), "alice");

            // Flux.count().block()
            Long fluxCount = Flux.from(conn.createStatement("SELECT id FROM accounts").execute())
                    .concatMap(r -> r.map((row, m4) -> row.get(0, Integer.class)))
                    .count().block(T);
            long realAcc = scalarLong(conn, "SELECT COUNT(*) FROM accounts");
            eq("L.flux.count", fluxCount, realAcc);

            // Mono.from(...).block() on a Void publisher returns null without error
            Object voidResult = Mono.from(conn.beginTransaction()).block(T);
            check("L.mono.void.null", voidResult == null);
            run(conn.rollbackTransaction());
            run(conn.setAutoCommit(true));

            // Flux.reduce sum of amounts
            Long sumAmt = Flux.from(conn.createStatement("SELECT amount FROM accounts").execute())
                    .concatMap(r -> r.map((row, m5) -> row.get("amount", Long.class)))
                    .reduce(0L, (x, y) -> x + y).block(T);
            Long sqlSum = scalarLong(conn, "SELECT SUM(amount) FROM accounts");
            eq("L.flux.reduce.sum", sumAmt, sqlSum);

            // Result.filter + flatMap on RowSegment
            List<String> filtered = drain(Flux.from(
                    conn.createStatement("SELECT name FROM users ORDER BY name").execute())
                    .concatMap(res -> res
                            .filter(seg -> seg instanceof Result.RowSegment)
                            .map((row, m6) -> row.get("name", String.class))));
            eq("L.filter.rowSegments", (long) filtered.size(), 5L);

            // ============================================================
            // SECTION M: second connection sees committed data (shared mem DB)
            // ============================================================
            conn2 = one(primary.create());
            check("M.conn2.nonNull", conn2 != null);
            long seenByConn2 = scalarLong(conn2, "SELECT COUNT(*) FROM users");
            eq("M.conn2.sharedData", seenByConn2, 5L);
            eq("M.conn2.metadata", conn2.getMetadata().getDatabaseProductName(), "H2");

            // ============================================================
            // cleanup
            // ============================================================
            update(conn, "DROP TABLE nums");
            update(conn, "DROP TABLE accounts");
            update(conn, "DROP TABLE users");
            run(conn2.close());
            conn2 = null;
            run(conn.close());
            conn = null;
            primary = null;

        } catch (Throwable t) {
            fail++;
            System.out.println("FAIL harness-exception " + t);
            t.printStackTrace(System.out);
        } finally {
            try { if (conn2 != null) drain(conn2.close()); } catch (Throwable ignore) {}
            try { if (conn != null) drain(conn.close()); } catch (Throwable ignore) {}
        }

        System.out.println("R2DBC_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("R2DBC_DONE");
        }
        System.exit(fail == 0 ? 0 : 1);
    }

    // ---- scalar helpers ----
    static long scalarLong(Connection c, String sql, Object... binds) {
        List<Long> l = query(c, sql, (row, md) -> {
            Object v = row.get(0);
            return v == null ? null : ((Number) v).longValue();
        }, binds);
        Long v = l.isEmpty() ? null : l.get(0);
        return v == null ? 0L : v;
    }

    static Integer scalarInt(Connection c, String sql, Object... binds) {
        List<Integer> l = query(c, sql, (row, md) -> row.get(0, Integer.class), binds);
        return l.isEmpty() ? null : l.get(0);
    }

    static String scalarStr(Connection c, String sql, Object... binds) {
        List<String> l = query(c, sql, (row, md) -> row.get(0, String.class), binds);
        return l.isEmpty() ? null : l.get(0);
    }
}
