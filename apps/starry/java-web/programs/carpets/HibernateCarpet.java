package org.starry.dod;

import jakarta.persistence.CascadeType;
import jakarta.persistence.Column;
import jakarta.persistence.Entity;
import jakarta.persistence.EntityManager;
import jakarta.persistence.EntityManagerFactory;
import jakarta.persistence.EntityTransaction;
import jakarta.persistence.EnumType;
import jakarta.persistence.Enumerated;
import jakarta.persistence.FetchType;
import jakarta.persistence.GeneratedValue;
import jakarta.persistence.GenerationType;
import jakarta.persistence.Id;
import jakarta.persistence.JoinColumn;
import jakarta.persistence.ManyToOne;
import jakarta.persistence.NamedQuery;
import jakarta.persistence.NoResultException;
import jakarta.persistence.NonUniqueResultException;
import jakarta.persistence.OneToMany;
import jakarta.persistence.Table;
import jakarta.persistence.Transient;
import jakarta.persistence.TypedQuery;
import jakarta.persistence.Version;
import jakarta.persistence.criteria.CriteriaBuilder;
import jakarta.persistence.criteria.CriteriaQuery;
import jakarta.persistence.criteria.Join;
import jakarta.persistence.criteria.ParameterExpression;
import jakarta.persistence.criteria.Predicate;
import jakarta.persistence.criteria.Root;

import org.hibernate.Hibernate;
import org.hibernate.Session;
import org.hibernate.SessionFactory;
import org.hibernate.Transaction;
import org.hibernate.cfg.Configuration;

import java.sql.Connection;
import java.sql.DriverManager;
import java.util.ArrayList;
import java.util.List;
import java.util.Objects;

/**
 * Industrial / carpet-grade Hibernate ORM + Jakarta Persistence coverage suite.
 *
 * Framework: Hibernate ORM 6.x (Jakarta Persistence 3.1) over an in-memory
 * SQLite database (org.xerial sqlite-jdbc 3.46.x, shared-cache memory mode),
 * community SQLiteDialect. Single process, no external network, /tmp-free
 * (pure in-memory). Self-counting; prints HIBERNATE_DONE only when fail==0.
 */
public class HibernateCarpet {

    /* shared-cache in-memory db kept alive by a held keepalive connection */
    static final String URL = "jdbc:sqlite:file:starrycarpetdb?mode=memory&cache=shared";

    static int ok = 0;
    static int fail = 0;

    static void ck(boolean cond, String name) {
        if (cond) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name);
        }
    }

    static void eq(Object expected, Object actual, String name) {
        if (Objects.equals(expected, actual)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + expected + "] actual=[" + actual + "]");
        }
    }

    static void group(String name, Runnable r) {
        try {
            r.run();
        } catch (Throwable e) {
            fail++;
            System.out.println("FAIL " + name + " threw " + e.getClass().getSimpleName() + ": " + e.getMessage());
        }
    }

    static long asLong(Object o) {
        return ((Number) o).longValue();
    }

    static double asDouble(Object o) {
        return ((Number) o).doubleValue();
    }

    /* ===================== entities ===================== */

    public enum Grade {
        JUNIOR, SENIOR, LEAD
    }

    @Entity(name = "Department")
    @Table(name = "departments")
    public static class Department {
        @Id
        @GeneratedValue(strategy = GenerationType.IDENTITY)
        @Column(name = "id")
        Long id;

        @Column(name = "dept_name", nullable = false, length = 64, unique = true)
        String name;

        @Column(name = "location", length = 128)
        String location;

        @OneToMany(mappedBy = "department", cascade = CascadeType.ALL, orphanRemoval = true, fetch = FetchType.LAZY)
        List<Employee> employees = new ArrayList<>();

        public Department() {
        }

        public Department(String name, String location) {
            this.name = name;
            this.location = location;
        }

        void add(Employee e) {
            e.department = this;
            this.employees.add(e);
        }

        void remove(Employee e) {
            this.employees.remove(e);
            e.department = null;
        }
    }

    @Entity(name = "Employee")
    @Table(name = "employees")
    @NamedQuery(name = "Employee.byMinSalary",
            query = "select e from Employee e where e.salary >= :min order by e.salary desc, e.id asc")
    @NamedQuery(name = "Employee.countAll",
            query = "select count(e) from Employee e")
    public static class Employee {
        @Id
        @GeneratedValue(strategy = GenerationType.IDENTITY)
        @Column(name = "id")
        Long id;

        @Column(name = "emp_name", nullable = false, length = 64)
        String name;

        @Column(name = "salary", nullable = false)
        int salary;

        @Enumerated(EnumType.STRING)
        @Column(name = "grade", length = 16)
        Grade grade;

        @Version
        @Column(name = "ver", nullable = false)
        long version;

        @ManyToOne(fetch = FetchType.LAZY)
        @JoinColumn(name = "dept_id")
        Department department;

        @Transient
        String scratch;

        public Employee() {
        }

        public Employee(String name, int salary, Grade grade) {
            this.name = name;
            this.salary = salary;
            this.grade = grade;
        }
    }

    /* ===================== helpers ===================== */

    static SessionFactory buildSessionFactory() {
        Configuration cfg = new Configuration();
        cfg.setProperty("hibernate.connection.driver_class", "org.sqlite.JDBC");
        cfg.setProperty("hibernate.connection.url", URL);
        cfg.setProperty("hibernate.dialect", "org.hibernate.community.dialect.SQLiteDialect");
        cfg.setProperty("hibernate.hbm2ddl.auto", "create");
        cfg.setProperty("hibernate.connection.pool_size", "1");
        cfg.setProperty("hibernate.connection.autocommit", "false");
        cfg.setProperty("hibernate.show_sql", "false");
        cfg.setProperty("hibernate.format_sql", "false");
        cfg.setProperty("hibernate.max_fetch_depth", "3");
        cfg.addAnnotatedClass(Department.class);
        cfg.addAnnotatedClass(Employee.class);
        return cfg.buildSessionFactory();
    }

    /** Wipe and reinsert a fixed, deterministic dataset (3 depts, 5 employees). */
    static void seed(SessionFactory sf) {
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            s.createMutationQuery("delete from Employee").executeUpdate();
            s.createMutationQuery("delete from Department").executeUpdate();

            Department eng = new Department("Engineering", "Building A");
            Department sales = new Department("Sales", "Building B");
            Department research = new Department("Research", null);

            eng.add(new Employee("Alice", 7000, Grade.SENIOR));
            eng.add(new Employee("Bob", 5000, Grade.JUNIOR));
            eng.add(new Employee("Carol", 9000, Grade.LEAD));
            sales.add(new Employee("Dave", 6000, Grade.SENIOR));
            sales.add(new Employee("Eve", 4000, Grade.JUNIOR));

            s.persist(eng);
            s.persist(sales);
            s.persist(research);
            t.commit();
        }
    }

    static Department deptByName(Session s, String n) {
        return s.createQuery("from Department where name = :n", Department.class)
                .setParameter("n", n).getSingleResult();
    }

    static Employee empByName(Session s, String n) {
        return s.createQuery("from Employee where name = :n", Employee.class)
                .setParameter("n", n).getSingleResult();
    }

    /* ===================== test groups ===================== */

    // G1: bootstrap, SessionFactory / EntityManagerFactory, metamodel
    static void g1(SessionFactory sf) {
        ck(sf != null, "g1.sf.notNull");
        ck(sf.isOpen(), "g1.sf.open");
        ck(sf instanceof EntityManagerFactory, "g1.sf.isEMF");
        EntityManagerFactory emf = sf;
        ck(emf.getMetamodel().getEntities().size() >= 2, "g1.metamodel.entities>=2");
        eq("Employee", emf.getMetamodel().entity(Employee.class).getName(), "g1.metamodel.Employee.name");
        ck(emf.getMetamodel().entity(Department.class) != null, "g1.metamodel.Department");
        ck(sf.getMetamodel() != null, "g1.sf.metamodel");
        ck(emf.isOpen(), "g1.emf.open");
    }

    // G2: seed sanity, aggregates baseline
    static void g2(SessionFactory sf) {
        seed(sf);
        try (Session s = sf.openSession()) {
            eq(3L, asLong(s.createQuery("select count(d) from Department d").getSingleResult()), "g2.deptCount");
            eq(5L, asLong(s.createQuery("select count(e) from Employee e").getSingleResult()), "g2.empCount");
            eq(31000L, asLong(s.createQuery("select sum(e.salary) from Employee e").getSingleResult()), "g2.sum");
            eq(6200.0, asDouble(s.createQuery("select avg(e.salary) from Employee e").getSingleResult()), "g2.avg");
            eq(4000L, asLong(s.createQuery("select min(e.salary) from Employee e").getSingleResult()), "g2.min");
            eq(9000L, asLong(s.createQuery("select max(e.salary) from Employee e").getSingleResult()), "g2.max");
        }
    }

    // G3: basic CRUD via native Session (persist/find/get/byId/update/remove)
    static void g3(SessionFactory sf) {
        seed(sf);
        Long newId;
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department d = new Department("QA", "Building C");
            ck(d.id == null, "g3.prePersist.idNull");
            s.persist(d);
            s.flush();
            newId = d.id;
            ck(newId != null, "g3.generatedId.notNull");
            ck(newId > 0, "g3.generatedId.positive");
            t.commit();
        }
        try (Session s = sf.openSession()) {
            Department byGet = s.get(Department.class, newId);
            eq("QA", byGet.name, "g3.get.name");
            Department byId = s.byId(Department.class).load(newId);
            eq("Building C", byId.location, "g3.byId.location");
            eq(byGet, byId, "g3.get==byId.firstLevelCache");
        }
        // update in a tx
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department d = s.get(Department.class, newId);
            d.location = "Building Z";
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq("Building Z", s.get(Department.class, newId).location, "g3.update.persisted");
        }
        // remove
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department d = s.get(Department.class, newId);
            s.remove(d);
            t.commit();
        }
        try (Session s = sf.openSession()) {
            ck(s.get(Department.class, newId) == null, "g3.remove.gone");
            eq(3L, asLong(s.createQuery("select count(d) from Department d").getSingleResult()), "g3.count.backToBaseline");
        }
        // persist employee referencing existing department, navigate
        Long empId;
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department eng = deptByName(s, "Engineering");
            Employee e = new Employee("Frank", 8000, Grade.SENIOR);
            e.department = eng;
            s.persist(e);
            s.flush();
            empId = e.id;
            t.commit();
        }
        try (Session s = sf.openSession()) {
            Employee e = s.get(Employee.class, empId);
            eq("Frank", e.name, "g3.emp.name");
            eq(8000, e.salary, "g3.emp.salary");
            // @ManyToOne(LAZY): association is an uninitialized proxy until touched
            ck(!Hibernate.isInitialized(e.department), "g3.emp.dept.lazyNotInit");
            Department dept = (Department) Hibernate.unproxy(e.department);
            eq("Engineering", dept.name, "g3.emp.deptNav");
            ck(Hibernate.isInitialized(e.department), "g3.emp.dept.initAfterAccess");
        }
    }

    // G4: merge / detached entities
    static void g4(SessionFactory sf) {
        seed(sf);
        Long id;
        try (Session s = sf.openSession()) {
            id = empByName(s, "Alice").id;
        }
        // load, detach, modify, merge
        Employee detached;
        try (Session s = sf.openSession()) {
            detached = s.get(Employee.class, id);
        } // session closed -> detached
        detached.salary = 7777;
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            ck(!s.contains(detached), "g4.detached.notContained");
            Employee managed = (Employee) s.merge(detached);
            ck(s.contains(managed), "g4.merged.contained");
            ck(managed != detached, "g4.merge.returnsCopy");
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq(7777, s.get(Employee.class, id).salary, "g4.merge.persisted");
        }
        // merge a brand-new transient (id null) -> insert
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Employee fresh = new Employee("Grace", 5500, Grade.JUNIOR);
            Employee managed = (Employee) s.merge(fresh);
            ck(managed.id != null, "g4.merge.transient.assignsId");
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq(1L, asLong(s.createQuery("select count(e) from Employee e where e.name = 'Grace'")
                    .getSingleResult()), "g4.merge.transient.inserted");
        }
    }

    // G5: flush / clear / refresh / evict / contains / isDirty
    static void g5(SessionFactory sf) {
        seed(sf);
        Long id;
        try (Session s = sf.openSession()) {
            id = empByName(s, "Bob").id;
        }
        // flush makes row visible to same-tx query before commit
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Employee e = new Employee("Heidi", 4200, Grade.JUNIOR);
            s.persist(e);
            s.flush();
            long cnt = asLong(s.createQuery("select count(x) from Employee x where x.name = 'Heidi'")
                    .getSingleResult());
            eq(1L, cnt, "g5.flush.visibleInTx");
            t.rollback(); // discard
        }
        try (Session s = sf.openSession()) {
            eq(0L, asLong(s.createQuery("select count(x) from Employee x where x.name = 'Heidi'")
                    .getSingleResult()), "g5.rollbackAfterFlush.gone");
        }
        // clear() detaches everything
        try (Session s = sf.openSession()) {
            Employee e = s.get(Employee.class, id);
            ck(s.contains(e), "g5.beforeClear.contained");
            s.clear();
            ck(!s.contains(e), "g5.afterClear.detached");
        }
        // refresh reverts in-memory change
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Employee e = s.get(Employee.class, id);
            int original = e.salary;
            e.salary = 1;
            ck(s.isDirty(), "g5.isDirty.afterChange");
            s.refresh(e);
            eq(original, e.salary, "g5.refresh.reverted");
            ck(!s.isDirty(), "g5.notDirty.afterRefresh");
            t.commit();
        }
        // evict single entity
        try (Session s = sf.openSession()) {
            Employee a = empByName(s, "Alice");
            Employee c = empByName(s, "Carol");
            s.evict(a);
            ck(!s.contains(a), "g5.evict.target.detached");
            ck(s.contains(c), "g5.evict.other.stillManaged");
        }
    }

    // G6: first-level (persistence-context) cache + getReference
    static void g6(SessionFactory sf) {
        seed(sf);
        Long id;
        try (Session s = sf.openSession()) {
            id = empByName(s, "Carol").id;
        }
        try (Session s = sf.openSession()) {
            Employee a = s.get(Employee.class, id);
            Employee b = s.get(Employee.class, id);
            ck(a == b, "g6.sameSession.sameIdentity");
            s.clear();
            Employee c = s.get(Employee.class, id);
            ck(c != a, "g6.afterClear.newIdentity");
            eq(a.name, c.name, "g6.afterClear.sameData");
        }
        try (Session s = sf.openSession()) {
            Employee ref = s.getReference(Employee.class, id);
            ck(ref != null, "g6.getReference.notNull");
            ck(s.contains(ref), "g6.getReference.contained");
            // getReference yields a proxy not hydrated from the DB yet
            ck(!Hibernate.isInitialized(ref), "g6.getReference.notInitYet");
            Employee real = (Employee) Hibernate.unproxy(ref);
            eq("Carol", real.name, "g6.getReference.loadsOnAccess");
            ck(Hibernate.isInitialized(ref), "g6.getReference.initAfterAccess");
        }
    }

    // G7: transaction commit / rollback / status + constraint rollback
    static void g7(SessionFactory sf) {
        seed(sf);
        long baseline;
        try (Session s = sf.openSession()) {
            baseline = asLong(s.createQuery("select count(d) from Department d").getSingleResult());
        }
        // status active after begin, inactive after commit
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            ck(t.isActive(), "g7.tx.activeAfterBegin");
            t.commit();
            ck(!t.isActive(), "g7.tx.inactiveAfterCommit");
        }
        // rollback discards an insert
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            s.persist(new Department("Temp", "void"));
            t.rollback();
            ck(!t.isActive(), "g7.tx.inactiveAfterRollback");
        }
        try (Session s = sf.openSession()) {
            eq(baseline, asLong(s.createQuery("select count(d) from Department d").getSingleResult()),
                    "g7.rollback.discarded");
        }
        // unique constraint violation -> exception -> rollback
        boolean threw = false;
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            try {
                s.persist(new Department("Engineering", "dup")); // dept_name is unique
                s.flush();
                t.commit();
            } catch (RuntimeException ex) {
                threw = true;
                if (t.isActive()) {
                    t.rollback();
                }
            }
        }
        ck(threw, "g7.uniqueConstraint.threw");
        // session machinery still usable afterward
        try (Session s = sf.openSession()) {
            eq(baseline, asLong(s.createQuery("select count(d) from Department d").getSingleResult()),
                    "g7.afterConstraint.baselineIntact");
        }
    }

    // G8: HQL / JPQL breadth
    static void g8(SessionFactory sf) {
        seed(sf);
        try (Session s = sf.openSession()) {
            // select all
            List<Employee> all = s.createQuery("from Employee", Employee.class).getResultList();
            eq(5, all.size(), "g8.selectAll.size");

            // named parameter on enum
            List<Employee> seniors = s.createQuery(
                    "select e from Employee e where e.grade = :g order by e.name", Employee.class)
                    .setParameter("g", Grade.SENIOR).getResultList();
            eq(2, seniors.size(), "g8.namedParam.enum.size");
            eq("Alice", seniors.get(0).name, "g8.namedParam.enum.first");

            // positional parameter
            long highCnt = asLong(s.createQuery(
                    "select count(e) from Employee e where e.salary >= ?1")
                    .setParameter(1, 6000).getSingleResult());
            eq(3L, highCnt, "g8.positionalParam.count");

            // order by desc
            Employee top = s.createQuery(
                    "from Employee e order by e.salary desc, e.id asc", Employee.class)
                    .setMaxResults(1).getSingleResult();
            eq("Carol", top.name, "g8.orderBy.top");

            // aggregates
            eq(5L, asLong(s.createQuery("select count(e) from Employee e", Long.class).getSingleResult()),
                    "g8.count.typed");
            eq(31000L, asLong(s.createQuery("select sum(e.salary) from Employee e").getSingleResult()),
                    "g8.sum");
            eq(6200.0, asDouble(s.createQuery("select avg(e.salary) from Employee e").getSingleResult()),
                    "g8.avg");

            // scalar projection
            List<String> names = s.createQuery("select e.name from Employee e order by e.name", String.class)
                    .getResultList();
            eq(5, names.size(), "g8.projection.size");
            ck(names.contains("Alice") && names.contains("Eve"), "g8.projection.contains");

            // join
            long engCnt = asLong(s.createQuery(
                    "select count(e) from Employee e join e.department d where d.name = :dn")
                    .setParameter("dn", "Engineering").getSingleResult());
            eq(3L, engCnt, "g8.join.count");

            // group by + having
            List<Object[]> grouped = s.createQuery(
                    "select d.name, count(e) from Employee e join e.department d "
                            + "group by d.name having count(e) > 2", Object[].class).getResultList();
            eq(1, grouped.size(), "g8.groupByHaving.size");
            eq("Engineering", grouped.get(0)[0], "g8.groupByHaving.dept");
            eq(3L, asLong(grouped.get(0)[1]), "g8.groupByHaving.count");

            // distinct
            long distinctGrades = s.createQuery(
                    "select count(distinct e.grade) from Employee e").getSingleResult() instanceof Number
                    ? asLong(s.createQuery("select count(distinct e.grade) from Employee e").getSingleResult())
                    : -1;
            eq(3L, distinctGrades, "g8.distinct.grades");

            // pagination
            List<Employee> page = s.createQuery(
                    "from Employee e order by e.salary desc, e.id asc", Employee.class)
                    .setFirstResult(1).setMaxResults(2).getResultList();
            eq(2, page.size(), "g8.pagination.size");
            eq("Alice", page.get(0).name, "g8.pagination.first");
            eq("Dave", page.get(1).name, "g8.pagination.second");

            // named queries
            List<Employee> nq = s.createNamedQuery("Employee.byMinSalary", Employee.class)
                    .setParameter("min", 6000).getResultList();
            eq(3, nq.size(), "g8.namedQuery.size");
            eq("Carol", nq.get(0).name, "g8.namedQuery.firstDesc");
            eq(5L, asLong(s.createNamedQuery("Employee.countAll").getSingleResult()),
                    "g8.namedQuery.count");

            // NoResultException
            boolean noResult = false;
            try {
                s.createQuery("from Employee e where e.salary > 999999", Employee.class).getSingleResult();
            } catch (NoResultException ex) {
                noResult = true;
            }
            ck(noResult, "g8.getSingleResult.noResult");

            // NonUniqueResultException
            boolean nonUnique = false;
            try {
                s.createQuery("from Employee", Employee.class).getSingleResult();
            } catch (NonUniqueResultException ex) {
                nonUnique = true;
            }
            ck(nonUnique, "g8.getSingleResult.nonUnique");
        }

        // DML update
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            int updated = s.createMutationQuery(
                    "update Employee e set e.salary = e.salary + 1000 where e.grade = :g")
                    .setParameter("g", Grade.JUNIOR).executeUpdate();
            eq(2, updated, "g8.dml.update.count");
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq(33000L, asLong(s.createQuery("select sum(e.salary) from Employee e").getSingleResult()),
                    "g8.dml.update.effect");
        }
        // DML delete
        seed(sf);
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            int deleted = s.createMutationQuery("delete from Employee e where e.salary < :s")
                    .setParameter("s", 6000).executeUpdate();
            eq(2, deleted, "g8.dml.delete.count"); // Bob 5000, Eve 4000
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq(3L, asLong(s.createQuery("select count(e) from Employee e").getSingleResult()),
                    "g8.dml.delete.remaining");
        }
    }

    // G9: Criteria API
    static void g9(SessionFactory sf) {
        seed(sf);
        try (Session s = sf.openSession()) {
            CriteriaBuilder cb = s.getCriteriaBuilder();

            // select all
            CriteriaQuery<Employee> all = cb.createQuery(Employee.class);
            Root<Employee> r1 = all.from(Employee.class);
            all.select(r1);
            eq(5, s.createQuery(all).getResultList().size(), "g9.selectAll");

            // equal predicate
            CriteriaQuery<Employee> q2 = cb.createQuery(Employee.class);
            Root<Employee> r2 = q2.from(Employee.class);
            q2.select(r2).where(cb.equal(r2.get("grade"), Grade.LEAD));
            List<Employee> leads = s.createQuery(q2).getResultList();
            eq(1, leads.size(), "g9.equal.size");
            eq("Carol", leads.get(0).name, "g9.equal.value");

            // gt predicate + order
            CriteriaQuery<Employee> q3 = cb.createQuery(Employee.class);
            Root<Employee> r3 = q3.from(Employee.class);
            q3.select(r3).where(cb.gt(r3.<Integer>get("salary"), 5000))
                    .orderBy(cb.desc(r3.get("salary")));
            List<Employee> high = s.createQuery(q3).getResultList();
            eq(3, high.size(), "g9.gt.size");
            eq("Carol", high.get(0).name, "g9.gt.orderDesc");

            // like predicate
            CriteriaQuery<Employee> q4 = cb.createQuery(Employee.class);
            Root<Employee> r4 = q4.from(Employee.class);
            q4.select(r4).where(cb.like(r4.<String>get("name"), "A%"));
            eq(1, s.createQuery(q4).getResultList().size(), "g9.like.size");

            // and predicate
            CriteriaQuery<Employee> q5 = cb.createQuery(Employee.class);
            Root<Employee> r5 = q5.from(Employee.class);
            Predicate p5 = cb.and(cb.gt(r5.<Integer>get("salary"), 4000),
                    cb.like(r5.<String>get("name"), "%e%"));
            q5.select(r5).where(p5);
            eq(2, s.createQuery(q5).getResultList().size(), "g9.and.size"); // Alice, Dave

            // or predicate
            CriteriaQuery<Employee> q6 = cb.createQuery(Employee.class);
            Root<Employee> r6 = q6.from(Employee.class);
            Predicate p6 = cb.or(cb.equal(r6.get("grade"), Grade.LEAD),
                    cb.equal(r6.get("grade"), Grade.JUNIOR));
            q6.select(r6).where(p6);
            eq(3, s.createQuery(q6).getResultList().size(), "g9.or.size"); // Carol + Bob + Eve

            // count via CriteriaBuilder.count
            CriteriaQuery<Long> q7 = cb.createQuery(Long.class);
            Root<Employee> r7 = q7.from(Employee.class);
            q7.select(cb.count(r7));
            eq(5L, s.createQuery(q7).getSingleResult(), "g9.count");

            // parameter expression
            CriteriaQuery<Employee> q8 = cb.createQuery(Employee.class);
            Root<Employee> r8 = q8.from(Employee.class);
            ParameterExpression<Integer> minSal = cb.parameter(Integer.class, "minSal");
            q8.select(r8).where(cb.ge(r8.<Integer>get("salary"), minSal));
            eq(3, s.createQuery(q8).setParameter("minSal", 6000).getResultList().size(),
                    "g9.parameterExpression");

            // multiselect (projection of name + salary)
            CriteriaQuery<Object[]> q9 = cb.createQuery(Object[].class);
            Root<Employee> r9 = q9.from(Employee.class);
            q9.multiselect(r9.<String>get("name"), r9.<Integer>get("salary"));
            List<Object[]> rows = s.createQuery(q9).getResultList();
            eq(5, rows.size(), "g9.multiselect.size");
            ck(rows.get(0).length == 2, "g9.multiselect.tupleWidth");

            // criteria join
            CriteriaQuery<Employee> q10 = cb.createQuery(Employee.class);
            Root<Employee> r10 = q10.from(Employee.class);
            Join<Employee, Department> dj = r10.join("department");
            q10.select(r10).where(cb.equal(dj.get("name"), "Engineering"));
            eq(3, s.createQuery(q10).getResultList().size(), "g9.join.size");

            // aggregate max via CriteriaBuilder
            CriteriaQuery<Integer> q11 = cb.createQuery(Integer.class);
            Root<Employee> r11 = q11.from(Employee.class);
            q11.select(cb.max(r11.<Integer>get("salary")));
            eq(9000L, asLong(s.createQuery(q11).getSingleResult()), "g9.max");
        }
    }

    // G10: relationships, cascade, orphanRemoval, navigation
    static void g10(SessionFactory sf) {
        seed(sf);
        Long deptId;
        // cascade persist children
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department ops = new Department("Operations", "Building D");
            ops.add(new Employee("Ivan", 6100, Grade.SENIOR));
            ops.add(new Employee("Judy", 5900, Grade.JUNIOR));
            s.persist(ops); // cascade ALL persists employees
            s.flush();
            deptId = ops.id;
            ck(ops.employees.get(0).id != null, "g10.cascade.child0.id");
            ck(ops.employees.get(1).id != null, "g10.cascade.child1.id");
            t.commit();
        }
        // reload, verify collection + back-reference
        try (Session s = sf.openSession()) {
            Department ops = s.get(Department.class, deptId);
            eq(2, ops.employees.size(), "g10.collection.size");
            Employee any = ops.employees.get(0);
            eq("Operations", any.department.name, "g10.backRef");
        }
        // orphanRemoval: remove a child from the collection -> deleted
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department ops = s.get(Department.class, deptId);
            Employee victim = ops.employees.get(0);
            ops.remove(victim);
            t.commit();
        }
        try (Session s = sf.openSession()) {
            Department ops = s.get(Department.class, deptId);
            eq(1, ops.employees.size(), "g10.orphanRemoval.collection");
            eq(1L, asLong(s.createQuery("select count(e) from Employee e where e.department.id = :d")
                    .setParameter("d", deptId).getSingleResult()), "g10.orphanRemoval.dbCount");
        }
        // cascade remove: delete department deletes remaining children
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Department ops = s.get(Department.class, deptId);
            s.remove(ops);
            t.commit();
        }
        try (Session s = sf.openSession()) {
            ck(s.get(Department.class, deptId) == null, "g10.cascadeRemove.deptGone");
            eq(0L, asLong(s.createQuery("select count(e) from Employee e where e.department.id = :d")
                    .setParameter("d", deptId).getSingleResult()), "g10.cascadeRemove.childrenGone");
            eq(5L, asLong(s.createQuery("select count(e) from Employee e").getSingleResult()),
                    "g10.cascadeRemove.baselineIntact");
        }
    }

    // G11: native SQL queries
    static void g11(SessionFactory sf) {
        seed(sf);
        try (Session s = sf.openSession()) {
            long cnt = asLong(s.createNativeQuery("select count(*) from employees", Long.class)
                    .getSingleResult());
            eq(5L, cnt, "g11.native.count");

            long engCnt = asLong(s.createNativeQuery(
                    "select count(*) from employees e join departments d on e.dept_id = d.id "
                            + "where d.dept_name = :dn", Long.class)
                    .setParameter("dn", "Engineering").getSingleResult());
            eq(3L, engCnt, "g11.native.join.param");

            @SuppressWarnings("unchecked")
            List<Object> names = s.createNativeQuery(
                    "select emp_name from employees order by salary desc")
                    .getResultList();
            eq(5, names.size(), "g11.native.scalarList.size");
            eq("Carol", String.valueOf(names.get(0)), "g11.native.scalarList.first");

            // native query mapped to entity
            Employee alice = (Employee) s.createNativeQuery(
                    "select * from employees where emp_name = 'Alice'", Employee.class)
                    .getSingleResult();
            eq(7000, alice.salary, "g11.native.entityMapping");
        }
    }

    // G12: Jakarta Persistence EntityManager API path
    static void g12(SessionFactory sf) {
        seed(sf);
        EntityManagerFactory emf = sf;
        EntityManager em = emf.createEntityManager();
        try {
            ck(em.isOpen(), "g12.em.open");
            ck(em.getCriteriaBuilder() != null, "g12.em.criteriaBuilder");
            ck(em.getMetamodel() != null, "g12.em.metamodel");

            // persist via EntityTransaction
            EntityTransaction tx = em.getTransaction();
            tx.begin();
            ck(tx.isActive(), "g12.tx.active");
            Department d = new Department("Legal", "Building E");
            em.persist(d);
            em.flush();
            ck(d.id != null, "g12.em.persist.generatedId");
            ck(em.contains(d), "g12.em.contains");
            tx.commit();
            ck(!tx.isActive(), "g12.tx.committed");

            Long id = d.id;
            // find
            Department found = em.find(Department.class, id);
            eq("Legal", found.name, "g12.em.find");
            // getReference
            Department ref = em.getReference(Department.class, id);
            ck(ref != null, "g12.em.getReference");
            // TypedQuery
            TypedQuery<Employee> tq = em.createQuery("from Employee e order by e.salary desc", Employee.class);
            eq(5, tq.getResultList().size(), "g12.em.typedQuery.size");
            eq("Carol", tq.setMaxResults(1).getResultList().get(0).name, "g12.em.typedQuery.top");

            // merge + refresh
            tx = em.getTransaction();
            tx.begin();
            found.location = "Building E2";
            em.flush();
            found.location = "scratch-not-flushed";
            em.refresh(found);
            eq("Building E2", found.location, "g12.em.refresh");
            tx.commit();

            // remove
            tx = em.getTransaction();
            tx.begin();
            Department toDel = em.find(Department.class, id);
            em.remove(toDel);
            tx.commit();
            ck(em.find(Department.class, id) == null, "g12.em.remove");

            // clear -> detach
            Employee e = em.createQuery("from Employee e where e.name = 'Alice'", Employee.class)
                    .getSingleResult();
            ck(em.contains(e), "g12.em.beforeClear.contained");
            em.clear();
            ck(!em.contains(e), "g12.em.afterClear.detached");

            // EntityTransaction rollback
            tx = em.getTransaction();
            tx.begin();
            em.persist(new Department("Rollback", "void"));
            tx.rollback();
            ck(!tx.isActive(), "g12.tx.rolledBack");
            eq(0L, asLong(em.createQuery("select count(d) from Department d where d.name = 'Rollback'")
                    .getSingleResult()), "g12.em.rollback.discarded");

            // DML via EntityManager
            tx = em.getTransaction();
            tx.begin();
            int up = em.createQuery("update Employee e set e.salary = e.salary + 100 where e.grade = :g")
                    .setParameter("g", Grade.LEAD).executeUpdate();
            eq(1, up, "g12.em.dml.update");
            tx.commit();
        } finally {
            em.close();
            ck(!em.isOpen(), "g12.em.closed");
        }
    }

    // G13: @Version optimistic locking
    static void g13(SessionFactory sf) {
        seed(sf);
        Long id;
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Employee e = new Employee("Mallory", 5300, Grade.JUNIOR);
            s.persist(e);
            s.flush();
            id = e.id;
            eq(0L, e.version, "g13.version.initialZero");
            t.commit();
        }
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Employee e = s.get(Employee.class, id);
            e.salary = 5400;
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq(1L, s.get(Employee.class, id).version, "g13.version.incrementedTo1");
        }
        try (Session s = sf.openSession()) {
            Transaction t = s.beginTransaction();
            Employee e = s.get(Employee.class, id);
            e.salary = 5500;
            t.commit();
        }
        try (Session s = sf.openSession()) {
            eq(2L, s.get(Employee.class, id).version, "g13.version.incrementedTo2");
        }
    }

    /* ===================== main ===================== */

    public static void main(String[] args) {
        // keep framework logging quiet so the result markers stay clean
        System.setProperty("org.slf4j.simpleLogger.defaultLogLevel", "error");
        System.setProperty("org.slf4j.simpleLogger.log.org.hibernate", "error");
        System.setProperty("org.jboss.logging.provider", "slf4j");

        SessionFactory sf = null;
        Connection keepAlive = null;
        try {
            Class.forName("org.sqlite.JDBC");
            // hold the shared-cache in-memory database alive for the whole run
            keepAlive = DriverManager.getConnection(URL);

            sf = buildSessionFactory();
            final SessionFactory f = sf;

            group("G1.bootstrap", () -> g1(f));
            group("G2.seedSanity", () -> g2(f));
            group("G3.crud", () -> g3(f));
            group("G4.merge", () -> g4(f));
            group("G5.flushClearRefreshEvict", () -> g5(f));
            group("G6.firstLevelCache", () -> g6(f));
            group("G7.transaction", () -> g7(f));
            group("G8.hql", () -> g8(f));
            group("G9.criteria", () -> g9(f));
            group("G10.relationships", () -> g10(f));
            group("G11.nativeSql", () -> g11(f));
            group("G12.entityManager", () -> g12(f));
            group("G13.version", () -> g13(f));
        } catch (Throwable e) {
            fail++;
            System.out.println("FAIL fatal " + e.getClass().getName() + ": " + e.getMessage());
            e.printStackTrace();
        } finally {
            try {
                if (sf != null) {
                    sf.close();
                }
            } catch (Throwable ignored) {
            }
            try {
                if (keepAlive != null) {
                    keepAlive.close();
                }
            } catch (Throwable ignored) {
            }
        }

        System.out.println("HIBERNATE_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("HIBERNATE_DONE");
        }
    }
}
