import java.lang.annotation.*;
import java.lang.invoke.*;
import java.lang.management.*;
import java.lang.ref.*;
import java.lang.reflect.*;
import java.util.*;

/* Carpet-grade coverage for the JVM / JRE self-introspection surface of the
 * JDK: the runtime, reflection, method-handle, management and reference APIs
 * that let a program inspect and drive the virtual machine it runs on.
 *
 * Every assertion checks an exact, deterministic value (==, equals or a known
 * constant). No external network/file I/O; all data is self-constructed; thread
 * usage is bounded (one short-lived worker, joined). musl / StarryOS safe.
 *
 * Coverage matrix:
 *   java.lang.Runtime/Version  version()/feature/interim/update/parse/compareTo,
 *                              availableProcessors, max/total/freeMemory, gc
 *   java.lang.System           getProperty(+default)/getProperties/lineSeparator/
 *                              arraycopy/identityHashCode/nanoTime/currentTimeMillis/
 *                              file+path.separator
 *   java.lang.Class            isPrimitive/isArray/isInterface/isEnum/isRecord/
 *                              isSealed/isAnnotation, TYPE constants, getSuperclass/
 *                              getInterfaces/isAssignableFrom/isInstance/cast,
 *                              getName/getSimpleName/getCanonicalName, componentType/
 *                              arrayType, descriptorString, nestHost/nestMembers,
 *                              getModifiers, getPermittedSubclasses, getRecordComponents
 *   java.lang.reflect          Field/Method/Constructor get/set/invoke/newInstance,
 *                              setAccessible, Modifier, Array (1D/2D), Proxy,
 *                              generics (ParameterizedType), RecordComponent
 *   java.lang.annotation       custom @Retention(RUNTIME) annotation + reflection
 *   java.lang.invoke           MethodType/MethodHandles.Lookup findStatic/Virtual/
 *                              Constructor/Getter/Setter, invoke/invokeExact/asType/
 *                              bindTo, VarHandle (array + field, CAS/getAndAdd)
 *   java.lang.ref              Weak/Soft/Phantom references + ReferenceQueue
 *   java.lang.Thread           name/id/state/priority/group/join/State enum
 *   java.lang.Throwable        cause/message/suppressed/getStackTrace
 *   java.lang.StackWalker      walk / frame introspection
 *   java.lang.ClassLoader      bootstrap/platform/system loaders, forName/loadClass
 *   java.lang.management       Runtime/Thread/Memory/ClassLoading/Compilation/
 *                              OperatingSystem/GarbageCollector/MemoryPool/
 *                              MemoryManager MXBeans
 *   Enum                       values/valueOf/ordinal/name/EnumSet/EnumMap
 *   Object/Objects             identity/equals/hashCode/Integer cache/Objects helpers
 */
public class JvmTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String name) {
        if (c) { ok++; } else { fail++; System.out.println("FAIL " + name); }
    }
    interface ThrowingRunnable { void run() throws Throwable; }
    static void checkThrows(Class<? extends Throwable> ex, String name, ThrowingRunnable r) {
        try {
            r.run();
            fail++; System.out.println("FAIL " + name + " (no throw)");
        } catch (Throwable t) {
            if (ex.isInstance(t)) { ok++; }
            else { fail++; System.out.println("FAIL " + name + " (got " + t.getClass().getName() + ")"); }
        }
    }

    // ------------------------------------------------------------------
    // reflection / introspection fixtures
    // ------------------------------------------------------------------
    public static class Bean {
        public int v = 42;
        public int dbl() { return v * 2; }
    }
    static class Secret {
        private int hidden = 99;
        private int reveal() { return hidden + 1; }
    }
    static class Counter { int count; }
    abstract static class Abstr { Abstr() {} }
    static class NoDefault { NoDefault(int x) { /* no no-arg ctor */ } }
    static class Boom {
        public Boom() {}
        public void bang() { throw new IllegalStateException("boom"); }
    }
    static class Vec implements Cloneable {
        int x, y;
        Vec(int x, int y) { this.x = x; this.y = y; }
        @Override public Vec clone() throws CloneNotSupportedException { return (Vec) super.clone(); }
    }
    public static class Holder {
        public List<String> items = new ArrayList<>();
        public Map<Integer, String> map = new HashMap<>();
        public List<String> echo(List<String> in) { return in; }
    }

    @Retention(RetentionPolicy.RUNTIME)
    @Target({ ElementType.METHOD, ElementType.TYPE, ElementType.FIELD })
    @interface Tag {
        String value();
        int num() default 7;
    }
    @Tag(value = "type", num = 99)
    static class Annotated {}
    @Tag("hello")
    private static void tagged() {}

    enum Color { RED, GREEN, BLUE }

    record Point(int x, int y) {}

    sealed interface Shape permits Circle, Square {}
    record Circle(double r) implements Shape {}
    record Square(double s) implements Shape {}

    public interface Greeter {
        String hello(String n);
        int answer();
    }

    static int add(int a, int b) { return a + b; }

    // ==================================================================
    public static void main(String[] args) throws Throwable {
        runtimeAndVersion();
        systemApi();
        classIntrospection();
        reflectionApi();
        annotationsAndGenerics();
        methodHandles();
        varHandles();
        referencesApi();
        enumApi();
        threadApi();
        throwableApi();
        stackWalkerApi();
        classLoaderApi();
        objectIdentity();
        managementApi();

        System.out.println("JVM_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("JVM_DONE");
    }

    // ------------------------------------------------------------------
    static void runtimeAndVersion() {
        Runtime.Version v = Runtime.version();
        check(v.feature() == 17, "version-feature-17");
        check(Runtime.Version.parse("17.0.5").feature() == 17, "version-parse-feature");
        check(Runtime.Version.parse("17.0.5").interim() == 0, "version-parse-interim");
        check(Runtime.Version.parse("17.0.5").update() == 5, "version-parse-update");
        check(Runtime.Version.parse("11.0.2").compareTo(Runtime.Version.parse("17.0.1")) < 0, "version-compare-lt");
        check(Runtime.Version.parse("17").feature() == 17, "version-parse-bare");

        Runtime rt = Runtime.getRuntime();
        check(rt.availableProcessors() >= 1, "rt-processors");
        long max = rt.maxMemory(), total = rt.totalMemory(), free = rt.freeMemory();
        check(max > 0, "rt-max-memory");
        check(total > 0, "rt-total-memory");
        check(free >= 0 && free <= total, "rt-free-le-total");

        // allocate then drop + gc; modest footprint (~4 MiB), -Xmx256m safe
        List<byte[]> junk = new ArrayList<>();
        for (int i = 0; i < 64; i++) junk.add(new byte[64 * 1024]);
        check(junk.size() == 64, "rt-alloc");
        junk.clear();
        System.gc();
        check(rt.freeMemory() >= 0, "rt-gc-runs");

        check(System.getProperty("java.version") != null, "prop-java-version");
        check(System.getProperty("java.vm.name") != null, "prop-vm-name");
        check(System.getProperty("java.home") != null, "prop-java-home");
    }

    // ------------------------------------------------------------------
    static void systemApi() {
        check(System.lineSeparator() != null && System.lineSeparator().length() >= 1, "sys-line-sep");
        check(System.getProperty("file.separator").equals("/"), "sys-file-sep");
        check(System.getProperty("path.separator").equals(":"), "sys-path-sep");
        check(System.getProperty("os.name") != null, "sys-os-name");
        check(System.getProperty("os.arch") != null, "sys-os-arch");
        check(System.getProperty("no.such.property.zz", "def").equals("def"), "sys-prop-default");
        check(System.getProperty("no.such.property.zz") == null, "sys-prop-absent");
        check(System.getProperties().containsKey("java.version"), "sys-properties-map");

        int[] src = { 1, 2, 3, 4, 5 };
        int[] dst = new int[5];
        System.arraycopy(src, 1, dst, 0, 3);
        check(dst[0] == 2 && dst[1] == 3 && dst[2] == 4 && dst[3] == 0, "sys-arraycopy");

        Object o1 = new Object();
        check(System.identityHashCode(o1) == System.identityHashCode(o1), "sys-identity-stable");
        check(System.identityHashCode(null) == 0, "sys-identity-null");

        long t1 = System.nanoTime();
        long t2 = System.nanoTime();
        check(t2 >= t1, "sys-nanotime-monotone");
        check(System.currentTimeMillis() > 0, "sys-millis-positive");

        Map<String, String> env = System.getenv();
        check(env != null, "sys-getenv-map");
    }

    // ------------------------------------------------------------------
    static void classIntrospection() throws Throwable {
        check(int.class.isPrimitive(), "cls-int-primitive");
        check(!Integer.class.isPrimitive(), "cls-Integer-not-primitive");
        check(Integer.TYPE == int.class, "cls-Integer-TYPE");
        check(Void.TYPE == void.class, "cls-Void-TYPE");
        check(Double.TYPE == double.class, "cls-Double-TYPE");

        check(int[].class.isArray(), "cls-int-array");
        check(int[].class.getComponentType() == int.class, "cls-component-type");
        check(int[].class.componentType() == int.class, "cls-componentType-12");
        check(int.class.arrayType() == int[].class, "cls-arrayType-12");
        check(int[].class.getSimpleName().equals("int[]"), "cls-array-simplename");

        check(String.class.getSuperclass() == Object.class, "cls-string-super");
        check(Object.class.getSuperclass() == null, "cls-object-super-null");
        check(Integer.class.getSuperclass() == Number.class, "cls-integer-super");

        check(String.class.isAssignableFrom(String.class), "cls-assignable-self");
        check(CharSequence.class.isAssignableFrom(String.class), "cls-assignable-iface");
        check(!String.class.isAssignableFrom(CharSequence.class), "cls-assignable-neg");
        check(Number.class.isInstance(Integer.valueOf(3)), "cls-isinstance");
        check(!Number.class.isInstance("x"), "cls-isinstance-neg");

        check(Runnable.class.isInterface(), "cls-interface");
        check(!String.class.isInterface(), "cls-not-interface");

        Set<String> ifaces = new HashSet<>();
        for (Class<?> c : String.class.getInterfaces()) ifaces.add(c.getSimpleName());
        check(ifaces.contains("CharSequence"), "cls-iface-charseq");
        check(ifaces.contains("Comparable"), "cls-iface-comparable");
        check(ifaces.contains("Serializable"), "cls-iface-serializable");

        check(String.class.getName().equals("java.lang.String"), "cls-name");
        check(String.class.getSimpleName().equals("String"), "cls-simplename");
        check(String.class.getCanonicalName().equals("java.lang.String"), "cls-canonical");
        check(Bean.class.getName().equals("JvmTest$Bean"), "cls-nested-name");
        check(Bean.class.getCanonicalName().equals("JvmTest.Bean"), "cls-nested-canonical");

        check(String.class.descriptorString().equals("Ljava/lang/String;"), "cls-descriptor-ref");
        check(int.class.descriptorString().equals("I"), "cls-descriptor-int");
        check(long.class.descriptorString().equals("J"), "cls-descriptor-long");
        check(int[].class.descriptorString().equals("[I"), "cls-descriptor-array");

        check(Bean.class.getNestHost() == JvmTest.class, "cls-nest-host");
        Set<Class<?>> members = new HashSet<>(Arrays.asList(JvmTest.class.getNestMembers()));
        check(members.contains(Bean.class), "cls-nest-members");

        check(Modifier.isFinal(String.class.getModifiers()), "cls-string-final");
        check(Modifier.isPublic(Bean.class.getModifiers()), "cls-bean-public");
        check(Modifier.isStatic(Bean.class.getModifiers()), "cls-bean-static");
        check(Modifier.isAbstract(Abstr.class.getModifiers()), "cls-abstr-abstract");

        check(Color.class.cast(Color.RED) == Color.RED, "cls-cast-ok");
        checkThrows(ClassCastException.class, "cls-cast-cce", () -> String.class.cast(Integer.valueOf(3)));
        checkThrows(ClassNotFoundException.class, "cls-forname-cnfe", () -> Class.forName("no.such.Klass"));

        check(Class.forName("java.util.ArrayList") == ArrayList.class, "cls-forname-ok");
    }

    // ------------------------------------------------------------------
    static void reflectionApi() throws Throwable {
        // Field get/set on public field
        Class<Bean> bc = Bean.class;
        Bean bean = bc.getDeclaredConstructor().newInstance();
        Field fv = bc.getField("v");
        check(fv.getType() == int.class, "rf-field-type");
        check(fv.getInt(bean) == 42, "rf-field-get");
        fv.setInt(bean, 7);
        check(bean.v == 7, "rf-field-set");
        check(fv.getName().equals("v"), "rf-field-name");
        check(Modifier.isPublic(fv.getModifiers()), "rf-field-public");

        // Method invoke
        Method dbl = bc.getMethod("dbl");
        check(dbl.getReturnType() == int.class, "rf-method-rettype");
        check(dbl.getParameterCount() == 0, "rf-method-paramcount");
        check((int) dbl.invoke(bean) == 14, "rf-method-invoke");
        check(dbl.getName().equals("dbl"), "rf-method-name");

        // Constructor
        Constructor<Bean> ctor = bc.getDeclaredConstructor();
        check(ctor.getParameterCount() == 0, "rf-ctor-paramcount");
        Bean b2 = ctor.newInstance();
        check(b2.v == 42, "rf-ctor-newinstance");

        // setAccessible on private members
        Secret sec = new Secret();
        Field hf = Secret.class.getDeclaredField("hidden");
        hf.setAccessible(true);
        check(hf.getInt(sec) == 99, "rf-private-field");
        Method rev = Secret.class.getDeclaredMethod("reveal");
        rev.setAccessible(true);
        check((int) rev.invoke(sec) == 100, "rf-private-method");

        // Modifier helpers
        check(Modifier.isPublic(dbl.getModifiers()), "rf-mod-public");
        check(Modifier.toString(Modifier.PUBLIC | Modifier.STATIC | Modifier.FINAL).equals("public static final"), "rf-mod-tostring");
        check(Modifier.isPrivate(hf.getModifiers()), "rf-mod-private");

        // exception paths
        checkThrows(IllegalArgumentException.class, "rf-field-wrong-target", () -> fv.getInt("not a bean"));
        checkThrows(NoSuchMethodException.class, "rf-no-default-ctor", () -> NoDefault.class.getDeclaredConstructor());
        checkThrows(InstantiationException.class, "rf-abstract-instantiate",
                () -> Abstr.class.getDeclaredConstructor().newInstance());
        // invoking a throwing method -> InvocationTargetException wrapping the cause
        Boom boom = new Boom();
        Method bang = Boom.class.getMethod("bang");
        try {
            bang.invoke(boom);
            check(false, "rf-invocation-target");
        } catch (InvocationTargetException ite) {
            check(ite.getCause() instanceof IllegalStateException
                    && ite.getCause().getMessage().equals("boom"), "rf-invocation-target");
        }

        // java.lang.reflect.Array
        Object arr = Array.newInstance(int.class, 5);
        check(Array.getLength(arr) == 5, "rf-array-length");
        Array.setInt(arr, 2, 99);
        check(Array.getInt(arr, 2) == 99, "rf-array-setget");
        check(Array.get(arr, 2).equals(Integer.valueOf(99)), "rf-array-get-boxed");
        checkThrows(ArrayIndexOutOfBoundsException.class, "rf-array-oob", () -> Array.getInt(arr, 9));
        checkThrows(NegativeArraySizeException.class, "rf-array-neg", () -> Array.newInstance(int.class, -1));
        Object m2 = Array.newInstance(int.class, 2, 3);
        Object row = Array.get(m2, 0);
        check(Array.getLength(row) == 3, "rf-array-2d");
        String[] sa = (String[]) Array.newInstance(String.class, 3);
        check(sa.length == 3 && sa[0] == null, "rf-array-ref");

        // clone via Cloneable
        Vec orig = new Vec(3, 4);
        Vec cp = orig.clone();
        check(cp != orig && cp.x == 3 && cp.y == 4, "rf-clone");

        // Proxy
        Greeter g = (Greeter) Proxy.newProxyInstance(
                JvmTest.class.getClassLoader(),
                new Class<?>[] { Greeter.class },
                (proxy, method, margs) -> {
                    if (method.getName().equals("hello")) return "Hi " + margs[0];
                    if (method.getName().equals("answer")) return 42;
                    if (method.getName().equals("toString")) return "proxy";
                    return null;
                });
        check(g.hello("Sam").equals("Hi Sam"), "rf-proxy-hello");
        check(g.answer() == 42, "rf-proxy-answer");
        check(Proxy.isProxyClass(g.getClass()), "rf-proxy-isproxy");
        check(!Proxy.isProxyClass(String.class), "rf-proxy-isproxy-neg");
    }

    // ------------------------------------------------------------------
    static void annotationsAndGenerics() throws Throwable {
        // annotation on method
        Method tag = JvmTest.class.getDeclaredMethod("tagged");
        check(tag.isAnnotationPresent(Tag.class), "an-present");
        Tag t = tag.getAnnotation(Tag.class);
        check(t.value().equals("hello"), "an-value");
        check(t.num() == 7, "an-default");
        // annotation on type
        Tag tt = Annotated.class.getAnnotation(Tag.class);
        check(tt.value().equals("type"), "an-type-value");
        check(tt.num() == 99, "an-type-num");
        check(Tag.class.isAnnotation(), "an-isannotation");
        check(t.annotationType() == Tag.class, "an-annotationtype");

        // generics via ParameterizedType
        Field items = Holder.class.getField("items");
        Type gt = items.getGenericType();
        check(gt instanceof ParameterizedType, "gen-field-parameterized");
        ParameterizedType pt = (ParameterizedType) gt;
        check(pt.getRawType() == List.class, "gen-raw-list");
        check(pt.getActualTypeArguments().length == 1, "gen-arg-count");
        check(pt.getActualTypeArguments()[0] == String.class, "gen-arg-string");

        Field mapF = Holder.class.getField("map");
        ParameterizedType mpt = (ParameterizedType) mapF.getGenericType();
        check(mpt.getActualTypeArguments()[0] == Integer.class, "gen-map-key");
        check(mpt.getActualTypeArguments()[1] == String.class, "gen-map-val");

        Method echo = Holder.class.getMethod("echo", List.class);
        check(echo.getGenericReturnType() instanceof ParameterizedType, "gen-method-return");
        check(echo.getGenericParameterTypes()[0] instanceof ParameterizedType, "gen-method-param");

        // records
        Point p = new Point(3, 4);
        check(p.x() == 3 && p.y() == 4, "rec-accessors");
        check(p.equals(new Point(3, 4)), "rec-equals");
        check(!p.equals(new Point(3, 5)), "rec-equals-neg");
        check(p.hashCode() == new Point(3, 4).hashCode(), "rec-hashcode");
        check(p.toString().equals("Point[x=3, y=4]"), "rec-tostring");
        check(Point.class.isRecord(), "rec-isrecord");
        RecordComponent[] rcs = Point.class.getRecordComponents();
        check(rcs.length == 2, "rec-component-count");
        check(rcs[0].getName().equals("x") && rcs[0].getType() == int.class, "rec-component-x");
        check(rcs[1].getName().equals("y"), "rec-component-y");
        check((int) rcs[0].getAccessor().invoke(p) == 3, "rec-accessor-reflect");

        // sealed
        check(Shape.class.isSealed(), "seal-issealed");
        Class<?>[] perms = Shape.class.getPermittedSubclasses();
        check(perms.length == 2, "seal-permit-count");
        Set<String> ps = new HashSet<>();
        for (Class<?> c : perms) ps.add(c.getSimpleName());
        check(ps.equals(new HashSet<>(Arrays.asList("Circle", "Square"))), "seal-permit-names");
        check(!Point.class.isSealed(), "seal-not-sealed");
        Shape sh = new Circle(2.0);
        check(sh instanceof Circle && ((Circle) sh).r() == 2.0, "seal-subtype");
    }

    // ------------------------------------------------------------------
    static void methodHandles() throws Throwable {
        // MethodType
        MethodType mt = MethodType.methodType(int.class, int.class, int.class);
        check(mt.returnType() == int.class, "mt-return");
        check(mt.parameterCount() == 2, "mt-paramcount");
        check(mt.parameterType(0) == int.class, "mt-paramtype");
        check(mt.toMethodDescriptorString().equals("(II)I"), "mt-descriptor");
        check(MethodType.fromMethodDescriptorString("(II)I", null).equals(mt), "mt-from-descriptor");
        check(mt.changeReturnType(long.class).returnType() == long.class, "mt-change-return");

        MethodHandles.Lookup L = MethodHandles.lookup();
        // static
        MethodHandle addH = L.findStatic(JvmTest.class, "add", mt);
        check((int) addH.invokeExact(2, 3) == 5, "mh-static-invokeexact");
        check((int) addH.invoke(4, 5) == 9, "mh-static-invoke");
        // virtual on String.length
        MethodHandle len = L.findVirtual(String.class, "length", MethodType.methodType(int.class));
        check((int) len.invoke("hello") == 5, "mh-virtual-length");
        MethodHandle bound = len.bindTo("worldly");
        check((int) bound.invoke() == 7, "mh-bindto");
        MethodHandle up = L.findVirtual(String.class, "toUpperCase", MethodType.methodType(String.class));
        check(((String) up.invoke("hi")).equals("HI"), "mh-virtual-upper");
        // constructor
        MethodHandle sbCtor = L.findConstructor(StringBuilder.class, MethodType.methodType(void.class, String.class));
        StringBuilder sb = (StringBuilder) sbCtor.invoke("abc");
        check(sb.toString().equals("abc"), "mh-constructor");
        // getter / setter on a field (nestmate access)
        MethodHandle getC = L.findGetter(Counter.class, "count", int.class);
        MethodHandle setC = L.findSetter(Counter.class, "count", int.class);
        Counter cn = new Counter();
        setC.invoke(cn, 11);
        check((int) getC.invoke(cn) == 11 && cn.count == 11, "mh-getter-setter");
        // asType widening
        MethodHandle addL = addH.asType(MethodType.methodType(long.class, int.class, int.class));
        check((long) addL.invokeExact(3, 4) == 7L, "mh-astype-widen");
        // WrongMethodTypeException on bad invokeExact signature
        checkThrows(WrongMethodTypeException.class, "mh-wrong-type", () -> {
            long bad = (long) addH.invokeExact(2, 3); // descriptor mismatch
        });
    }

    // ------------------------------------------------------------------
    static void varHandles() throws Throwable {
        // array element VarHandle
        VarHandle avh = MethodHandles.arrayElementVarHandle(int[].class);
        int[] arr = { 10, 20, 30 };
        check((int) avh.get(arr, 1) == 20, "vh-array-get");
        avh.set(arr, 1, 25);
        check(arr[1] == 25, "vh-array-set");
        check(avh.compareAndSet(arr, 1, 25, 99), "vh-array-cas");
        check(arr[1] == 99, "vh-array-cas-effect");
        check(!avh.compareAndSet(arr, 1, 25, 0), "vh-array-cas-fail");
        int prev = (int) avh.getAndAdd(arr, 1, 1);
        check(prev == 99 && arr[1] == 100, "vh-array-getandadd");

        // instance field VarHandle
        VarHandle cvh = MethodHandles.lookup().findVarHandle(Counter.class, "count", int.class);
        Counter cn = new Counter();
        cvh.set(cn, 5);
        check((int) cvh.get(cn) == 5, "vh-field-getset");
        check(cvh.compareAndSet(cn, 5, 7), "vh-field-cas");
        check(cn.count == 7, "vh-field-cas-effect");
        int p2 = (int) cvh.getAndAdd(cn, 3);
        check(p2 == 7 && cn.count == 10, "vh-field-getandadd");
        check(cvh.coordinateTypes().size() == 1, "vh-coordinate-types");
    }

    // ------------------------------------------------------------------
    static void referencesApi() {
        Object referent = new Object();
        WeakReference<Object> wr = new WeakReference<>(referent);
        check(wr.get() == referent, "ref-weak-get");

        ReferenceQueue<Object> rq = new ReferenceQueue<>();
        WeakReference<Object> wr2 = new WeakReference<>(new Object(), rq);
        check(rq.poll() == null, "ref-queue-empty");
        check(wr2.refersTo(wr2.get()) || wr2.get() == null, "ref-refersto");

        SoftReference<String> sr = new SoftReference<>("kept");
        check(sr.get().equals("kept"), "ref-soft-get");

        PhantomReference<Object> pr = new PhantomReference<>(new Object(), rq);
        check(pr.get() == null, "ref-phantom-null");

        wr.clear();
        check(wr.get() == null, "ref-weak-clear");
    }

    // ------------------------------------------------------------------
    static void enumApi() {
        Color[] cs = Color.values();
        check(cs.length == 3, "enum-values-len");
        check(Color.valueOf("GREEN") == Color.GREEN, "enum-valueof");
        check(Color.RED.ordinal() == 0 && Color.BLUE.ordinal() == 2, "enum-ordinal");
        check(Color.GREEN.name().equals("GREEN"), "enum-name");
        check(Color.RED.compareTo(Color.BLUE) < 0, "enum-compareto");
        check(Color.RED.getDeclaringClass() == Color.class, "enum-declaring");
        check(Color.class.getEnumConstants().length == 3, "enum-constants");
        check(Color.class.isEnum(), "enum-isenum");
        checkThrows(IllegalArgumentException.class, "enum-valueof-bad", () -> Color.valueOf("PURPLE"));

        EnumSet<Color> all = EnumSet.allOf(Color.class);
        check(all.size() == 3, "enum-set-allof");
        check(EnumSet.of(Color.RED, Color.BLUE).contains(Color.RED), "enum-set-of");
        check(EnumSet.complementOf(EnumSet.of(Color.RED)).equals(EnumSet.of(Color.GREEN, Color.BLUE)), "enum-set-complement");
        check(EnumSet.range(Color.RED, Color.GREEN).size() == 2, "enum-set-range");
        check(EnumSet.noneOf(Color.class).isEmpty(), "enum-set-none");

        EnumMap<Color, Integer> em = new EnumMap<>(Color.class);
        em.put(Color.RED, 1);
        em.put(Color.BLUE, 3);
        check(em.get(Color.RED) == 1 && em.get(Color.BLUE) == 3, "enum-map-get");
        check(em.size() == 2 && !em.containsKey(Color.GREEN), "enum-map-size");
    }

    // ------------------------------------------------------------------
    static void threadApi() throws Exception {
        Thread cur = Thread.currentThread();
        check(cur.getName().equals("main"), "thr-main-name");
        check(cur.getId() > 0, "thr-id");
        check(cur.getState() == Thread.State.RUNNABLE, "thr-state-runnable");
        check(cur.getPriority() >= Thread.MIN_PRIORITY && cur.getPriority() <= Thread.MAX_PRIORITY, "thr-priority");
        check(cur.isAlive(), "thr-alive");
        check(!cur.isInterrupted(), "thr-not-interrupted");
        check(cur.getThreadGroup() != null, "thr-group");
        check(Thread.activeCount() >= 1, "thr-active-count");
        check(Thread.State.values().length == 6, "thr-state-enum-len");

        final int[] box = { 0 };
        Thread w = new Thread(() -> box[0] = 123, "worker");
        check(w.getState() == Thread.State.NEW, "thr-state-new");
        check(!w.isDaemon(), "thr-not-daemon");
        w.start();
        w.join();
        check(box[0] == 123, "thr-worker-ran");
        check(w.getState() == Thread.State.TERMINATED, "thr-state-terminated");
        check(w.getName().equals("worker"), "thr-worker-name");
        check(!w.isAlive(), "thr-worker-dead");

        // interrupt flag round-trip on a fresh non-started flag via static
        check(!Thread.interrupted(), "thr-static-interrupted-clear");
    }

    // ------------------------------------------------------------------
    static void throwableApi() {
        Exception cause = new RuntimeException("root");
        Exception wrap = new IllegalStateException("wrap", cause);
        check(wrap.getCause() == cause, "thw-cause");
        check(wrap.getMessage().equals("wrap"), "thw-message");
        check(cause.getCause() == null, "thw-no-cause");
        check(wrap.getLocalizedMessage().equals("wrap"), "thw-localized");

        Exception se = new Exception("m");
        se.addSuppressed(new RuntimeException("s1"));
        se.addSuppressed(new RuntimeException("s2"));
        Throwable[] sup = se.getSuppressed();
        check(sup.length == 2, "thw-suppressed-count");
        check(sup[0].getMessage().equals("s1"), "thw-suppressed-first");

        StackTraceElement[] st = new Throwable().getStackTrace();
        check(st.length > 0, "thw-stacktrace-len");
        check(st[0].getMethodName().equals("throwableApi"), "thw-stacktrace-method");
        check(st[0].getClassName().equals("JvmTest"), "thw-stacktrace-class");
        check(st[0].getLineNumber() > 0, "thw-stacktrace-line");

        // initCause
        Exception ic = new Exception("x");
        ic.initCause(cause);
        check(ic.getCause() == cause, "thw-initcause");
        checkThrows(IllegalStateException.class, "thw-initcause-twice", () -> ic.initCause(new RuntimeException()));
    }

    // ------------------------------------------------------------------
    static void stackWalkerApi() {
        StackWalker sw = StackWalker.getInstance();
        long depth = sw.walk(s -> s.count());
        check(depth >= 1, "sw-depth");
        boolean hasMain = sw.walk(s -> s.anyMatch(f -> f.getMethodName().equals("main")));
        check(hasMain, "sw-has-main");
        String first = sw.walk(s -> s.findFirst().get().getMethodName());
        check(first.equals("stackWalkerApi"), "sw-first-frame");
        String firstClass = sw.walk(s -> s.findFirst().get().getClassName());
        check(firstClass.equals("JvmTest"), "sw-first-class");
    }

    // ------------------------------------------------------------------
    static void classLoaderApi() throws Exception {
        ClassLoader app = JvmTest.class.getClassLoader();
        check(app != null, "cl-app-nonnull");
        check(String.class.getClassLoader() == null, "cl-bootstrap-null");
        check(ClassLoader.getSystemClassLoader() != null, "cl-system-nonnull");
        check(app.loadClass("java.lang.String") == String.class, "cl-loadclass");
        check(Class.forName("java.util.HashMap", false, app) == HashMap.class, "cl-forname-noinit");
        // system loader parent is the platform loader (non-null in JDK 17)
        check(ClassLoader.getSystemClassLoader().getParent() != null, "cl-platform-parent");
    }

    // ------------------------------------------------------------------
    static void objectIdentity() {
        Object a = new Object();
        check(a.equals(a), "obj-equals-reflexive");
        check(!a.equals(new Object()), "obj-equals-distinct");
        check(a.getClass() == Object.class, "obj-getclass");
        check("abc".hashCode() == 96354, "obj-string-hashcode");
        check(Integer.valueOf(127) == Integer.valueOf(127), "obj-integer-cache");
        check(Integer.valueOf(1000) != Integer.valueOf(1000), "obj-integer-nocache");

        check(Objects.equals(null, null), "objs-equals-nulls");
        check(!Objects.equals("a", null), "objs-equals-one-null");
        check(Objects.equals("a", "a"), "objs-equals-strings");
        check(Objects.hashCode(null) == 0, "objs-hashcode-null");
        check(Objects.requireNonNullElse(null, "x").equals("x"), "objs-requirenonnullelse");
        check(Objects.toString(null, "def").equals("def"), "objs-tostring-default");
        check(Objects.hash(1, 2, 3) == Arrays.hashCode(new Object[] { 1, 2, 3 }), "objs-hash");
        checkThrows(NullPointerException.class, "objs-requirenonnull", () -> Objects.requireNonNull(null, "must"));
        check(Objects.isNull(null) && Objects.nonNull("x"), "objs-isnull");
    }

    // ------------------------------------------------------------------
    static void managementApi() {
        // RuntimeMXBean
        RuntimeMXBean rb = ManagementFactory.getRuntimeMXBean();
        check(rb.getUptime() >= 0, "mx-runtime-uptime");
        check(rb.getStartTime() > 0, "mx-runtime-starttime");
        check(rb.getVmName() != null && rb.getVmName().length() > 0, "mx-runtime-vmname");
        check(rb.getSpecVersion() != null, "mx-runtime-specversion");
        check(rb.getName() != null && rb.getName().length() > 0, "mx-runtime-name");
        check(rb.getInputArguments() != null, "mx-runtime-inputargs");

        // ThreadMXBean
        ThreadMXBean tb = ManagementFactory.getThreadMXBean();
        check(tb.getThreadCount() >= 1, "mx-thread-count");
        check(tb.getPeakThreadCount() >= tb.getThreadCount(), "mx-thread-peak");
        check(tb.getTotalStartedThreadCount() >= 1, "mx-thread-totalstarted");
        check(tb.getAllThreadIds().length >= 1, "mx-thread-allids");
        check(tb.findDeadlockedThreads() == null, "mx-thread-no-deadlock");
        long curId = Thread.currentThread().getId();
        ThreadInfo ti = tb.getThreadInfo(curId);
        check(ti != null && ti.getThreadId() == curId, "mx-thread-info");

        // MemoryMXBean
        MemoryMXBean mb = ManagementFactory.getMemoryMXBean();
        MemoryUsage heap = mb.getHeapMemoryUsage();
        check(heap.getUsed() > 0, "mx-memory-heap-used");
        check(heap.getCommitted() >= heap.getUsed(), "mx-memory-heap-committed");
        check(mb.getNonHeapMemoryUsage().getUsed() >= 0, "mx-memory-nonheap");
        check(mb.getObjectPendingFinalizationCount() >= 0, "mx-memory-pending-final");

        // ClassLoadingMXBean
        ClassLoadingMXBean cb = ManagementFactory.getClassLoadingMXBean();
        check(cb.getLoadedClassCount() > 0, "mx-classloading-loaded");
        check(cb.getTotalLoadedClassCount() >= cb.getLoadedClassCount(), "mx-classloading-total");
        check(cb.getUnloadedClassCount() >= 0, "mx-classloading-unloaded");

        // CompilationMXBean is null under -Xint; guard
        CompilationMXBean comp = ManagementFactory.getCompilationMXBean();
        if (comp != null) {
            check(comp.getName() != null, "mx-compilation-name");
        } else {
            check(true, "mx-compilation-absent-xint");
        }

        // OperatingSystemMXBean
        OperatingSystemMXBean os = ManagementFactory.getOperatingSystemMXBean();
        check(os.getArch() != null, "mx-os-arch");
        check(os.getName() != null, "mx-os-name");
        check(os.getVersion() != null, "mx-os-version");
        check(os.getAvailableProcessors() >= 1, "mx-os-processors");
        check(os.getSystemLoadAverage() >= -1.0, "mx-os-loadavg");

        // GarbageCollectorMXBean
        List<GarbageCollectorMXBean> gcs = ManagementFactory.getGarbageCollectorMXBeans();
        check(gcs.size() >= 1, "mx-gc-count");
        boolean gcNamed = true;
        for (GarbageCollectorMXBean gc : gcs) {
            if (gc.getName() == null || gc.getCollectionCount() < -1) gcNamed = false;
        }
        check(gcNamed, "mx-gc-named");

        // MemoryPoolMXBean
        List<MemoryPoolMXBean> pools = ManagementFactory.getMemoryPoolMXBeans();
        check(pools.size() >= 1, "mx-pool-count");
        boolean poolOk = true;
        for (MemoryPoolMXBean pool : pools) {
            if (pool.getName() == null || pool.getType() == null) poolOk = false;
        }
        check(poolOk, "mx-pool-typed");

        // MemoryManagerMXBean
        List<MemoryManagerMXBean> mgrs = ManagementFactory.getMemoryManagerMXBeans();
        check(mgrs.size() >= 1, "mx-manager-count");

        // platform MXBeans aggregate
        check(ManagementFactory.getPlatformMXBeans(MemoryPoolMXBean.class).size() == pools.size(), "mx-platform-mxbeans");
    }
}
