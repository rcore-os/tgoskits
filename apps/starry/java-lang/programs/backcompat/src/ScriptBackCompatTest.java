import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.io.Reader;
import java.io.StringReader;
import java.nio.charset.Charset;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeSet;

import org.junit.Test;

import bsh.EvalError;
import bsh.Interpreter;
import bsh.NameSpace;
import bsh.Primitive;
import bsh.TargetError;

/**
 * Java-8 backward-compatibility proof carpet for the SCRIPT library group:
 *   - bsh (BeanShell 2.0b6), org.apache-extras.beanshell:bsh:2.0b6
 *
 * Every test is fully DETERMINISTIC: fixed scripts produce fixed values,
 * and we assert exact results + exact runtime types. No randomness, no time,
 * no filesystem, no locale-sensitive formatting. Source is --release 8 only
 * (lambdas/streams/Optional/java.time are permitted but BeanShell scripts here
 * stay on the Java-8 expression surface). Must behave identically on
 * JDK 17/21/23/25.
 */
public class ScriptBackCompatTest {

    /* ----------------------------------------------------------------- *
     * Arithmetic, operators, precedence, numeric promotion             *
     * ----------------------------------------------------------------- */

    @Test
    public void testIntegerArithmeticPrecedence() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("2 + 3 * 4 - 1");
        assertEquals(Integer.class, r.getClass());
        assertEquals(13, ((Integer) r).intValue());
    }

    @Test
    public void testIntegerParentheses() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(20, ((Integer) new Interpreter().eval("(2 + 3) * 4")).intValue());
        assertEquals(20, bsh.eval("(2 + 3) * 4"));
    }

    @Test
    public void testIntegerDivisionAndModulo() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(3, ((Integer) bsh.eval("10 / 3")).intValue());
        assertEquals(1, ((Integer) bsh.eval("10 % 3")).intValue());
        assertEquals(-1, ((Integer) bsh.eval("-10 % 3")).intValue());
    }

    @Test
    public void testDoubleArithmeticReturnsDouble() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("3.0 / 2.0");
        assertEquals(Double.class, r.getClass());
        assertEquals(1.5d, (Double) r, 0.0d);
    }

    @Test
    public void testMixedIntDoublePromotion() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("7 / 2.0");
        assertEquals(Double.class, r.getClass());
        assertEquals(3.5d, (Double) r, 0.0d);
    }

    @Test
    public void testLongLiteralAndArithmetic() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("1000000L * 1000000L");
        assertEquals(Long.class, r.getClass());
        assertEquals(1000000000000L, ((Long) r).longValue());
    }

    @Test
    public void testFloatLiteral() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("1.5f + 0.5f");
        assertEquals(Float.class, r.getClass());
        assertEquals(2.0f, (Float) r, 0.0f);
    }

    @Test
    public void testUnaryNegationAndIncrement() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(-5, ((Integer) bsh.eval("-(2 + 3)")).intValue());
        Object r = bsh.eval("int x = 5; x++; x++; x");
        assertEquals(7, ((Integer) r).intValue());
        Object r2 = bsh.eval("int y = 5; ++y");
        assertEquals(6, ((Integer) r2).intValue());
    }

    @Test
    public void testCompoundAssignmentOperators() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("int a = 10; a += 5; a -= 3; a *= 2; a /= 4; a");
        assertEquals(6, ((Integer) r).intValue());
    }

    @Test
    public void testBitwiseOperators() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(12, ((Integer) bsh.eval("0xF & 0xC")).intValue());
        assertEquals(15, ((Integer) bsh.eval("0xC | 0x3")).intValue());
        assertEquals(6, ((Integer) bsh.eval("0x5 ^ 0x3")).intValue());
        assertEquals(8, ((Integer) bsh.eval("1 << 3")).intValue());
        assertEquals(4, ((Integer) bsh.eval("32 >> 3")).intValue());
        assertEquals(-1, ((Integer) bsh.eval("~0")).intValue());
    }

    @Test
    public void testUnsignedRightShift() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("-8 >>> 28");
        assertEquals(15, ((Integer) r).intValue());
    }

    /* ----------------------------------------------------------------- *
     * Booleans, comparisons, logical short-circuit, ternary            *
     * ----------------------------------------------------------------- */

    @Test
    public void testBooleanLogicalOperators() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(Boolean.FALSE, bsh.eval("true && false"));
        assertEquals(Boolean.TRUE, bsh.eval("true || false"));
        assertEquals(Boolean.TRUE, bsh.eval("!false"));
        assertEquals(Boolean.TRUE, bsh.eval("(2 > 1) && (3 >= 3)"));
    }

    @Test
    public void testComparisonOperators() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(Boolean.TRUE, bsh.eval("5 > 3"));
        assertEquals(Boolean.FALSE, bsh.eval("5 < 3"));
        assertEquals(Boolean.TRUE, bsh.eval("5 == 5"));
        assertEquals(Boolean.TRUE, bsh.eval("5 != 6"));
        assertEquals(Boolean.TRUE, bsh.eval("5 <= 5"));
        assertEquals(Boolean.TRUE, bsh.eval("5 >= 5"));
    }

    @Test
    public void testTernaryOperator() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(100, ((Integer) bsh.eval("(7 > 3) ? 100 : 200")).intValue());
        assertEquals(200, ((Integer) bsh.eval("(1 > 3) ? 100 : 200")).intValue());
    }

    @Test
    public void testShortCircuitAvoidsSideEffect() throws EvalError {
        Interpreter bsh = new Interpreter();
        // If short-circuit works, the right operand (which would set flag) never runs.
        Object r = bsh.eval("boolean flag = false; boolean result = (false && (flag = true)); flag");
        assertEquals(Boolean.FALSE, r);
        Object r2 = bsh.eval("boolean flag2 = false; boolean result2 = (true || (flag2 = true)); flag2");
        assertEquals(Boolean.FALSE, r2);
    }

    /* ----------------------------------------------------------------- *
     * Control flow: if/else, while, for, do-while, switch              *
     * ----------------------------------------------------------------- */

    @Test
    public void testIfElseChain() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int classify(int n) {"
          + "  if (n < 0) return -1;"
          + "  else if (n == 0) return 0;"
          + "  else return 1;"
          + "}"
          + "classify(-5) * 100 + classify(0) * 10 + classify(7)";
        Object r = bsh.eval(script);
        assertEquals(-100 + 0 + 1, ((Integer) r).intValue());
    }

    @Test
    public void testWhileLoopSum() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int sum = 0; int i = 1;"
          + "while (i <= 100) { sum += i; i++; }"
          + "sum";
        Object r = bsh.eval(script);
        assertEquals(5050, ((Integer) r).intValue());
    }

    @Test
    public void testForLoopProduct() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "long product = 1;"
          + "for (int i = 1; i <= 6; i++) { product *= i; }"
          + "product";
        Object r = bsh.eval(script);
        assertEquals(720L, ((Long) r).longValue());
    }

    @Test
    public void testDoWhileLoop() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int count = 0; int x = 10;"
          + "do { count++; x -= 3; } while (x > 0);"
          + "count";
        Object r = bsh.eval(script);
        assertEquals(4, ((Integer) r).intValue());
    }

    @Test
    public void testForLoopWithBreakAndContinue() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int sum = 0;"
          + "for (int i = 0; i < 20; i++) {"
          + "  if (i % 2 == 0) continue;"   // skip evens
          + "  if (i > 10) break;"          // stop after 10
          + "  sum += i;"                   // 1+3+5+7+9
          + "}"
          + "sum";
        Object r = bsh.eval(script);
        assertEquals(1 + 3 + 5 + 7 + 9, ((Integer) r).intValue());
    }

    @Test
    public void testNestedLoops() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int total = 0;"
          + "for (int i = 1; i <= 3; i++) {"
          + "  for (int j = 1; j <= 3; j++) {"
          + "    total += i * j;"
          + "  }"
          + "}"
          + "total";
        // (1+2+3)*(1+2+3) = 36
        assertEquals(36, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testSwitchStatement() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "String dayName(int d) {"
          + "  switch (d) {"
          + "    case 1: return \"Mon\";"
          + "    case 2: return \"Tue\";"
          + "    case 3: return \"Wed\";"
          + "    default: return \"?\";"
          + "  }"
          + "}"
          + "dayName(2) + dayName(3) + dayName(9)";
        assertEquals("TueWed?", bsh.eval(script));
    }

    /* ----------------------------------------------------------------- *
     * Method / function definitions, recursion, overload-like dispatch *
     * ----------------------------------------------------------------- */

    @Test
    public void testRecursiveFactorial() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("int factorial(int n) { if (n <= 1) return 1; return n * factorial(n - 1); }");
        assertEquals(1, ((Integer) bsh.eval("factorial(0)")).intValue());
        assertEquals(120, ((Integer) bsh.eval("factorial(5)")).intValue());
        assertEquals(3628800, ((Integer) bsh.eval("factorial(10)")).intValue());
    }

    @Test
    public void testRecursiveFibonacci() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("int fib(int n) { if (n < 2) return n; return fib(n-1) + fib(n-2); }");
        assertEquals(0, ((Integer) bsh.eval("fib(0)")).intValue());
        assertEquals(1, ((Integer) bsh.eval("fib(1)")).intValue());
        assertEquals(55, ((Integer) bsh.eval("fib(10)")).intValue());
        assertEquals(6765, ((Integer) bsh.eval("fib(20)")).intValue());
    }

    @Test
    public void testMethodWithMultipleArguments() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("int gcd(int a, int b) { while (b != 0) { int t = b; b = a % b; a = t; } return a; }");
        assertEquals(6, ((Integer) bsh.eval("gcd(48, 18)")).intValue());
        assertEquals(1, ((Integer) bsh.eval("gcd(17, 5)")).intValue());
        assertEquals(12, ((Integer) bsh.eval("gcd(36, 24)")).intValue());
    }

    @Test
    public void testLooselyTypedReturnValue() throws EvalError {
        Interpreter bsh = new Interpreter();
        // BeanShell allows "loose" (untyped) functions; here we return a String.
        bsh.eval("greet(name) { return \"Hello, \" + name + \"!\"; }");
        assertEquals("Hello, World!", bsh.eval("greet(\"World\")"));
    }

    @Test
    public void testMethodCallingMethod() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("int square(int x) { return x * x; }");
        bsh.eval("int sumOfSquares(int a, int b) { return square(a) + square(b); }");
        assertEquals(25, ((Integer) bsh.eval("sumOfSquares(3, 4)")).intValue());
    }

    /* ----------------------------------------------------------------- *
     * Variable get / set across the Java/BeanShell boundary            *
     * ----------------------------------------------------------------- */

    @Test
    public void testSetGetIntPrimitive() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.set("a", 7);
        bsh.set("b", 35);
        Object r = bsh.eval("a + b");
        assertEquals(42, ((Integer) r).intValue());
        assertEquals(Integer.valueOf(7), bsh.get("a"));
    }

    @Test
    public void testSetGetVariousPrimitiveOverloads() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.set("li", 5L);
        bsh.set("dd", 2.5d);
        bsh.set("ff", 1.5f);
        bsh.set("bb", true);
        assertEquals(Long.valueOf(5L), bsh.get("li"));
        assertEquals(Double.valueOf(2.5d), bsh.get("dd"));
        assertEquals(Float.valueOf(1.5f), bsh.get("ff"));
        assertEquals(Boolean.TRUE, bsh.get("bb"));
        // Use them in a mixed expression.
        assertEquals(7.5d, (Double) bsh.eval("li + dd"), 0.0d);
    }

    @Test
    public void testSetGetObject() throws EvalError {
        Interpreter bsh = new Interpreter();
        List<String> list = new ArrayList<String>(Arrays.asList("alpha", "beta", "gamma"));
        bsh.set("data", list);
        Object size = bsh.eval("data.size()");
        assertEquals(3, ((Integer) size).intValue());
        assertEquals("beta", bsh.eval("data.get(1)"));
        // Mutate the same object instance from inside the script.
        bsh.eval("data.add(\"delta\")");
        assertEquals(4, list.size());
        assertEquals("delta", list.get(3));
    }

    @Test
    public void testGetVariableSetInsideScript() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("computed = 6 * 7;");
        Object r = bsh.get("computed");
        assertEquals(Integer.valueOf(42), r);
    }

    @Test
    public void testUnsetVariable() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.set("temp", 99);
        assertEquals(Integer.valueOf(99), bsh.get("temp"));
        bsh.unset("temp");
        // After unset the variable resolves to null via Interpreter.get.
        assertNull(bsh.get("temp"));
    }

    @Test
    public void testGetUndefinedVariableReturnsNull() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertNull(bsh.get("neverDefined"));
    }

    /* ----------------------------------------------------------------- *
     * Calling into the Java standard library                            *
     * ----------------------------------------------------------------- */

    @Test
    public void testCallMathStaticMethods() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(12, ((Integer) bsh.eval("Math.max(7, 12)")).intValue());
        assertEquals(7, ((Integer) bsh.eval("Math.min(7, 12)")).intValue());
        assertEquals(5.0d, (Double) bsh.eval("Math.sqrt(25.0)"), 0.0d);
        assertEquals(8.0d, (Double) bsh.eval("Math.pow(2.0, 3.0)"), 0.0d);
        assertEquals(5L, ((Long) bsh.eval("Math.round(4.6)")).longValue());
        assertEquals(7, ((Integer) bsh.eval("Math.abs(-7)")).intValue());
    }

    @Test
    public void testMathConstants() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(Math.PI, (Double) bsh.eval("Math.PI"), 0.0d);
        assertEquals(Math.E, (Double) bsh.eval("Math.E"), 0.0d);
    }

    @Test
    public void testStringMethods() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(5, ((Integer) bsh.eval("\"hello\".length()")).intValue());
        assertEquals("HELLO", bsh.eval("\"hello\".toUpperCase()"));
        assertEquals("ell", bsh.eval("\"hello\".substring(1, 4)"));
        assertEquals(Boolean.TRUE, bsh.eval("\"hello world\".contains(\"world\")"));
        assertEquals("h-e-l-l-o", bsh.eval("\"hello\".replace(\"\", \"-\").substring(1, 10)"));
        assertEquals(Integer.valueOf(2), bsh.eval("\"hello\".indexOf(\"l\")"));
        assertEquals("dlrow", bsh.eval("new StringBuilder(\"world\").reverse().toString()"));
    }

    @Test
    public void testIntegerBoxingAndParsing() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(255, ((Integer) bsh.eval("Integer.parseInt(\"FF\", 16)")).intValue());
        assertEquals(123, ((Integer) bsh.eval("Integer.parseInt(\"123\")")).intValue());
        assertEquals("101", bsh.eval("Integer.toBinaryString(5)"));
        assertEquals(Integer.valueOf(Integer.MAX_VALUE), bsh.eval("Integer.MAX_VALUE"));
    }

    @Test
    public void testCallJavaUtilArrayList() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "import java.util.ArrayList;"
          + "list = new ArrayList();"
          + "list.add(\"x\"); list.add(\"y\"); list.add(\"z\");"
          + "list.size() * 100 + list.indexOf(\"y\")";
        assertEquals(301, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testCallJavaUtilHashMap() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "import java.util.HashMap;"
          + "m = new HashMap();"
          + "m.put(\"one\", 1); m.put(\"two\", 2); m.put(\"three\", 3);"
          + "m.get(\"one\") + m.get(\"two\") + m.get(\"three\")";
        assertEquals(6, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testCallJavaUtilCollectionsSort() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "import java.util.ArrayList;"
          + "import java.util.Collections;"
          + "l = new ArrayList();"
          + "l.add(3); l.add(1); l.add(2);"
          + "Collections.sort(l);"
          + "l.get(0) * 100 + l.get(1) * 10 + l.get(2)";
        assertEquals(123, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testStringBuilderInScript() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "sb = new StringBuilder();"
          + "for (int i = 0; i < 5; i++) { sb.append(i); }"
          + "sb.toString()";
        assertEquals("01234", bsh.eval(script));
    }

    /* ----------------------------------------------------------------- *
     * Arrays                                                            *
     * ----------------------------------------------------------------- */

    @Test
    public void testIntArrayCreationAndAccess() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int[] arr = new int[]{10, 20, 30, 40};"
          + "int s = 0;"
          + "for (int i = 0; i < arr.length; i++) { s += arr[i]; }"
          + "s";
        assertEquals(100, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testArrayReturnedToJava() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("new int[]{1, 2, 3}");
        assertTrue(r instanceof int[]);
        assertArrayEquals(new int[] {1, 2, 3}, (int[]) r);
    }

    @Test
    public void testStringArrayAndForEach() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "String[] words = new String[]{\"a\", \"bb\", \"ccc\"};"
          + "int total = 0;"
          + "for (String w : words) { total += w.length(); }"
          + "total";
        assertEquals(6, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testTwoDimensionalArray() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int[][] m = new int[][]{{1, 2}, {3, 4}};"
          + "m[0][0] + m[0][1] + m[1][0] + m[1][1]";
        assertEquals(10, ((Integer) bsh.eval(script)).intValue());
    }

    /* ----------------------------------------------------------------- *
     * Returning typed values; null and void                            *
     * ----------------------------------------------------------------- */

    @Test
    public void testExplicitReturnString() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object r = bsh.eval("return \"deterministic\";");
        assertEquals(String.class, r.getClass());
        assertEquals("deterministic", r);
    }

    @Test
    public void testCharLiteralAndConversion() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object c = bsh.eval("char ch = 'A'; ch");
        assertEquals(Character.class, c.getClass());
        assertEquals(Character.valueOf('A'), c);
        Object asInt = bsh.eval("(int) 'A'");
        assertEquals(65, ((Integer) asInt).intValue());
    }

    @Test
    public void testByteAndShortTypes() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object b = bsh.eval("byte bv = 100; bv");
        assertEquals(Byte.class, b.getClass());
        assertEquals(Byte.valueOf((byte) 100), b);
        Object s = bsh.eval("short sv = 1000; sv");
        assertEquals(Short.class, s.getClass());
        assertEquals(Short.valueOf((short) 1000), s);
    }

    @Test
    public void testNullLiteralUnwrapsToJavaNull() throws EvalError {
        Interpreter bsh = new Interpreter();
        // Interpreter.eval unwraps Primitive.NULL to a Java null result.
        Object r = bsh.eval("null");
        assertNull(r);
    }

    @Test
    public void testAssignedNullReadsBackAsJavaNull() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("Object obj = null;");
        // Reading a null-valued variable through Interpreter.get yields Java null.
        assertNull(bsh.get("obj"));
    }

    @Test
    public void testVoidStatementUnwrapsToJavaNull() throws EvalError {
        Interpreter bsh = new Interpreter();
        // A statement with no value (e.g. a declaration) evaluates to VOID,
        // which Interpreter.eval unwraps to a Java null result.
        Object r = bsh.eval("int unusedX = 5;");
        assertNull(r);
    }

    /* ----------------------------------------------------------------- *
     * Type checks and casting                                          *
     * ----------------------------------------------------------------- */

    @Test
    public void testInstanceofOperator() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(Boolean.TRUE, bsh.eval("\"text\" instanceof String"));
        assertEquals(Boolean.TRUE, bsh.eval("new java.util.ArrayList() instanceof java.util.List"));
        assertEquals(Boolean.FALSE, bsh.eval("\"text\" instanceof Integer"));
    }

    @Test
    public void testExplicitCasts() throws EvalError {
        Interpreter bsh = new Interpreter();
        Object d2i = bsh.eval("(int) 3.99");
        assertEquals(3, ((Integer) d2i).intValue());
        Object i2d = bsh.eval("(double) 5");
        assertEquals(5.0d, (Double) i2d, 0.0d);
        Object i2l = bsh.eval("(long) 42");
        assertEquals(42L, ((Long) i2l).longValue());
    }

    /* ----------------------------------------------------------------- *
     * NameSpace API                                                    *
     * ----------------------------------------------------------------- */

    @Test
    public void testNameSpaceGetVariable() throws Exception {
        Interpreter bsh = new Interpreter();
        bsh.eval("alpha = 11; beta = 22;");
        NameSpace ns = bsh.getNameSpace();
        // NameSpace.getVariable returns a bsh.Primitive wrapper for primitives;
        // Primitive.unwrap yields the plain Java boxed value.
        assertEquals(Integer.valueOf(11), Primitive.unwrap(ns.getVariable("alpha")));
        assertEquals(Integer.valueOf(22), Primitive.unwrap(ns.getVariable("beta")));
    }

    @Test
    public void testNameSpaceSetVariableVisibleInScript() throws Exception {
        Interpreter bsh = new Interpreter();
        NameSpace ns = bsh.getNameSpace();
        ns.setVariable("injected", Integer.valueOf(50), false);
        Object r = bsh.eval("injected + 5");
        assertEquals(55, ((Integer) r).intValue());
    }

    @Test
    public void testNameSpaceVariableNamesContainsDefined() throws Exception {
        Interpreter bsh = new Interpreter();
        bsh.eval("foo = 1; bar = 2; baz = 3;");
        NameSpace ns = bsh.getNameSpace();
        TreeSet<String> names = new TreeSet<String>(Arrays.asList(ns.getVariableNames()));
        assertTrue(names.contains("foo"));
        assertTrue(names.contains("bar"));
        assertTrue(names.contains("baz"));
    }

    @Test
    public void testNameSpaceClearRemovesVariables() throws Exception {
        Interpreter bsh = new Interpreter();
        bsh.eval("toClear = 123;");
        NameSpace ns = bsh.getNameSpace();
        assertEquals(Integer.valueOf(123), Primitive.unwrap(ns.getVariable("toClear")));
        ns.clear();
        // After clear the variable is gone; getVariable returns Primitive.VOID for unknown.
        assertSame(Primitive.VOID, ns.getVariable("toClear"));
    }

    @Test
    public void testNamedNameSpaceName() {
        NameSpace ns = new NameSpace((NameSpace) null, "myScope");
        assertEquals("myScope", ns.getName());
    }

    @Test
    public void testEvalWithExplicitNameSpaceIsolation() throws Exception {
        Interpreter bsh = new Interpreter();
        NameSpace scopeA = new NameSpace(bsh.getNameSpace(), "scopeA");
        bsh.eval("localVal = 7;", scopeA);
        // The variable lives in scopeA, not the global namespace.
        assertEquals(Integer.valueOf(7), Primitive.unwrap(scopeA.getVariable("localVal")));
        assertNull(bsh.get("localVal"));
    }

    /* ----------------------------------------------------------------- *
     * eval overloads: Reader-based evaluation                          *
     * ----------------------------------------------------------------- */

    @Test
    public void testEvalFromReader() throws EvalError {
        Interpreter bsh = new Interpreter();
        // The Reader-based parser requires a statement terminator (return ...;)
        // for the final value-producing statement at EOF.
        Reader reader = new StringReader(
            "int total = 0; for (int i = 1; i <= 10; i++) total += i; return total;");
        Object r = bsh.eval(reader);
        assertEquals(55, ((Integer) r).intValue());
    }

    @Test
    public void testEvalFromInputStreamReader() throws EvalError {
        Interpreter bsh = new Interpreter();
        byte[] bytes = "return 6 * 9 + 6;".getBytes(Charset.forName("UTF-8"));
        Reader reader = new InputStreamReader(new ByteArrayInputStream(bytes), Charset.forName("UTF-8"));
        Object r = bsh.eval(reader);
        assertEquals(60, ((Integer) r).intValue());
    }

    @Test
    public void testEvalWithSourceFileInfo() throws EvalError {
        Interpreter bsh = new Interpreter();
        Reader reader = new StringReader("return 3 + 4;");
        Object r = bsh.eval(reader, bsh.getNameSpace(), "synthetic.bsh");
        assertEquals(7, ((Integer) r).intValue());
    }

    /* ----------------------------------------------------------------- *
     * Error handling: EvalError, ParseError, TargetError               *
     * ----------------------------------------------------------------- */

    @Test
    public void testEvalErrorOnUndefinedMethod() {
        Interpreter bsh = new Interpreter();
        try {
            bsh.eval("thisMethodDoesNotExistAnywhere(1, 2, 3)");
            fail("expected EvalError for undefined method");
        } catch (EvalError e) {
            assertNotNull(e.getMessage());
        }
    }

    @Test
    public void testEvalErrorOnSyntaxError() {
        Interpreter bsh = new Interpreter();
        try {
            bsh.eval("int x = ;;; @@@ broken");
            fail("expected EvalError for malformed script");
        } catch (EvalError e) {
            assertNotNull(e.getMessage());
        }
    }

    @Test
    public void testTargetErrorWrapsThrownException() {
        Interpreter bsh = new Interpreter();
        try {
            bsh.eval("Integer.parseInt(\"not-a-number\")");
            fail("expected an EvalError wrapping NumberFormatException");
        } catch (EvalError e) {
            // BeanShell wraps target exceptions in TargetError (a subclass of EvalError).
            assertTrue(e instanceof TargetError);
            Throwable target = ((TargetError) e).getTarget();
            assertNotNull(target);
            assertEquals(NumberFormatException.class, target.getClass());
        }
    }

    @Test
    public void testTargetErrorForExplicitThrow() {
        Interpreter bsh = new Interpreter();
        try {
            bsh.eval("throw new IllegalStateException(\"boom\");");
            fail("expected EvalError wrapping IllegalStateException");
        } catch (EvalError e) {
            assertTrue(e instanceof TargetError);
            Throwable target = ((TargetError) e).getTarget();
            assertEquals(IllegalStateException.class, target.getClass());
            assertEquals("boom", target.getMessage());
        }
    }

    @Test
    public void testCaughtExceptionInsideScript() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "String result;"
          + "try {"
          + "  Integer.parseInt(\"xyz\");"
          + "  result = \"no-error\";"
          + "} catch (NumberFormatException e) {"
          + "  result = \"caught\";"
          + "}"
          + "result";
        assertEquals("caught", bsh.eval(script));
    }

    @Test
    public void testFinallyBlockRuns() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "StringBuilder sb = new StringBuilder();"
          + "try { sb.append(\"t\"); throw new RuntimeException(\"x\"); }"
          + "catch (RuntimeException e) { sb.append(\"c\"); }"
          + "finally { sb.append(\"f\"); }"
          + "sb.toString()";
        assertEquals("tcf", bsh.eval(script));
    }

    /* ----------------------------------------------------------------- *
     * getInterface: scripted implementation of a Java interface        *
     * ----------------------------------------------------------------- */

    @Test
    public void testGetInterfaceComparator() throws EvalError {
        Interpreter bsh = new Interpreter();
        // Define a compare(o1,o2) method, then expose this namespace as a Comparator.
        bsh.eval("compare(o1, o2) { return o1 - o2; }");
        @SuppressWarnings("unchecked")
        Comparator<Integer> cmp = (Comparator<Integer>) bsh.getInterface(Comparator.class);
        assertNotNull(cmp);
        assertTrue(cmp.compare(3, 7) < 0);
        assertTrue(cmp.compare(9, 2) > 0);
        assertEquals(0, cmp.compare(5, 5));

        // Use the scripted comparator to drive a real java.util.Collections.sort.
        List<Integer> nums = new ArrayList<Integer>(Arrays.asList(5, 1, 4, 2, 3));
        java.util.Collections.sort(nums, cmp);
        assertEquals(Arrays.asList(1, 2, 3, 4, 5), nums);
    }

    @Test
    public void testGetInterfaceRunnable() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.set("counter", new int[] {0});
        bsh.eval("run() { counter[0] = counter[0] + 10; }");
        Runnable runnable = (Runnable) bsh.getInterface(Runnable.class);
        runnable.run();
        runnable.run();
        int[] counter = (int[]) bsh.get("counter");
        assertEquals(20, counter[0]);
    }

    /* ----------------------------------------------------------------- *
     * Output capture: setOut + printing from a script                  *
     * ----------------------------------------------------------------- */

    @Test
    public void testScriptPrintCapturedViaSetOut() throws EvalError {
        Interpreter bsh = new Interpreter();
        ByteArrayOutputStream baos = new ByteArrayOutputStream();
        PrintStream ps = new PrintStream(baos, true);
        bsh.setOut(ps);
        // BeanShell's print() goes to the configured out stream.
        bsh.eval("print(\"line-1\"); print(\"line-2\");");
        ps.flush();
        String out = new String(baos.toByteArray(), Charset.forName("UTF-8"));
        assertTrue(out.contains("line-1"));
        assertTrue(out.contains("line-2"));
    }

    /* ----------------------------------------------------------------- *
     * Interpreter state persistence across multiple eval calls         *
     * ----------------------------------------------------------------- */

    @Test
    public void testStatePersistsAcrossEvalCalls() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("accumulator = 0;");
        bsh.eval("accumulator += 10;");
        bsh.eval("accumulator += 20;");
        bsh.eval("accumulator += 12;");
        Object r = bsh.eval("accumulator");
        assertEquals(42, ((Integer) r).intValue());
    }

    @Test
    public void testMethodDefinedOnceCalledMany() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.eval("int triple(int n) { return n * 3; }");
        int sum = 0;
        for (int i = 1; i <= 5; i++) {
            Object r = bsh.eval("triple(" + i + ")");
            sum += ((Integer) r).intValue();
        }
        // 3*(1+2+3+4+5) = 45
        assertEquals(45, sum);
    }

    @Test
    public void testIndependentInterpretersAreIsolated() throws EvalError {
        Interpreter a = new Interpreter();
        Interpreter b = new Interpreter();
        a.eval("shared = 1;");
        b.eval("shared = 2;");
        assertEquals(Integer.valueOf(1), a.get("shared"));
        assertEquals(Integer.valueOf(2), b.get("shared"));
    }

    /* ----------------------------------------------------------------- *
     * BeanShell-specific conveniences                                  *
     * ----------------------------------------------------------------- */

    @Test
    public void testLooseTypingReassignmentAcrossTypes() throws EvalError {
        Interpreter bsh = new Interpreter();
        // Untyped variable can be reassigned to different types.
        bsh.eval("x = 5;");
        assertEquals(Integer.valueOf(5), bsh.get("x"));
        bsh.eval("x = \"now a string\";");
        assertEquals("now a string", bsh.get("x"));
        bsh.eval("x = 3.14;");
        assertEquals(Double.valueOf(3.14d), bsh.get("x"));
    }

    @Test
    public void testStrictJavaModeRejectsUntypedVar() {
        Interpreter bsh = new Interpreter();
        bsh.setStrictJava(true);
        assertTrue(bsh.getStrictJava());
        try {
            // In strict-Java mode, an untyped declaration is illegal.
            bsh.eval("undeclaredLoose = 5;");
            fail("expected EvalError under strictJava for untyped variable");
        } catch (EvalError e) {
            assertNotNull(e.getMessage());
        }
    }

    @Test
    public void testStrictJavaModeAcceptsTypedDeclarations() throws EvalError {
        Interpreter bsh = new Interpreter();
        bsh.setStrictJava(true);
        Object r = bsh.eval("int total = 0; for (int i = 1; i <= 4; i++) { total += i; } return total;");
        assertEquals(10, ((Integer) r).intValue());
    }

    @Test
    public void testScriptedMapBuildingReturnedToJava() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "import java.util.HashMap;"
          + "HashMap m = new HashMap();"
          + "for (int i = 1; i <= 3; i++) { m.put(\"k\" + i, i * i); }"
          + "m";
        Object r = bsh.eval(script);
        assertTrue(r instanceof Map);
        @SuppressWarnings("unchecked")
        Map<String, Object> m = (Map<String, Object>) r;
        assertEquals(3, m.size());
        assertEquals(Integer.valueOf(1), m.get("k1"));
        assertEquals(Integer.valueOf(4), m.get("k2"));
        assertEquals(Integer.valueOf(9), m.get("k3"));
    }

    @Test
    public void testScriptUsesJavaObjectPassedIn() throws EvalError {
        Interpreter bsh = new Interpreter();
        Map<String, Integer> prices = new HashMap<String, Integer>();
        prices.put("apple", 3);
        prices.put("pear", 5);
        prices.put("kiwi", 2);
        bsh.set("prices", prices);
        Object total = bsh.eval("prices.get(\"apple\") + prices.get(\"pear\") + prices.get(\"kiwi\")");
        assertEquals(10, ((Integer) total).intValue());
    }

    @Test
    public void testNestedBlockScoping() throws EvalError {
        Interpreter bsh = new Interpreter();
        String script =
            "int outer = 100;"
          + "{ int inner = 23; outer += inner; }"
          + "outer";
        assertEquals(123, ((Integer) bsh.eval(script)).intValue());
    }

    @Test
    public void testStringConcatenationWithNumbers() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals("value=42", bsh.eval("\"value=\" + (40 + 2)"));
        assertEquals("3.14 is pi", bsh.eval("3.14 + \" is pi\""));
        assertEquals("true!", bsh.eval("(1 < 2) + \"!\""));
    }

    @Test
    public void testDeterministicAcrossRepeatedRuns() throws EvalError {
        // Same script, fresh interpreter, must always equal the same value.
        String script =
            "long acc = 0;"
          + "for (int i = 1; i <= 1000; i++) { acc += (i * i); }"
          + "acc";
        long expected = 333833500L; // sum of squares 1..1000
        for (int run = 0; run < 5; run++) {
            Interpreter bsh = new Interpreter();
            Object r = bsh.eval(script);
            assertEquals(expected, ((Long) r).longValue());
        }
    }

    @Test
    public void testBooleanWrapperEqualityDeterminism() throws EvalError {
        Interpreter bsh = new Interpreter();
        assertEquals(Boolean.TRUE, bsh.eval("Boolean.valueOf(\"true\")"));
        assertEquals(Boolean.FALSE, bsh.eval("Boolean.valueOf(\"false\")"));
        assertFalse((Boolean) bsh.eval("Boolean.parseBoolean(\"nope\")"));
    }
}
