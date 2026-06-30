import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;

import java.io.Serializable;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.CopyOnWriteArrayList;

import org.apache.logging.log4j.Level;
import org.apache.logging.log4j.Marker;
import org.apache.logging.log4j.MarkerManager;
import org.apache.logging.log4j.ThreadContext;
import org.apache.logging.log4j.core.Filter;
import org.apache.logging.log4j.core.Layout;
import org.apache.logging.log4j.core.LogEvent;
import org.apache.logging.log4j.core.Logger;
import org.apache.logging.log4j.core.LoggerContext;
import org.apache.logging.log4j.core.appender.AbstractAppender;
import org.apache.logging.log4j.core.config.Configuration;
import org.apache.logging.log4j.core.config.LoggerConfig;
import org.apache.logging.log4j.core.config.NullConfiguration;
import org.apache.logging.log4j.message.MessageFormatMessage;
import org.apache.logging.log4j.message.ParameterizedMessage;
import org.apache.logging.log4j.message.StringFormattedMessage;
import org.apache.logging.log4j.spi.StandardLevel;
import org.junit.After;
import org.junit.AfterClass;
import org.junit.Before;
import org.junit.BeforeClass;
import org.junit.Test;

/**
 * Java-8 backward-compatibility proof carpet, Logging group.
 *
 * <p>Libraries under test (Java-8-era, pure-Java):
 * <ul>
 *   <li>org.apache.logging.log4j:log4j-api:2.17.1</li>
 *   <li>org.apache.logging.log4j:log4j-core:2.17.1</li>
 * </ul>
 *
 * <p>Compiled with {@code --release 8} (bytecode major 52); uses only Java 8 source
 * features and only Log4j2 2.x API that is stable across JDK 17/21/23/25.
 *
 * <p>DETERMINISM STRATEGY: every test drives a private {@link LoggerContext} built on a
 * {@link NullConfiguration}. A custom in-memory appender ({@link CapturingAppender})
 * records ONLY the formatted message string (and, for the structured tests, the level /
 * marker name / MDC snapshot / logger name / thrown class name) -- never a timestamp,
 * thread id or any other non-deterministic field. Fixed inputs therefore always yield a
 * fixed, order-stable list of captured strings which we assert exactly.
 */
public class LoggingBackCompatTest {

    // ------------------------------------------------------------------
    // In-memory capturing appender. Captures only deterministic fields.
    // ------------------------------------------------------------------
    static final class CapturingAppender extends AbstractAppender {

        final List<String> messages = new CopyOnWriteArrayList<>();
        final List<Level> levels = new CopyOnWriteArrayList<>();
        final List<String> markerNames = new CopyOnWriteArrayList<>();
        final List<Map<String, String>> contexts = new CopyOnWriteArrayList<>();
        final List<String> loggerNames = new CopyOnWriteArrayList<>();
        final List<String> thrownClasses = new CopyOnWriteArrayList<>();

        @SuppressWarnings("deprecation")
        CapturingAppender(final String name) {
            super(name, (Filter) null, (Layout<? extends Serializable>) null);
        }

        @Override
        public void append(final LogEvent event) {
            messages.add(event.getMessage().getFormattedMessage());
            levels.add(event.getLevel());
            final Marker m = event.getMarker();
            markerNames.add(m == null ? null : m.getName());
            contexts.add(event.getContextData().toMap());
            loggerNames.add(event.getLoggerName());
            final Throwable t = event.getThrown();
            thrownClasses.add(t == null ? null : t.getClass().getName());
        }

        void reset() {
            messages.clear();
            levels.clear();
            markerNames.clear();
            contexts.clear();
            loggerNames.clear();
            thrownClasses.clear();
        }

        List<String> messages() {
            return new ArrayList<>(messages);
        }
    }

    // A single private context + appender + logger config, created ONCE. Re-creating
    // the LoggerConfig per test makes the context's cached Logger route to a stale
    // config (verified), so we instead build the pipeline once and reset the capture
    // buffers + level threshold between tests for full isolation and determinism.
    private static LoggerContext ctx;
    private static CapturingAppender sharedAppender;

    private CapturingAppender appender; // alias of sharedAppender, per-test convenience
    private Logger logger;              // bound to logger name "test.logger"

    private static final String LOGGER_NAME = "test.logger";

    @BeforeClass
    public static void bootContext() {
        // A standalone context that does NOT touch any classpath log4j2.xml.
        ctx = new LoggerContext("backcompat-logging-test");
        ctx.start(new NullConfiguration());

        final Configuration config = ctx.getConfiguration();
        sharedAppender = new CapturingAppender("cap");
        sharedAppender.start();
        config.addAppender(sharedAppender);

        final LoggerConfig lc = new LoggerConfig(LOGGER_NAME, Level.TRACE, false);
        lc.addAppender(sharedAppender, Level.TRACE, null);
        config.addLogger(LOGGER_NAME, lc);
        ctx.updateLoggers();
    }

    @AfterClass
    public static void shutdownContext() {
        if (ctx != null) {
            ctx.stop();
            ctx = null;
        }
        sharedAppender = null;
    }

    @Before
    public void wireAppender() {
        // Always start from a clean MDC/NDC so tests are independent.
        ThreadContext.clearAll();
        appender = sharedAppender;
        appender.reset();

        // Reset threshold to TRACE so every test starts wide open.
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.TRACE);
        ctx.updateLoggers();

        logger = ctx.getLogger(LOGGER_NAME);
    }

    @After
    public void unwireAppender() {
        // Restore wide-open threshold and clear capture + MDC for the next test.
        if (ctx != null) {
            ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.TRACE);
            ctx.updateLoggers();
        }
        if (sharedAppender != null) {
            sharedAppender.reset();
        }
        ThreadContext.clearAll();
    }

    // ==================================================================
    // 1. Basic level logging -> exact captured message list
    // ==================================================================
    @Test
    public void infoLogsSingleMessage() {
        logger.info("hello world");
        assertEquals(Collections.singletonList("hello world"), appender.messages());
        assertEquals(1, appender.levels.size());
        assertEquals(Level.INFO, appender.levels.get(0));
        assertEquals(LOGGER_NAME, appender.loggerNames.get(0));
    }

    @Test
    public void allSixLevelsCapturedInOrder() {
        logger.trace("t");
        logger.debug("d");
        logger.info("i");
        logger.warn("w");
        logger.error("e");
        logger.fatal("f");
        assertEquals(Arrays.asList("t", "d", "i", "w", "e", "f"), appender.messages());
        assertEquals(
                Arrays.asList(Level.TRACE, Level.DEBUG, Level.INFO, Level.WARN, Level.ERROR, Level.FATAL),
                new ArrayList<>(appender.levels));
    }

    @Test
    public void emptyAppenderWhenNothingLogged() {
        assertTrue(appender.messages().isEmpty());
    }

    @Test
    public void logViaGenericLevelMethod() {
        logger.log(Level.WARN, "generic-level");
        assertEquals(Collections.singletonList("generic-level"), appender.messages());
        assertEquals(Level.WARN, appender.levels.get(0));
    }

    // ==================================================================
    // 2. Level threshold filtering (deterministic suppression)
    // ==================================================================
    @Test
    public void levelThresholdSuppressesLowerLevels() {
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.WARN);
        ctx.updateLoggers();

        logger.trace("t");   // suppressed
        logger.debug("d");   // suppressed
        logger.info("i");    // suppressed
        logger.warn("w");    // kept
        logger.error("e");   // kept
        assertEquals(Arrays.asList("w", "e"), appender.messages());
    }

    @Test
    public void offLevelSuppressesEverything() {
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.OFF);
        ctx.updateLoggers();
        logger.fatal("nope");
        logger.error("nope2");
        assertTrue(appender.messages().isEmpty());
    }

    @Test
    public void isEnabledMatchesThreshold() {
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.INFO);
        ctx.updateLoggers();
        assertFalse(logger.isTraceEnabled());
        assertFalse(logger.isDebugEnabled());
        assertTrue(logger.isInfoEnabled());
        assertTrue(logger.isWarnEnabled());
        assertTrue(logger.isErrorEnabled());
    }

    // ==================================================================
    // 3. Parameterized ({}) messages
    // ==================================================================
    @Test
    public void parameterizedSinglePlaceholder() {
        logger.info("user={}", "alice");
        assertEquals(Collections.singletonList("user=alice"), appender.messages());
    }

    @Test
    public void parameterizedTwoPlaceholders() {
        logger.info("{} + {} = {}", 1, 2, 3);
        assertEquals(Collections.singletonList("1 + 2 = 3"), appender.messages());
    }

    @Test
    public void parameterizedTooFewArgsLeavesPlaceholder() {
        logger.info("a={} b={}", "x");
        assertEquals(Collections.singletonList("a=x b={}"), appender.messages());
    }

    @Test
    public void parameterizedExtraArgsIgnoredInText() {
        logger.info("only={}", "first", "second");
        assertEquals(Collections.singletonList("only=first"), appender.messages());
    }

    @Test
    public void escapedPlaceholderIsLiteral() {
        // \{} is an escaped placeholder -> rendered literally as {}
        logger.info("literal=\\{} value={}", "v");
        assertEquals(Collections.singletonList("literal={} value=v"), appender.messages());
    }

    @Test
    public void parameterizedNullArgumentRendersNull() {
        logger.info("x={}", (Object) null);
        assertEquals(Collections.singletonList("x=null"), appender.messages());
    }

    // ==================================================================
    // 4. Message object types (ParameterizedMessage / String.format / MessageFormat)
    // ==================================================================
    @Test
    public void parameterizedMessageObject() {
        ParameterizedMessage pm = new ParameterizedMessage("{}-{}", "a", "b");
        assertEquals("a-b", pm.getFormattedMessage());
        assertEquals("{}-{}", pm.getFormat());
        assertArrayEquals(new Object[] {"a", "b"}, pm.getParameters());
        logger.info(pm);
        assertEquals(Collections.singletonList("a-b"), appender.messages());
    }

    @Test
    public void stringFormattedMessageObject() {
        StringFormattedMessage sm =
                new StringFormattedMessage(Locale.ROOT, "pi=%.2f n=%d", 3.14159, 7);
        assertEquals("pi=3.14 n=7", sm.getFormattedMessage());
        logger.info(sm);
        assertEquals(Collections.singletonList("pi=3.14 n=7"), appender.messages());
    }

    @Test
    public void messageFormatMessageObject() {
        MessageFormatMessage mf =
                new MessageFormatMessage(Locale.ROOT, "{0} loves {1}", "Tom", "Jerry");
        assertEquals("Tom loves Jerry", mf.getFormattedMessage());
        logger.info(mf);
        assertEquals(Collections.singletonList("Tom loves Jerry"), appender.messages());
    }

    @Test
    public void printfFormattedHelper() {
        logger.printf(Level.INFO, "[%05d]", 42);
        assertEquals(Collections.singletonList("[00042]"), appender.messages());
    }

    // ==================================================================
    // 5. Throwable attachment captured deterministically (class name only)
    // ==================================================================
    @Test
    public void throwableAttachedToEvent() {
        IllegalStateException ex = new IllegalStateException("boom");
        logger.error("failed", ex);
        assertEquals(Collections.singletonList("failed"), appender.messages());
        assertEquals("java.lang.IllegalStateException", appender.thrownClasses.get(0));
    }

    @Test
    public void noThrowableYieldsNull() {
        logger.info("clean");
        assertNull(appender.thrownClasses.get(0));
    }

    // ==================================================================
    // 6. Markers
    // ==================================================================
    @Test
    public void markerAttachedToEvent() {
        Marker sql = MarkerManager.getMarker("SQL");
        logger.info(sql, "select 1");
        assertEquals(Collections.singletonList("select 1"), appender.messages());
        assertEquals("SQL", appender.markerNames.get(0));
    }

    @Test
    public void noMarkerYieldsNull() {
        logger.info("plain");
        assertNull(appender.markerNames.get(0));
    }

    @Test
    public void markerManagerReturnsSameInstance() {
        Marker a = MarkerManager.getMarker("UNIQUE_MK_A");
        Marker b = MarkerManager.getMarker("UNIQUE_MK_A");
        assertSame(a, b);
        assertTrue(MarkerManager.exists("UNIQUE_MK_A"));
        assertEquals("UNIQUE_MK_A", a.getName());
    }

    @Test
    public void markerParentHierarchyInstanceOf() {
        Marker parent = MarkerManager.getMarker("PARENT_MK");
        Marker child = MarkerManager.getMarker("CHILD_MK").setParents(parent);
        assertTrue(child.hasParents());
        assertTrue(child.isInstanceOf(parent));
        assertTrue(child.isInstanceOf("PARENT_MK"));
        assertFalse(parent.isInstanceOf(child));
        assertArrayEquals(new Marker[] {parent}, child.getParents());
    }

    @Test
    public void markerAddAndRemoveParent() {
        Marker p1 = MarkerManager.getMarker("ADD_P1");
        Marker p2 = MarkerManager.getMarker("ADD_P2");
        Marker c = MarkerManager.getMarker("ADD_C").addParents(p1).addParents(p2);
        assertTrue(c.isInstanceOf(p1));
        assertTrue(c.isInstanceOf(p2));
        assertTrue(c.remove(p1));
        assertFalse(c.isInstanceOf(p1));
        assertTrue(c.isInstanceOf(p2));
    }

    // ==================================================================
    // 7. ThreadContext (MDC) captured deterministically
    // ==================================================================
    @Test
    public void mdcCapturedOnEvent() {
        ThreadContext.put("requestId", "R-001");
        ThreadContext.put("user", "bob");
        logger.info("with-mdc");
        Map<String, String> snap = appender.contexts.get(0);
        assertEquals("R-001", snap.get("requestId"));
        assertEquals("bob", snap.get("user"));
        assertEquals(2, snap.size());
    }

    @Test
    public void mdcRemovedKeyNotPresent() {
        ThreadContext.put("k1", "v1");
        ThreadContext.put("k2", "v2");
        ThreadContext.remove("k1");
        logger.info("after-remove");
        Map<String, String> snap = appender.contexts.get(0);
        assertFalse(snap.containsKey("k1"));
        assertEquals("v2", snap.get("k2"));
        assertEquals(1, snap.size());
    }

    @Test
    public void mdcEmptyByDefault() {
        logger.info("no-mdc");
        assertTrue(appender.contexts.get(0).isEmpty());
    }

    @Test
    public void threadContextApiBehaviour() {
        assertTrue(ThreadContext.isEmpty());
        ThreadContext.put("a", "1");
        assertTrue(ThreadContext.containsKey("a"));
        assertEquals("1", ThreadContext.get("a"));
        assertFalse(ThreadContext.isEmpty());

        ThreadContext.putIfNull("a", "2"); // already set, no change
        assertEquals("1", ThreadContext.get("a"));
        ThreadContext.putIfNull("b", "3"); // not set, applies
        assertEquals("3", ThreadContext.get("b"));

        Map<String, String> bulk = new HashMap<>();
        bulk.put("c", "4");
        bulk.put("d", "5");
        ThreadContext.putAll(bulk);
        assertEquals("4", ThreadContext.get("c"));
        assertEquals("5", ThreadContext.get("d"));

        Map<String, String> ctxMap = ThreadContext.getImmutableContext();
        assertEquals(4, ctxMap.size());

        ThreadContext.removeAll(Arrays.asList("a", "b"));
        assertFalse(ThreadContext.containsKey("a"));
        assertFalse(ThreadContext.containsKey("b"));

        ThreadContext.clearMap();
        assertTrue(ThreadContext.isEmpty());
    }

    @Test
    public void threadContextStackNdc() {
        assertEquals(0, ThreadContext.getDepth());
        ThreadContext.push("frame-1");
        ThreadContext.push("frame-2");
        assertEquals(2, ThreadContext.getDepth());
        assertEquals("frame-2", ThreadContext.peek());
        assertEquals("frame-2", ThreadContext.pop());
        assertEquals(1, ThreadContext.getDepth());
        assertEquals("frame-1", ThreadContext.peek());
        ThreadContext.clearStack();
        assertEquals(0, ThreadContext.getDepth());
    }

    @Test
    public void threadContextStackParameterizedPush() {
        ThreadContext.push("op {} on {}", "INSERT", "users");
        assertEquals("op INSERT on users", ThreadContext.peek());
        ThreadContext.clearStack();
    }

    // ==================================================================
    // 8. Level value object semantics
    // ==================================================================
    @Test
    public void levelIntOrdering() {
        // Lower intLevel == higher severity in Log4j2's standard ladder.
        assertEquals(100, Level.FATAL.intLevel());
        assertEquals(200, Level.ERROR.intLevel());
        assertEquals(300, Level.WARN.intLevel());
        assertEquals(400, Level.INFO.intLevel());
        assertEquals(500, Level.DEBUG.intLevel());
        assertEquals(600, Level.TRACE.intLevel());
        assertTrue(Level.ERROR.intLevel() < Level.WARN.intLevel());
        assertTrue(Level.WARN.intLevel() < Level.INFO.intLevel());
        assertEquals(Integer.MAX_VALUE, Level.ALL.intLevel());
        assertEquals(0, Level.OFF.intLevel());
    }

    @Test
    public void levelSpecificityComparisons() {
        // ERROR is more specific (higher severity) than INFO.
        assertTrue(Level.ERROR.isMoreSpecificThan(Level.INFO));
        assertTrue(Level.DEBUG.isLessSpecificThan(Level.INFO));
        assertFalse(Level.INFO.isMoreSpecificThan(Level.ERROR));
        assertTrue(Level.WARN.isInRange(Level.ERROR, Level.DEBUG));
        assertFalse(Level.TRACE.isInRange(Level.ERROR, Level.INFO));
    }

    @Test
    public void levelNameAndLookup() {
        assertEquals("INFO", Level.INFO.name());
        assertEquals("INFO", Level.INFO.toString());
        assertSame(Level.INFO, Level.getLevel("INFO"));
        assertSame(Level.WARN, Level.toLevel("WARN"));
        assertSame(Level.DEBUG, Level.toLevel("does-not-exist", Level.DEBUG));
        assertNull(Level.getLevel("NO_SUCH_LEVEL"));
        assertSame(Level.INFO, Level.valueOf("INFO"));
    }

    @Test
    public void levelStandardEnumMapping() {
        assertEquals(StandardLevel.ERROR, Level.ERROR.getStandardLevel());
        assertEquals(StandardLevel.INFO, Level.INFO.getStandardLevel());
    }

    @Test
    public void levelValuesContainsBuiltins() {
        Level[] vals = Level.values();
        List<Level> list = Arrays.asList(vals);
        assertTrue(list.contains(Level.OFF));
        assertTrue(list.contains(Level.FATAL));
        assertTrue(list.contains(Level.ERROR));
        assertTrue(list.contains(Level.WARN));
        assertTrue(list.contains(Level.INFO));
        assertTrue(list.contains(Level.DEBUG));
        assertTrue(list.contains(Level.TRACE));
        assertTrue(list.contains(Level.ALL));
    }

    @Test
    public void customLevelForName() {
        Level verbose = Level.forName("VERBOSE_CUSTOM", 550);
        assertEquals("VERBOSE_CUSTOM", verbose.name());
        assertEquals(550, verbose.intLevel());
        // forName returns the same registered instance on repeat.
        assertSame(verbose, Level.forName("VERBOSE_CUSTOM", 550));
        assertSame(verbose, Level.getLevel("VERBOSE_CUSTOM"));

        // It must be usable as an ordinary log level.
        logger.log(verbose, "verbose-msg");
        assertEquals(Collections.singletonList("verbose-msg"), appender.messages());
        assertSame(verbose, appender.levels.get(0));
    }

    @Test
    public void levelComparable() {
        assertTrue(Level.ERROR.compareTo(Level.WARN) < 0);
        assertEquals(0, Level.INFO.compareTo(Level.INFO));
        assertEquals(Level.INFO, Level.INFO);
    }

    // ==================================================================
    // 9. Logger identity / naming / additivity
    // ==================================================================
    @Test
    public void loggerNameMatches() {
        assertEquals(LOGGER_NAME, logger.getName());
        assertFalse(logger.isAdditive());
    }

    @Test
    public void sameNameReturnsSameLoggerInstance() {
        Logger a = ctx.getLogger(LOGGER_NAME);
        Logger b = ctx.getLogger(LOGGER_NAME);
        assertSame(a, b);
    }

    @Test
    public void loggerEffectiveLevelReflectsConfig() {
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.WARN);
        ctx.updateLoggers();
        assertEquals(Level.WARN, ctx.getLogger(LOGGER_NAME).getLevel());
    }

    // ==================================================================
    // 10. Supplier / lazy logging (Java 8 lambda) evaluated only when enabled
    // ==================================================================
    @Test
    public void supplierEvaluatedWhenEnabled() {
        final int[] calls = {0};
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.INFO);
        ctx.updateLoggers();
        logger.info(() -> {
            calls[0]++;
            return "lazy-on";
        });
        assertEquals(1, calls[0]);
        assertEquals(Collections.singletonList("lazy-on"), appender.messages());
    }

    @Test
    public void supplierNotEvaluatedWhenDisabled() {
        final int[] calls = {0};
        ctx.getConfiguration().getLoggerConfig(LOGGER_NAME).setLevel(Level.ERROR);
        ctx.updateLoggers();
        logger.debug(() -> {
            calls[0]++;
            return "lazy-off";
        });
        assertEquals(0, calls[0]);
        assertTrue(appender.messages().isEmpty());
    }

    // ==================================================================
    // 11. Appender lifecycle / capture isolation
    // ==================================================================
    @Test
    public void appenderNameAndStartedState() {
        assertEquals("cap", appender.getName());
        assertTrue(appender.isStarted());
    }

    @Test
    public void resetClearsCapture() {
        logger.info("one");
        assertEquals(1, appender.messages().size());
        appender.reset();
        assertTrue(appender.messages().isEmpty());
        logger.info("two");
        assertEquals(Collections.singletonList("two"), appender.messages());
    }

    // ==================================================================
    // 12. Combined deterministic scenario (level + marker + mdc + params)
    // ==================================================================
    @Test
    public void combinedDeterministicScenario() {
        Marker audit = MarkerManager.getMarker("AUDIT");
        ThreadContext.put("txn", "T-77");
        logger.warn(audit, "rows affected: {}", 5);
        logger.error(audit, "rollback {}", "yes");

        assertEquals(Arrays.asList("rows affected: 5", "rollback yes"), appender.messages());
        assertEquals(Arrays.asList(Level.WARN, Level.ERROR), new ArrayList<>(appender.levels));
        assertEquals(Arrays.asList("AUDIT", "AUDIT"), new ArrayList<>(appender.markerNames));
        for (Map<String, String> snap : appender.contexts) {
            assertEquals("T-77", snap.get("txn"));
        }
        assertNotNull(audit);
    }
}
