package org.starry.dod;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicReference;

import lombok.AccessLevel;
import lombok.AllArgsConstructor;
import lombok.Builder;
import lombok.Cleanup;
import lombok.Data;
import lombok.EqualsAndHashCode;
import lombok.Getter;
import lombok.NoArgsConstructor;
import lombok.NonNull;
import lombok.RequiredArgsConstructor;
import lombok.Setter;
import lombok.Singular;
import lombok.SneakyThrows;
import lombok.Synchronized;
import lombok.ToString;
import lombok.Value;
import lombok.With;
import lombok.experimental.Accessors;
import lombok.experimental.FieldDefaults;
import lombok.experimental.SuperBuilder;
import lombok.experimental.UtilityClass;
import lombok.extern.java.Log;

/**
 * Carpet-grade coverage for Project Lombok 1.18.34 generated code.
 * Every annotation under test gets a dedicated subject type; assertions exercise
 * the generated methods / constructors / immutability / null-guards at runtime
 * via direct calls and reflection, with exact-equality checks.
 */
public class LombokCarpet {

    // ----- self-counting assertion harness -----
    static int ok = 0;
    static int fail = 0;

    static void check(String name, boolean cond) {
        if (cond) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name);
        }
    }

    static void eq(String name, Object exp, Object act) {
        boolean c = (exp == null) ? (act == null) : exp.equals(act);
        if (c) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + exp + "] actual=[" + act + "]");
        }
    }

    static void eqi(String name, long exp, long act) {
        if (exp == act) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + exp + "] actual=[" + act + "]");
        }
    }

    interface ThrowingRunnable {
        void run() throws Throwable;
    }

    static void expect(String name, Class<? extends Throwable> type, String wantMsg, ThrowingRunnable r) {
        try {
            r.run();
            fail++;
            System.out.println("FAIL " + name + " (no exception thrown)");
        } catch (Throwable t) {
            if (!type.isInstance(t)) {
                fail++;
                System.out.println("FAIL " + name + " wrong-type=[" + t.getClass().getName() + "]");
            } else if (wantMsg != null && !wantMsg.equals(t.getMessage())) {
                fail++;
                System.out.println("FAIL " + name + " msg-expected=[" + wantMsg + "] actual=[" + t.getMessage() + "]");
            } else {
                ok++;
            }
        }
    }

    static boolean hasMethod(Class<?> c, String name, Class<?>... params) {
        try {
            c.getDeclaredMethod(name, params);
            return true;
        } catch (NoSuchMethodException e) {
            return false;
        }
    }

    // ===================================================================
    // Custom runtime annotation for onMethod_ propagation test
    // ===================================================================
    @Retention(RetentionPolicy.RUNTIME)
    @Target(ElementType.METHOD)
    @interface Marker {
    }

    // ===================================================================
    // 1. @Getter / @Setter : class-level, AccessLevel.NONE, onMethod_
    // ===================================================================
    @Getter
    @Setter
    static class Account {
        private long balance;
        private String owner;
        @Setter(AccessLevel.NONE)
        @Getter(onMethod_ = { @Deprecated })
        private String id;
        @Setter(onMethod_ = { @Marker })
        private boolean active;

        Account(String id) {
            this.id = id;
        }
    }

    // ===================================================================
    // 2. @ToString : default, of, exclude, callSuper
    // ===================================================================
    @ToString
    static class Point {
        int x = 3;
        int y = 4;
    }

    @ToString(of = { "x" })
    static class PointOf {
        int x = 3;
        int y = 4;
    }

    @ToString(exclude = "y")
    static class PointEx {
        int x = 3;
        int y = 4;
    }

    static class Sup {
        @Override
        public String toString() {
            return "SUP";
        }
    }

    @ToString(callSuper = true)
    static class Sub1 extends Sup {
        int v = 9;
    }

    // ===================================================================
    // 3. @EqualsAndHashCode : default, exclude, callSuper
    // ===================================================================
    @EqualsAndHashCode
    @AllArgsConstructor
    static class Pair {
        int a;
        int b;
    }

    @EqualsAndHashCode(exclude = "b")
    @AllArgsConstructor
    static class PairEx {
        int a;
        int b;
    }

    @EqualsAndHashCode
    @AllArgsConstructor
    static class ParentEq {
        int p;
    }

    @EqualsAndHashCode(callSuper = true)
    static class ChildEq extends ParentEq {
        int c;

        ChildEq(int p, int c) {
            super(p);
            this.c = c;
        }
    }

    // ===================================================================
    // 4. @Data : getter/setter/toString/equals/hashCode/RequiredArgsConstructor
    // ===================================================================
    @Data
    static class DataBean {
        private final int code;
        private String label;
    }

    // ===================================================================
    // 5. @Value : immutable, final class, no setters, all-args ctor
    // ===================================================================
    @Value
    static class ValueBean {
        int x;
        String name;
    }

    // ===================================================================
    // 6. @NoArgs / @AllArgs / @RequiredArgs(staticName)
    // ===================================================================
    @NoArgsConstructor
    @AllArgsConstructor
    static class Ctors {
        int a;
        String b;
    }

    @RequiredArgsConstructor(staticName = "of")
    @Getter
    static class ReqCtor {
        private final int id;
        private final String tag;
    }

    // ===================================================================
    // 7. @Builder + @Builder.Default + @Singular
    // ===================================================================
    @Builder
    @Getter
    static class BBean {
        private int x;
        @Builder.Default
        private String tag = "def";
        @Singular
        private List<String> items;
        @Singular
        private Map<String, Integer> scores;
    }

    // ===================================================================
    // 8. @SuperBuilder : parent / child
    // ===================================================================
    @SuperBuilder
    @Getter
    static class SbParent {
        private int p;
    }

    @SuperBuilder
    @Getter
    static class SbChild extends SbParent {
        private int c;
    }

    // ===================================================================
    // 9. @With : withX returns new instance, original unchanged
    // ===================================================================
    @AllArgsConstructor
    @Getter
    static class WithBean {
        @With
        private final int a;
        @With
        private final String b;
    }

    // ===================================================================
    // 10. @NonNull : ctor / setter / method param null-guards
    // ===================================================================
    @AllArgsConstructor
    @Getter
    static class Nn {
        @NonNull
        private String name;
        private int v;
    }

    @Setter
    static class NnSetter {
        @NonNull
        private String label;
    }

    static class NnMethod {
        static int useName(@NonNull String s) {
            return s.length();
        }
    }

    // ===================================================================
    // 11. @SneakyThrows : checked exception without throws clause
    // ===================================================================
    static class Sneaky {
        @SneakyThrows
        String read() {
            throw new java.io.IOException("boom");
        }
    }

    // ===================================================================
    // 12. @Synchronized : generated $lock and named lock
    // ===================================================================
    static class Sync {
        private int counter = 0;
        private final Object lock = new Object();

        @Synchronized
        void inc() {
            counter++;
        }

        @Synchronized("lock")
        void inc2() {
            counter++;
        }

        int get() {
            return counter;
        }
    }

    // ===================================================================
    // 13. @Cleanup : auto close in finally
    // ===================================================================
    static class Res implements AutoCloseable {
        boolean closed = false;

        @Override
        public void close() {
            closed = true;
        }
    }

    static class CleanupHost {
        static Res lastRes;

        static int run() {
            @Cleanup
            Res r = new Res();
            lastRes = r;
            return 1;
        }
    }

    // ===================================================================
    // 14. @Log : java.util.logging.Logger (JDK-builtin)
    // ===================================================================
    @Log
    static class Logged {
    }

    @Log(topic = "MY_TOPIC")
    static class LoggedTopic {
    }

    // ===================================================================
    // 15. @Accessors : fluent / chain / prefix
    // ===================================================================
    @Accessors(fluent = true)
    @Getter
    @Setter
    @NoArgsConstructor
    static class Fluent {
        private int x;
        private String y;
    }

    @Accessors(chain = true)
    @Getter
    @Setter
    @NoArgsConstructor
    static class Chain {
        private int a;
        private String b;
    }

    @Accessors(prefix = "m")
    @Getter
    @AllArgsConstructor
    static class Prefixed {
        private int mValue;
    }

    // ===================================================================
    // 16. @FieldDefaults : makeFinal + level
    // ===================================================================
    @FieldDefaults(makeFinal = true, level = AccessLevel.PRIVATE)
    @AllArgsConstructor
    @Getter
    static class Fd {
        int a;
        String b;
    }

    // ===================================================================
    // 17. @UtilityClass : private ctor, static members, final class
    // ===================================================================
    @UtilityClass
    static class Util {
        int CONST = 42;

        int square(int n) {
            return n * n;
        }
    }

    // ===================================================================
    // 18. @Getter(lazy=true) : compute once, cache
    // ===================================================================
    static class Lazy {
        static int calls = 0;

        @Getter(lazy = true)
        private final int value = compute();

        private int compute() {
            calls++;
            return 99;
        }
    }

    // ===================================================================
    // MAIN
    // ===================================================================
    public static void main(String[] args) throws Exception {

        // ---- 1. @Getter / @Setter ----
        Account acc = new Account("ID-1");
        acc.setBalance(150L);
        acc.setOwner("alice");
        acc.setActive(true);
        eqi("getter.balance", 150L, acc.getBalance());
        eq("getter.owner", "alice", acc.getOwner());
        eq("getter.id", "ID-1", acc.getId());
        check("setter.active", acc.isActive());
        // @Setter(AccessLevel.NONE) -> no setId method
        check("setter.none.absent", !hasMethod(Account.class, "setId", String.class));
        // boolean getter is isActive (not getActive)
        check("getter.boolean.isActive", hasMethod(Account.class, "isActive"));
        check("getter.boolean.getActive.absent", !hasMethod(Account.class, "getActive"));
        // onMethod_ propagation: getId has @Deprecated
        Method getId = Account.class.getDeclaredMethod("getId");
        check("onMethod.getter.deprecated", getId.isAnnotationPresent(Deprecated.class));
        // onMethod_ on setter: setActive has @Marker
        Method setActive = Account.class.getDeclaredMethod("setActive", boolean.class);
        check("onMethod.setter.marker", setActive.isAnnotationPresent(Marker.class));

        // ---- 2. @ToString ----
        eq("toString.default", "LombokCarpet.Point(x=3, y=4)", new Point().toString());
        eq("toString.of", "LombokCarpet.PointOf(x=3)", new PointOf().toString());
        eq("toString.exclude", "LombokCarpet.PointEx(x=3)", new PointEx().toString());
        eq("toString.callSuper", "LombokCarpet.Sub1(super=SUP, v=9)", new Sub1().toString());

        // ---- 3. @EqualsAndHashCode ----
        Pair p1 = new Pair(1, 2);
        Pair p2 = new Pair(1, 2);
        Pair p3 = new Pair(1, 9);
        check("equals.equal", p1.equals(p2));
        check("equals.symmetric", p2.equals(p1));
        check("equals.hashCodeEqual", p1.hashCode() == p2.hashCode());
        check("equals.notEqual", !p1.equals(p3));
        check("equals.notNull", !p1.equals(null));
        check("equals.reflexive", p1.equals(p1));
        // exclude: b ignored
        check("equals.exclude.equal", new PairEx(5, 1).equals(new PairEx(5, 999)));
        check("equals.exclude.diffA", !new PairEx(5, 1).equals(new PairEx(6, 1)));
        // callSuper: parent field participates
        check("equals.callSuper.equal", new ChildEq(1, 2).equals(new ChildEq(1, 2)));
        check("equals.callSuper.diffParent", !new ChildEq(1, 2).equals(new ChildEq(9, 2)));
        check("equals.callSuper.diffChild", !new ChildEq(1, 2).equals(new ChildEq(1, 3)));

        // ---- 4. @Data ----
        DataBean db = new DataBean(7);
        db.setLabel("hi");
        eqi("data.getCode", 7, db.getCode());
        eq("data.getLabel", "hi", db.getLabel());
        eq("data.toString", "LombokCarpet.DataBean(code=7, label=hi)", db.toString());
        DataBean db2 = new DataBean(7);
        db2.setLabel("hi");
        check("data.equals", db.equals(db2));
        check("data.hashCode", db.hashCode() == db2.hashCode());
        // @Data has no setCode for final field
        check("data.final.noSetter", !hasMethod(DataBean.class, "setCode", int.class));

        // ---- 5. @Value ----
        ValueBean vb = new ValueBean(5, "a");
        eqi("value.getX", 5, vb.getX());
        eq("value.getName", "a", vb.getName());
        check("value.classFinal", Modifier.isFinal(ValueBean.class.getModifiers()));
        Field vx = ValueBean.class.getDeclaredField("x");
        check("value.field.private", Modifier.isPrivate(vx.getModifiers()));
        check("value.field.final", Modifier.isFinal(vx.getModifiers()));
        check("value.noSetter", !hasMethod(ValueBean.class, "setX", int.class));
        check("value.equals", vb.equals(new ValueBean(5, "a")));
        check("value.notEqual", !vb.equals(new ValueBean(6, "a")));
        eq("value.toString", "LombokCarpet.ValueBean(x=5, name=a)", vb.toString());

        // ---- 6. constructors ----
        Ctors c0 = new Ctors();
        eqi("noargs.default.a", 0, c0.a);
        check("noargs.default.b", c0.b == null);
        Ctors c1 = new Ctors(3, "z");
        eqi("allargs.a", 3, c1.a);
        eq("allargs.b", "z", c1.b);
        ReqCtor rc = ReqCtor.of(11, "t");
        eqi("reqargs.staticFactory.id", 11, rc.getId());
        eq("reqargs.staticFactory.tag", "t", rc.getTag());
        // staticName makes the constructor private
        Constructor<ReqCtor> rcCtor = ReqCtor.class.getDeclaredConstructor(int.class, String.class);
        check("reqargs.ctor.private", Modifier.isPrivate(rcCtor.getModifiers()));
        check("reqargs.staticFactory.exists", hasMethod(ReqCtor.class, "of", int.class, String.class));

        // ---- 7. @Builder + Default + Singular ----
        BBean bDefault = BBean.builder().x(1).build();
        eqi("builder.x", 1, bDefault.getX());
        eq("builder.default.applied", "def", bDefault.getTag());
        check("builder.singular.empty", bDefault.getItems().isEmpty());
        eqi("builder.singular.empty.size", 0, bDefault.getScores().size());
        BBean bFull = BBean.builder()
                .x(2)
                .tag("override")
                .item("a")
                .item("b")
                .score("k1", 10)
                .score("k2", 20)
                .build();
        eq("builder.default.override", "override", bFull.getTag());
        eqi("builder.singular.list.size", 2, bFull.getItems().size());
        eq("builder.singular.list.0", "a", bFull.getItems().get(0));
        eq("builder.singular.list.1", "b", bFull.getItems().get(1));
        eqi("builder.singular.map.size", 2, bFull.getScores().size());
        eqi("builder.singular.map.get", 10, bFull.getScores().get("k1"));
        // @Singular collections are immutable
        expect("builder.singular.list.immutable", UnsupportedOperationException.class, null,
                () -> bFull.getItems().add("nope"));
        expect("builder.singular.map.immutable", UnsupportedOperationException.class, null,
                () -> bFull.getScores().put("x", 1));

        // ---- 8. @SuperBuilder ----
        SbChild sc = SbChild.builder().p(100).c(200).build();
        eqi("superbuilder.parentField", 100, sc.getP());
        eqi("superbuilder.childField", 200, sc.getC());

        // ---- 9. @With ----
        WithBean w1 = new WithBean(1, "orig");
        WithBean w2 = w1.withA(10);
        check("with.newInstance", w1 != w2);
        eqi("with.changed", 10, w2.getA());
        eq("with.unchanged.b", "orig", w2.getB());
        eqi("with.original.intact", 1, w1.getA());
        WithBean w3 = w1.withB("new");
        eq("with.changedB", "new", w3.getB());
        eqi("with.keptA", 1, w3.getA());

        // ---- 10. @NonNull ----
        Nn nnOk = new Nn("bob", 5);
        eq("nonnull.ctor.valid", "bob", nnOk.getName());
        expect("nonnull.ctor.null", NullPointerException.class,
                "name is marked non-null but is null", () -> new Nn(null, 5));
        NnSetter ns = new NnSetter();
        ns.setLabel("ok");
        expect("nonnull.setter.null", NullPointerException.class,
                "label is marked non-null but is null", () -> ns.setLabel(null));
        eqi("nonnull.method.valid", 4, NnMethod.useName("good"));
        expect("nonnull.method.null", NullPointerException.class,
                "s is marked non-null but is null", () -> NnMethod.useName(null));

        // ---- 11. @SneakyThrows ----
        Method readM = Sneaky.class.getDeclaredMethod("read");
        eqi("sneaky.noThrowsClause", 0, readM.getExceptionTypes().length);
        expect("sneaky.throwsAtRuntime", java.io.IOException.class, "boom",
                () -> new Sneaky().read());

        // ---- 12. @Synchronized ----
        Field lockField = Sync.class.getDeclaredField("$lock");
        check("sync.lockField.exists", lockField != null);
        check("sync.lockField.private", Modifier.isPrivate(lockField.getModifiers()));
        check("sync.lockField.final", Modifier.isFinal(lockField.getModifiers()));
        check("sync.namedLockField", Sync.class.getDeclaredField("lock") != null);
        final Sync sync = new Sync();
        final int threads = 4;
        final int iters = 1000;
        Thread[] ts = new Thread[threads];
        for (int i = 0; i < threads; i++) {
            ts[i] = new Thread(() -> {
                for (int k = 0; k < iters; k++) {
                    sync.inc();
                }
            });
        }
        for (Thread t : ts) {
            t.start();
        }
        for (Thread t : ts) {
            t.join();
        }
        eqi("sync.concurrentCount", (long) threads * iters, sync.get());
        sync.inc2();
        eqi("sync.namedLockMethod", (long) threads * iters + 1, sync.get());

        // ---- 13. @Cleanup ----
        CleanupHost.run();
        check("cleanup.closed", CleanupHost.lastRes.closed);

        // ---- 14. @Log ----
        Field logField = Logged.class.getDeclaredField("log");
        check("log.field.static", Modifier.isStatic(logField.getModifiers()));
        check("log.field.final", Modifier.isFinal(logField.getModifiers()));
        check("log.field.type", logField.getType() == java.util.logging.Logger.class);
        logField.setAccessible(true);
        java.util.logging.Logger logger = (java.util.logging.Logger) logField.get(null);
        check("log.field.nonNull", logger != null);
        eq("log.field.name", Logged.class.getName(), logger.getName());
        // can actually invoke without throwing
        logger.fine("deterministic-msg");
        ok++; // log invocation produced no exception
        // @Log(topic=...) names the logger from the topic
        Field logField2 = LoggedTopic.class.getDeclaredField("log");
        logField2.setAccessible(true);
        java.util.logging.Logger logger2 = (java.util.logging.Logger) logField2.get(null);
        eq("log.topic.name", "MY_TOPIC", logger2.getName());

        // ---- 15. @Accessors ----
        Fluent f = new Fluent();
        Fluent fRet = f.x(5);
        check("accessors.fluent.setterReturnsThis", fRet == f);
        eqi("accessors.fluent.getter", 5, f.x());
        f.y("hi");
        eq("accessors.fluent.getterStr", "hi", f.y());
        check("accessors.fluent.noGetPrefix", !hasMethod(Fluent.class, "getX"));
        Chain ch = new Chain();
        Chain chRet = ch.setA(1);
        check("accessors.chain.returnsThis", chRet == ch);
        check("accessors.chain.fluentChain", ch.setA(2).setB("z") == ch);
        eqi("accessors.chain.getA", 2, ch.getA());
        eq("accessors.chain.getB", "z", ch.getB());
        Prefixed pf = new Prefixed(7);
        eqi("accessors.prefix.getter", 7, pf.getValue());
        check("accessors.prefix.field", Prefixed.class.getDeclaredField("mValue") != null);

        // ---- 16. @FieldDefaults ----
        Fd fd = new Fd(8, "q");
        eqi("fielddefaults.getA", 8, fd.getA());
        eq("fielddefaults.getB", "q", fd.getB());
        Field fdA = Fd.class.getDeclaredField("a");
        check("fielddefaults.private", Modifier.isPrivate(fdA.getModifiers()));
        check("fielddefaults.final", Modifier.isFinal(fdA.getModifiers()));

        // ---- 17. @UtilityClass ----
        eqi("utility.staticMethod", 25, Util.square(5));
        eqi("utility.staticField", 42, Util.CONST);
        check("utility.classFinal", Modifier.isFinal(Util.class.getModifiers()));
        Method sq = Util.class.getDeclaredMethod("square", int.class);
        check("utility.method.static", Modifier.isStatic(sq.getModifiers()));
        Constructor<?>[] uCtors = Util.class.getDeclaredConstructors();
        eqi("utility.singleCtor", 1, uCtors.length);
        check("utility.ctor.private", Modifier.isPrivate(uCtors[0].getModifiers()));
        uCtors[0].setAccessible(true);
        expect("utility.ctor.throws", java.lang.reflect.InvocationTargetException.class, null,
                () -> uCtors[0].newInstance());

        // ---- 18. @Getter(lazy=true) ----
        Lazy.calls = 0;
        Lazy lz = new Lazy();
        eqi("lazy.notComputedAtCtor", 0, Lazy.calls);
        // the field was rewritten into an AtomicReference holder
        Field lzField = Lazy.class.getDeclaredField("value");
        check("lazy.field.atomicRef", lzField.getType() == AtomicReference.class);
        eqi("lazy.firstGet", 99, lz.getValue());
        eqi("lazy.computedOnce", 1, Lazy.calls);
        eqi("lazy.secondGet", 99, lz.getValue());
        eqi("lazy.stillOnce", 1, Lazy.calls);

        // ---- summary ----
        System.out.println("LOMBOK_CARPET_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("LOMBOK_DONE");
        }
    }
}
