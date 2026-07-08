package org.starry.dod;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.slf4j.MDC;
import org.slf4j.Marker;
import org.slf4j.MarkerFactory;
import org.slf4j.helpers.MessageFormatter;
import org.slf4j.helpers.FormattingTuple;

import ch.qos.logback.classic.Level;
import ch.qos.logback.classic.LoggerContext;
import ch.qos.logback.classic.PatternLayout;
import ch.qos.logback.classic.encoder.PatternLayoutEncoder;
import ch.qos.logback.classic.filter.ThresholdFilter;
import ch.qos.logback.classic.spi.ILoggingEvent;
import ch.qos.logback.classic.spi.IThrowableProxy;
import ch.qos.logback.core.OutputStreamAppender;
import ch.qos.logback.core.read.ListAppender;
import ch.qos.logback.core.spi.FilterReply;

import java.io.ByteArrayOutputStream;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * Carpet-grade coverage for the slf4j (2.0.x) + logback-classic (1.5.x) logging stack.
 * Deterministic, offline, no network, no temp files. Every assertion checks an exact
 * value (==, equals, exact formatted text), never "ran without throwing".
 */
public class LogCarpet {

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

    static void eq(String name, Object actual, Object expected) {
        boolean c = Objects.equals(actual, expected);
        if (c) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " | expected=[" + expected + "] actual=[" + actual + "]");
        }
    }

    /** Normalize platform line separators so encoder output compares deterministically. */
    static String norm(String s) {
        if (s == null) return null;
        return s.replace(System.lineSeparator(), "\n");
    }

    interface LogJob {
        void run(ch.qos.logback.classic.Logger lg);
    }

    /** Build an isolated logger + OutputStreamAppender(PatternLayoutEncoder) and capture rendered text. */
    static String render(LoggerContext ctx, String name, String pattern, Level lvl, LogJob job) {
        ByteArrayOutputStream baos = new ByteArrayOutputStream();
        PatternLayoutEncoder enc = new PatternLayoutEncoder();
        enc.setContext(ctx);
        enc.setPattern(pattern);
        enc.start();
        OutputStreamAppender<ILoggingEvent> app = new OutputStreamAppender<>();
        app.setContext(ctx);
        app.setEncoder(enc);
        app.setOutputStream(baos);
        app.setImmediateFlush(true);
        app.start();
        ch.qos.logback.classic.Logger lg = ctx.getLogger(name);
        lg.setLevel(lvl);
        lg.setAdditive(false);
        lg.detachAndStopAllAppenders();
        lg.addAppender(app);
        job.run(lg);
        app.stop();
        lg.detachAppender(app);
        return norm(baos.toString());
    }

    /** Build an isolated logger + ListAppender capturing ILoggingEvents. */
    static ListAppender<ILoggingEvent> capLogger(LoggerContext ctx, String name, Level lvl,
                                                 ch.qos.logback.classic.Logger[] out) {
        ListAppender<ILoggingEvent> la = new ListAppender<>();
        la.setContext(ctx);
        la.start();
        ch.qos.logback.classic.Logger lg = ctx.getLogger(name);
        lg.setLevel(lvl);
        lg.setAdditive(false);
        lg.detachAndStopAllAppenders();
        lg.addAppender(la);
        if (out != null && out.length > 0) out[0] = lg;
        return la;
    }

    public static void main(String[] args) {
        // Force slf4j/logback initialization, then take exclusive programmatic control.
        Logger boot = LoggerFactory.getLogger(LogCarpet.class);
        eq("boot.name", boot.getName(), "org.starry.dod.LogCarpet");
        check("boot.factory.is.logback", LoggerFactory.getILoggerFactory() instanceof LoggerContext);

        LoggerContext ctx = (LoggerContext) LoggerFactory.getILoggerFactory();
        ctx.reset(); // drop the default console appender so only our captures see events
        ctx.getLogger(Logger.ROOT_LOGGER_NAME).setLevel(Level.OFF);

        groupFactoryAndLevels(ctx);
        groupCapturedLogging(ctx);
        groupMessageFormatter();
        groupMdc(ctx);
        groupMarkers();
        groupLogbackLevel();
        groupLevelFiltering(ctx);
        groupThresholdFilter(ctx);
        groupThrowableProxy(ctx);
        groupPatternRendering(ctx);
        groupEffectiveLevel(ctx);
        groupAppenderManagement(ctx);

        System.out.println("LOG_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("LOG_DONE");
        }
    }

    // ---------------------------------------------------------------- factory + isXxxEnabled
    static void groupFactoryAndLevels(LoggerContext ctx) {
        Logger byClass = LoggerFactory.getLogger(LogCarpet.class);
        eq("factory.byClass.name", byClass.getName(), "org.starry.dod.LogCarpet");
        Logger byString = LoggerFactory.getLogger("com.acme.Service");
        eq("factory.byString.name", byString.getName(), "com.acme.Service");
        // slf4j and logback factory return the same singleton instance for a given name
        check("factory.same.instance", LoggerFactory.getLogger("com.acme.Service") == byString);
        check("factory.logback.same.instance", ctx.getLogger("com.acme.Service") == byString);

        ch.qos.logback.classic.Logger lg = ctx.getLogger("lvl.gate");
        lg.setAdditive(false);

        lg.setLevel(Level.INFO);
        check("gate.info.trace.off", !lg.isTraceEnabled());
        check("gate.info.debug.off", !lg.isDebugEnabled());
        check("gate.info.info.on", lg.isInfoEnabled());
        check("gate.info.warn.on", lg.isWarnEnabled());
        check("gate.info.error.on", lg.isErrorEnabled());

        lg.setLevel(Level.WARN);
        check("gate.warn.info.off", !lg.isInfoEnabled());
        check("gate.warn.warn.on", lg.isWarnEnabled());
        check("gate.warn.error.on", lg.isErrorEnabled());
        check("gate.warn.debug.off", !lg.isDebugEnabled());

        lg.setLevel(Level.ERROR);
        check("gate.error.warn.off", !lg.isWarnEnabled());
        check("gate.error.error.on", lg.isErrorEnabled());

        lg.setLevel(Level.TRACE);
        check("gate.trace.trace.on", lg.isTraceEnabled());
        check("gate.trace.debug.on", lg.isDebugEnabled());
        check("gate.trace.info.on", lg.isInfoEnabled());
        check("gate.trace.warn.on", lg.isWarnEnabled());
        check("gate.trace.error.on", lg.isErrorEnabled());

        lg.setLevel(Level.OFF);
        check("gate.off.trace.off", !lg.isTraceEnabled());
        check("gate.off.error.off", !lg.isErrorEnabled());

        // marker-overloaded isXxxEnabled mirrors plain variant when no turbo filters present
        Marker m = MarkerFactory.getMarker("GATE");
        lg.setLevel(Level.INFO);
        check("gate.marker.info.on", lg.isInfoEnabled(m));
        check("gate.marker.debug.off", !lg.isDebugEnabled(m));
        check("gate.isEnabledFor.info", lg.isEnabledFor(Level.INFO));
        check("gate.isEnabledFor.debug.off", !lg.isEnabledFor(Level.DEBUG));
    }

    // ---------------------------------------------------------------- captured logging via ListAppender
    static void groupCapturedLogging(LoggerContext ctx) {
        ch.qos.logback.classic.Logger[] holder = new ch.qos.logback.classic.Logger[1];
        ListAppender<ILoggingEvent> la = capLogger(ctx, "cap.basic", Level.TRACE, holder);
        Logger log = holder[0];

        log.trace("t-msg");
        log.debug("d-msg");
        log.info("i-msg");
        log.warn("w-msg");
        log.error("e-msg");
        eq("cap.count.5", la.list.size(), 5);
        eq("cap.lvl.trace", la.list.get(0).getLevel(), Level.TRACE);
        eq("cap.lvl.debug", la.list.get(1).getLevel(), Level.DEBUG);
        eq("cap.lvl.info", la.list.get(2).getLevel(), Level.INFO);
        eq("cap.lvl.warn", la.list.get(3).getLevel(), Level.WARN);
        eq("cap.lvl.error", la.list.get(4).getLevel(), Level.ERROR);
        eq("cap.msg.info", la.list.get(2).getFormattedMessage(), "i-msg");
        eq("cap.loggername", la.list.get(2).getLoggerName(), "cap.basic");
        eq("cap.thread.main", la.list.get(2).getThreadName(), "main");
        la.list.clear();

        // parameterized {} placeholders: 1, 2, vararg(3), array, null
        log.info("one={}", 11);
        log.info("a={} b={}", 1, 2);
        log.info("x={} y={} z={}", "a", "b", "c");
        log.info("arr={}", (Object) new int[]{1, 2, 3});
        log.info("strs={}", (Object) new String[]{"p", "q"});
        log.info("v={}", (Object) null);
        eq("param.count", la.list.size(), 6);
        eq("param.one", la.list.get(0).getFormattedMessage(), "one=11");
        eq("param.raw", la.list.get(0).getMessage(), "one={}");
        eq("param.argcount.one", la.list.get(0).getArgumentArray().length, 1);
        eq("param.argval.one", la.list.get(0).getArgumentArray()[0], 11);
        eq("param.two", la.list.get(1).getFormattedMessage(), "a=1 b=2");
        eq("param.argcount.two", la.list.get(1).getArgumentArray().length, 2);
        eq("param.three", la.list.get(2).getFormattedMessage(), "x=a y=b z=c");
        eq("param.argcount.three", la.list.get(2).getArgumentArray().length, 3);
        eq("param.intarray", la.list.get(3).getFormattedMessage(), "arr=[1, 2, 3]");
        eq("param.strarray", la.list.get(4).getFormattedMessage(), "strs=[p, q]");
        eq("param.null", la.list.get(5).getFormattedMessage(), "v=null");
        la.list.clear();

        // marker-carrying log records the marker on the event
        Marker net = MarkerFactory.getMarker("NETWORK");
        log.warn(net, "down={}", "eth0");
        eq("marker.event.count", la.list.size(), 1);
        eq("marker.event.msg", la.list.get(0).getFormattedMessage(), "down=eth0");
        List<Marker> ml = la.list.get(0).getMarkerList();
        check("marker.event.present", ml != null && ml.size() == 1);
        eq("marker.event.name", ml.get(0).getName(), "NETWORK");
        la.list.clear();

        // a level below the logger threshold drops the record entirely
        holder[0].setLevel(Level.WARN);
        log.info("dropped");
        log.warn("kept");
        eq("drop.count", la.list.size(), 1);
        eq("drop.kept.msg", la.list.get(0).getFormattedMessage(), "kept");
    }

    // ---------------------------------------------------------------- slf4j MessageFormatter helper
    static void groupMessageFormatter() {
        eq("mf.one", MessageFormatter.format("a{}c", "B").getMessage(), "aBc");
        eq("mf.two", MessageFormatter.format("{}-{}", "x", "y").getMessage(), "x-y");
        eq("mf.arr3", MessageFormatter.arrayFormat("{} {} {}", new Object[]{1, 2, 3}).getMessage(), "1 2 3");
        eq("mf.noph", MessageFormatter.arrayFormat("no placeholders", new Object[]{1}).getMessage(),
                "no placeholders");
        eq("mf.missing", MessageFormatter.arrayFormat("{} and {}", new Object[]{"only-one"}).getMessage(),
                "only-one and {}");
        // escaped placeholder => literal "{}"
        eq("mf.escaped", MessageFormatter.arrayFormat("\\{} literal", new Object[]{"x"}).getMessage(),
                "{} literal");
        // escaped backslash then real placeholder => "\" + value
        eq("mf.escbackslash", MessageFormatter.arrayFormat("\\\\{}", new Object[]{"x"}).getMessage(), "\\x");
        eq("mf.nullarg", MessageFormatter.arrayFormat("{}", new Object[]{null}).getMessage(), "null");
        // array argument is rendered element-by-element
        eq("mf.intarr", MessageFormatter.arrayFormat("{}", new Object[]{new int[]{1, 2, 3}}).getMessage(),
                "[1, 2, 3]");
        eq("mf.strarr", MessageFormatter.arrayFormat("{}", new Object[]{new String[]{"a", "b"}}).getMessage(),
                "[a, b]");
        // trailing throwable is detected and trimmed from arguments
        RuntimeException ex = new RuntimeException("boom");
        FormattingTuple ft = MessageFormatter.arrayFormat("v={}", new Object[]{"a", ex});
        eq("mf.thr.msg", ft.getMessage(), "v=a");
        check("mf.thr.throwable", ft.getThrowable() == ex);
        eq("mf.thr.argcount", ft.getArgArray().length, 1);
        eq("mf.thr.argval", ft.getArgArray()[0], "a");
        // FormattingTuple.NULL sentinel
        check("mf.NULL.message.null", FormattingTuple.NULL.getMessage() == null);
        // two-arg format also auto-detects a trailing throwable
        FormattingTuple ft2 = MessageFormatter.format("only {}", "one", ex);
        eq("mf.format2.msg", ft2.getMessage(), "only one");
        check("mf.format2.thr", ft2.getThrowable() == ex);
    }

    // ---------------------------------------------------------------- MDC + event MDC map
    static void groupMdc(LoggerContext ctx) {
        MDC.clear();
        check("mdc.empty.initial", MDC.get("k") == null);
        MDC.put("user", "alice");
        MDC.put("req", "r-9");
        eq("mdc.get.user", MDC.get("user"), "alice");
        eq("mdc.get.req", MDC.get("req"), "r-9");
        Map<String, String> copy = MDC.getCopyOfContextMap();
        eq("mdc.copy.size", copy.size(), 2);
        eq("mdc.copy.user", copy.get("user"), "alice");
        // copy is a snapshot, mutating it does not affect MDC
        copy.put("user", "MUTATED");
        eq("mdc.copy.independent", MDC.get("user"), "alice");
        MDC.remove("req");
        check("mdc.remove", MDC.get("req") == null);
        eq("mdc.after.remove.user", MDC.get("user"), "alice");

        // captured event reflects MDC at log time (read before clearing)
        ch.qos.logback.classic.Logger[] holder = new ch.qos.logback.classic.Logger[1];
        ListAppender<ILoggingEvent> la = capLogger(ctx, "mdc.cap", Level.DEBUG, holder);
        holder[0].info("hello");
        Map<String, String> em = la.list.get(0).getMDCPropertyMap();
        eq("mdc.event.user", em.get("user"), "alice");
        check("mdc.event.no.req", !em.containsKey("req"));

        MDC.clear();
        Map<String, String> afterClear = MDC.getCopyOfContextMap();
        eq("mdc.clear.size", afterClear == null ? 0 : afterClear.size(), 0);

        // setContextMap replaces the whole map
        Map<String, String> seed = new HashMap<>();
        seed.put("a", "1");
        seed.put("b", "2");
        MDC.setContextMap(seed);
        eq("mdc.setmap.a", MDC.get("a"), "1");
        eq("mdc.setmap.b", MDC.get("b"), "2");
        MDC.clear();

        // putCloseable removes the key automatically on close
        try (MDC.MDCCloseable c = MDC.putCloseable("scoped", "yes")) {
            eq("mdc.closeable.inside", MDC.get("scoped"), "yes");
        }
        check("mdc.closeable.after", MDC.get("scoped") == null);
        MDC.clear();
    }

    // ---------------------------------------------------------------- Marker / MarkerFactory
    static void groupMarkers() {
        Marker m1 = MarkerFactory.getMarker("M1");
        eq("marker.name", m1.getName(), "M1");
        // getMarker is interned per name
        check("marker.interned", MarkerFactory.getMarker("M1") == m1);
        // detached markers are fresh instances each call
        Marker d1 = MarkerFactory.getDetachedMarker("M1");
        Marker d2 = MarkerFactory.getDetachedMarker("M1");
        check("marker.detached.notsame.interned", d1 != m1);
        check("marker.detached.distinct", d1 != d2);
        eq("marker.detached.name", d1.getName(), "M1");

        Marker parent = MarkerFactory.getDetachedMarker("PARENT");
        Marker child = MarkerFactory.getDetachedMarker("CHILD");
        check("marker.no.refs.before", !parent.hasReferences());
        parent.add(child);
        check("marker.has.refs", parent.hasReferences());
        check("marker.has.children", parent.hasChildren());
        check("marker.contains.child", parent.contains(child));
        check("marker.contains.byname", parent.contains("CHILD"));
        check("marker.not.contains.other", !parent.contains("OTHER"));
        Iterator<Marker> it = parent.iterator();
        check("marker.iter.hasnext", it.hasNext());
        eq("marker.iter.value", it.next().getName(), "CHILD");
        check("marker.remove", parent.remove(child));
        check("marker.contains.after.remove", !parent.contains(child));
        check("marker.no.refs.after", !parent.hasReferences());
    }

    // ---------------------------------------------------------------- logback Level enum
    static void groupLogbackLevel() {
        eq("level.info.str", Level.INFO.levelStr, "INFO");
        eq("level.error.str", Level.ERROR.levelStr, "ERROR");
        eq("level.info.int", Level.INFO_INT, 20000);
        eq("level.error.int", Level.ERROR_INT, 40000);
        eq("level.trace.int", Level.TRACE_INT, 5000);
        eq("level.debug.int", Level.DEBUG_INT, 10000);
        eq("level.warn.int", Level.WARN_INT, 30000);
        eq("level.info.toInt", Level.INFO.toInt(), 20000);
        eq("level.field.levelInt", Level.WARN.levelInt, Level.WARN_INT);
        eq("level.off.max", Level.OFF_INT, Integer.MAX_VALUE);
        eq("level.all.min", Level.ALL_INT, Integer.MIN_VALUE);
        check("level.order", Level.ERROR_INT > Level.WARN_INT
                && Level.WARN_INT > Level.INFO_INT
                && Level.INFO_INT > Level.DEBUG_INT
                && Level.DEBUG_INT > Level.TRACE_INT);
        check("level.toLevel.warn", Level.toLevel("warn") == Level.WARN);
        check("level.toLevel.error.upper", Level.toLevel("ERROR") == Level.ERROR);
        check("level.toLevel.unknown.default.debug", Level.toLevel("bogus") == Level.DEBUG);
        check("level.toLevel.unknown.withdefault", Level.toLevel("bogus", Level.WARN) == Level.WARN);
        check("level.toLevel.int", Level.toLevel(Level.ERROR_INT) == Level.ERROR);
        check("level.valueOf", Level.valueOf("DEBUG") == Level.DEBUG);
        check("level.ge.true", Level.WARN.isGreaterOrEqual(Level.INFO));
        check("level.ge.self", Level.INFO.isGreaterOrEqual(Level.INFO));
        check("level.ge.false", !Level.INFO.isGreaterOrEqual(Level.WARN));
        check("level.convert.slf4j", Level.convertAnSLF4JLevel(org.slf4j.event.Level.INFO) == Level.INFO);
        eq("level.toString", Level.WARN.toString(), "WARN");
    }

    // ---------------------------------------------------------------- per-logger level filtering
    static void groupLevelFiltering(LoggerContext ctx) {
        ch.qos.logback.classic.Logger[] holder = new ch.qos.logback.classic.Logger[1];
        ListAppender<ILoggingEvent> la = capLogger(ctx, "filter.lvl", Level.WARN, holder);
        Logger log = holder[0];
        log.trace("t");
        log.debug("d");
        log.info("i");
        log.warn("w");
        log.error("e");
        eq("filter.warn.count", la.list.size(), 2);
        eq("filter.warn.first", la.list.get(0).getLevel(), Level.WARN);
        eq("filter.warn.second", la.list.get(1).getLevel(), Level.ERROR);

        la.list.clear();
        holder[0].setLevel(Level.OFF);
        log.error("nope");
        log.warn("nope");
        eq("filter.off.count", la.list.size(), 0);

        la.list.clear();
        holder[0].setLevel(Level.TRACE);
        log.trace("a");
        log.debug("b");
        log.info("c");
        log.warn("d");
        log.error("e");
        eq("filter.trace.count", la.list.size(), 5);
    }

    // ---------------------------------------------------------------- ThresholdFilter
    static void groupThresholdFilter(LoggerContext ctx) {
        ThresholdFilter tf = new ThresholdFilter();
        tf.setLevel("WARN");
        tf.start();

        ListAppender<ILoggingEvent> la = new ListAppender<>();
        la.setContext(ctx);
        la.addFilter(tf);
        la.start();
        ch.qos.logback.classic.Logger lg = ctx.getLogger("threshold.cap");
        lg.setLevel(Level.TRACE);
        lg.setAdditive(false);
        lg.detachAndStopAllAppenders();
        lg.addAppender(la);

        lg.trace("t");
        lg.debug("d");
        lg.info("i");
        lg.warn("w");
        lg.error("e");
        eq("threshold.count", la.list.size(), 2);
        eq("threshold.first", la.list.get(0).getLevel(), Level.WARN);
        eq("threshold.second", la.list.get(1).getLevel(), Level.ERROR);

        // unit-level: decide() returns NEUTRAL at/above threshold, DENY below
        ILoggingEvent warnEvt = la.list.get(0);
        ILoggingEvent errEvt = la.list.get(1);
        check("threshold.decide.warn.neutral", tf.decide(warnEvt) == FilterReply.NEUTRAL);
        check("threshold.decide.error.neutral", tf.decide(errEvt) == FilterReply.NEUTRAL);

        // build an INFO event through a no-filter capture to feed decide()
        ch.qos.logback.classic.Logger[] holder = new ch.qos.logback.classic.Logger[1];
        ListAppender<ILoggingEvent> plain = capLogger(ctx, "threshold.plain", Level.TRACE, holder);
        holder[0].info("info-evt");
        check("threshold.decide.info.deny", tf.decide(plain.list.get(0)) == FilterReply.DENY);
    }

    // ---------------------------------------------------------------- throwable proxy chain
    static void groupThrowableProxy(LoggerContext ctx) {
        ch.qos.logback.classic.Logger[] holder = new ch.qos.logback.classic.Logger[1];
        ListAppender<ILoggingEvent> la = capLogger(ctx, "throwable.cap", Level.DEBUG, holder);
        Logger log = holder[0];

        Exception cause = new IllegalStateException("root-cause");
        RuntimeException outer = new RuntimeException("outer", cause);

        // (String, Throwable) form: no argument array
        log.error("simple-fail", outer);
        ILoggingEvent e0 = la.list.get(0);
        eq("thr.simple.msg", e0.getFormattedMessage(), "simple-fail");
        check("thr.simple.noargs", e0.getArgumentArray() == null);
        IThrowableProxy p0 = e0.getThrowableProxy();
        check("thr.simple.proxy.present", p0 != null);
        eq("thr.simple.proxy.class", p0.getClassName(), "java.lang.RuntimeException");
        eq("thr.simple.proxy.msg", p0.getMessage(), "outer");
        check("thr.simple.proxy.stack", p0.getStackTraceElementProxyArray().length > 0);
        IThrowableProxy c0 = p0.getCause();
        check("thr.simple.cause.present", c0 != null);
        eq("thr.simple.cause.class", c0.getClassName(), "java.lang.IllegalStateException");
        eq("thr.simple.cause.msg", c0.getMessage(), "root-cause");
        check("thr.simple.cause.nocause", c0.getCause() == null);

        la.list.clear();
        // parameterized message with trailing throwable: arg consumed, throwable attached
        log.error("code={}", 7, outer);
        ILoggingEvent e1 = la.list.get(0);
        eq("thr.param.msg", e1.getFormattedMessage(), "code=7");
        eq("thr.param.argcount", e1.getArgumentArray().length, 1);
        eq("thr.param.argval", e1.getArgumentArray()[0], 7);
        eq("thr.param.proxy.class", e1.getThrowableProxy().getClassName(), "java.lang.RuntimeException");
    }

    // ---------------------------------------------------------------- PatternLayout / encoder rendering
    static void groupPatternRendering(LoggerContext ctx) {
        // %level / %logger / %msg
        eq("pat.basic",
                render(ctx, "alpha.beta.Gamma", "[%level] %logger - %msg%n", Level.DEBUG,
                        lg -> lg.info("hi")),
                "[INFO] alpha.beta.Gamma - hi\n");

        // %logger{0} -> right-most segment only
        eq("pat.logger0",
                render(ctx, "alpha.beta.Gamma", "%logger{0}|%msg%n", Level.DEBUG,
                        lg -> lg.warn("x")),
                "Gamma|x\n");

        // %-5level left-justified padding, exact per level
        eq("pat.pad.info",
                render(ctx, "pad.one", "%-5level|%msg%n", Level.TRACE, lg -> lg.info("m")),
                "INFO |m\n");
        eq("pat.pad.warn",
                render(ctx, "pad.two", "%-5level|%msg%n", Level.TRACE, lg -> lg.warn("m")),
                "WARN |m\n");
        eq("pat.pad.error",
                render(ctx, "pad.three", "%-5level|%msg%n", Level.TRACE, lg -> lg.error("m")),
                "ERROR|m\n");
        eq("pat.pad.debug",
                render(ctx, "pad.four", "%-5level|%msg%n", Level.TRACE, lg -> lg.debug("m")),
                "DEBUG|m\n");
        eq("pat.pad.trace",
                render(ctx, "pad.five", "%-5level|%msg%n", Level.TRACE, lg -> lg.trace("m")),
                "TRACE|m\n");

        // %thread on the main thread is deterministic
        eq("pat.thread",
                render(ctx, "thr.render", "[%thread] %msg%n", Level.DEBUG, lg -> lg.info("z")),
                "[main] z\n");

        // %msg uses the FORMATTED message (placeholders resolved)
        eq("pat.formatted",
                render(ctx, "fmt.render", "%msg%n", Level.DEBUG, lg -> lg.info("a={} b={}", 3, 4)),
                "a=3 b=4\n");

        // %X{key} pulls specific MDC entries; missing key renders empty
        String mdcOut = render(ctx, "mdc.render", "%X{reqId}=%X{user}|%msg%n", Level.DEBUG, lg -> {
            MDC.put("reqId", "r1");
            MDC.put("user", "bob");
            lg.info("payload");
            MDC.clear();
        });
        eq("pat.mdc.present", mdcOut, "r1=bob|payload\n");
        String mdcMissing = render(ctx, "mdc.render2", "[%X{nope}]%msg%n", Level.DEBUG,
                lg -> lg.info("p"));
        eq("pat.mdc.missing", mdcMissing, "[]p\n");

        // multiple events accumulate, with logger-level filtering applied through the encoder path
        eq("pat.multi.filtered",
                render(ctx, "multi.render", "%level:%msg%n", Level.WARN, lg -> {
                    lg.info("skip");
                    lg.warn("b");
                    lg.error("c");
                }),
                "WARN:b\nERROR:c\n");

        // %d{yyyy} date conversion: year digits then message
        String dated = render(ctx, "date.render", "%d{yyyy} %msg", Level.DEBUG, lg -> lg.info("dated"));
        check("pat.date.shape", dated.matches("\\d{4} dated"));

        // standalone PatternLayout.doLayout against a captured event
        ch.qos.logback.classic.Logger[] holder = new ch.qos.logback.classic.Logger[1];
        ListAppender<ILoggingEvent> la = capLogger(ctx, "layout.src", Level.DEBUG, holder);
        holder[0].error("layout-msg");
        PatternLayout pl = new PatternLayout();
        pl.setContext(ctx);
        pl.setPattern("[%level] %logger{0} >> %msg");
        pl.start();
        check("pat.layout.started", pl.isStarted());
        eq("pat.layout.doLayout", pl.doLayout(la.list.get(0)), "[ERROR] src >> layout-msg");
        pl.stop();

        // encoder reports its configured pattern back
        PatternLayoutEncoder enc = new PatternLayoutEncoder();
        enc.setContext(ctx);
        enc.setPattern("%msg%n");
        eq("pat.encoder.getPattern", enc.getPattern(), "%msg%n");
    }

    // ---------------------------------------------------------------- effective level inheritance
    static void groupEffectiveLevel(LoggerContext ctx) {
        ch.qos.logback.classic.Logger parent = ctx.getLogger("inh.parent");
        parent.setLevel(Level.INFO);
        ch.qos.logback.classic.Logger child = ctx.getLogger("inh.parent.child");
        // child has no explicit level -> inherits parent's
        check("inh.child.level.null", child.getLevel() == null);
        check("inh.child.effective.info", child.getEffectiveLevel() == Level.INFO);
        check("inh.child.info.on", child.isInfoEnabled());
        check("inh.child.debug.off", !child.isDebugEnabled());

        child.setLevel(Level.DEBUG);
        check("inh.child.effective.debug", child.getEffectiveLevel() == Level.DEBUG);
        check("inh.child.debug.on.after", child.isDebugEnabled());
        check("inh.parent.unchanged", parent.getEffectiveLevel() == Level.INFO);

        // reverting to null re-inherits
        child.setLevel(null);
        check("inh.child.reinherit", child.getEffectiveLevel() == Level.INFO);
    }

    // ---------------------------------------------------------------- appender attach/detach lifecycle
    static void groupAppenderManagement(LoggerContext ctx) {
        ListAppender<ILoggingEvent> a = new ListAppender<>();
        a.setContext(ctx);
        a.setName("mgmt-appender");
        check("mgmt.notstarted.before", !a.isStarted());
        a.start();
        check("mgmt.started.after", a.isStarted());
        eq("mgmt.name", a.getName(), "mgmt-appender");

        ch.qos.logback.classic.Logger lg = ctx.getLogger("appmgmt");
        lg.setLevel(Level.DEBUG);
        lg.setAdditive(false);
        lg.detachAndStopAllAppenders();
        check("mgmt.additive.false", !lg.isAdditive());
        lg.addAppender(a);
        check("mgmt.attached", lg.isAttached(a));
        check("mgmt.getByName", lg.getAppender("mgmt-appender") == a);
        Iterator<?> it = lg.iteratorForAppenders();
        check("mgmt.iter.hasnext", it.hasNext());
        check("mgmt.detach", lg.detachAppender(a));
        check("mgmt.detached", !lg.isAttached(a));
        check("mgmt.getByName.gone", lg.getAppender("mgmt-appender") == null);
        a.stop();
        check("mgmt.stopped", !a.isStarted());

        // context identity / naming
        ch.qos.logback.classic.Logger root = ctx.getLogger(Logger.ROOT_LOGGER_NAME);
        eq("mgmt.root.name", root.getName(), "ROOT");
        check("mgmt.exists", ctx.exists("appmgmt") == lg);
        check("mgmt.exists.absent", ctx.exists("never.created.logger.xyz") == null);
    }
}
