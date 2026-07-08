import static org.junit.Assert.*;

import com.google.gson.ExclusionStrategy;
import com.google.gson.FieldAttributes;
import com.google.gson.FieldNamingPolicy;
import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonDeserializationContext;
import com.google.gson.JsonDeserializer;
import com.google.gson.JsonElement;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;
import com.google.gson.JsonParseException;
import com.google.gson.JsonParser;
import com.google.gson.JsonPrimitive;
import com.google.gson.JsonSerializationContext;
import com.google.gson.JsonSerializer;
import com.google.gson.JsonSyntaxException;
import com.google.gson.LongSerializationPolicy;
import com.google.gson.TypeAdapter;
import com.google.gson.annotations.Expose;
import com.google.gson.annotations.SerializedName;
import com.google.gson.annotations.Since;
import com.google.gson.reflect.TypeToken;
import com.google.gson.stream.JsonReader;
import com.google.gson.stream.JsonToken;
import com.google.gson.stream.JsonWriter;

import org.junit.Test;

import java.io.IOException;
import java.io.StringReader;
import java.io.StringWriter;
import java.lang.reflect.Type;
import java.math.BigDecimal;
import java.math.BigInteger;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Comprehensive, deterministic JUnit4 carpet for the Json library group (Gson 2.10.1).
 *
 * Java-8-only source (compiled with --release 8, bytecode 52). Runs identically on
 * JDK 17/21/23/25. No Nashorn/JAXB/CORBA. All inputs are fixed literals and all
 * assertions check exact JSON strings or parsed field values, so the suite is
 * fully deterministic (Gson preserves declared field order for reflective POJOs and
 * insertion order for LinkedHashMap / JsonObject).
 */
public class JsonBackCompatTest {

    // ----------------------------------------------------------------
    // Fixed model classes (declared field order => stable serialization)
    // ----------------------------------------------------------------

    enum Color { RED, GREEN, BLUE }

    static class Point {
        int x;
        int y;
        Point() {}
        Point(int x, int y) { this.x = x; this.y = y; }
    }

    static class Person {
        String name;
        int age;
        boolean active;
        Person() {}
        Person(String name, int age, boolean active) {
            this.name = name; this.age = age; this.active = active;
        }
    }

    static class Nested {
        String label;
        Point origin;
        List<Integer> values;
        Nested() {}
        Nested(String label, Point origin, List<Integer> values) {
            this.label = label; this.origin = origin; this.values = values;
        }
    }

    static class Renamed {
        @SerializedName("first_name")
        String firstName;
        @SerializedName(value = "id", alternate = {"identifier", "key"})
        int id;
        Renamed() {}
        Renamed(String firstName, int id) { this.firstName = firstName; this.id = id; }
    }

    static class WithEnum {
        String name;
        Color color;
        WithEnum() {}
        WithEnum(String name, Color color) { this.name = name; this.color = color; }
    }

    static class ExposeModel {
        @Expose
        String shown;
        @Expose(serialize = false, deserialize = true)
        String inOnly;
        @Expose(serialize = true, deserialize = false)
        String outOnly;
        String hidden; // no @Expose
        ExposeModel() {}
        ExposeModel(String shown, String inOnly, String outOnly, String hidden) {
            this.shown = shown; this.inOnly = inOnly; this.outOnly = outOnly; this.hidden = hidden;
        }
    }

    static class VersionedModel {
        String stable;
        @Since(2.0)
        String newer;
        VersionedModel() {}
        VersionedModel(String stable, String newer) { this.stable = stable; this.newer = newer; }
    }

    static class TransientModel {
        int kept;
        transient int dropped;
        static int staticField = 99;
        TransientModel() {}
        TransientModel(int kept, int dropped) { this.kept = kept; this.dropped = dropped; }
    }

    static class NullableModel {
        String present;
        String absent; // null
        NullableModel() {}
        NullableModel(String present, String absent) { this.present = present; this.absent = absent; }
    }

    // A type with a registered custom (de)serializer
    static class Money {
        long cents;
        Money() {}
        Money(long cents) { this.cents = cents; }
    }

    // ----------------------------------------------------------------
    // 1. Basic serialization of a flat POJO -> exact JSON
    // ----------------------------------------------------------------

    @Test
    public void testSerializeFlatPojoExactJson() {
        Gson gson = new Gson();
        Person p = new Person("Ada", 36, true);
        assertEquals("{\"name\":\"Ada\",\"age\":36,\"active\":true}", gson.toJson(p));
    }

    @Test
    public void testSerializePointExactJson() {
        Gson gson = new Gson();
        assertEquals("{\"x\":3,\"y\":-7}", gson.toJson(new Point(3, -7)));
    }

    // ----------------------------------------------------------------
    // 2. Deserialization of a flat POJO
    // ----------------------------------------------------------------

    @Test
    public void testDeserializeFlatPojo() {
        Gson gson = new Gson();
        Person p = gson.fromJson("{\"name\":\"Grace\",\"age\":40,\"active\":false}", Person.class);
        assertEquals("Grace", p.name);
        assertEquals(40, p.age);
        assertFalse(p.active);
    }

    @Test
    public void testDeserializeIgnoresUnknownFields() {
        Gson gson = new Gson();
        Person p = gson.fromJson("{\"name\":\"Lin\",\"age\":21,\"active\":true,\"extra\":42}", Person.class);
        assertEquals("Lin", p.name);
        assertEquals(21, p.age);
        assertTrue(p.active);
    }

    @Test
    public void testRoundTripPojo() {
        Gson gson = new Gson();
        Person original = new Person("Knuth", 86, true);
        String json = gson.toJson(original);
        Person back = gson.fromJson(json, Person.class);
        assertEquals(original.name, back.name);
        assertEquals(original.age, back.age);
        assertEquals(original.active, back.active);
    }

    // ----------------------------------------------------------------
    // 3. Nested objects + lists
    // ----------------------------------------------------------------

    @Test
    public void testSerializeNestedExactJson() {
        Gson gson = new Gson();
        Nested n = new Nested("box", new Point(1, 2), Arrays.asList(10, 20, 30));
        assertEquals("{\"label\":\"box\",\"origin\":{\"x\":1,\"y\":2},\"values\":[10,20,30]}", gson.toJson(n));
    }

    @Test
    public void testDeserializeNested() {
        Gson gson = new Gson();
        Nested n = gson.fromJson(
                "{\"label\":\"cell\",\"origin\":{\"x\":5,\"y\":6},\"values\":[7,8]}", Nested.class);
        assertEquals("cell", n.label);
        assertEquals(5, n.origin.x);
        assertEquals(6, n.origin.y);
        assertEquals(Arrays.asList(7, 8), n.values);
    }

    @Test
    public void testSerializeListOfPojos() {
        Gson gson = new Gson();
        List<Point> pts = Arrays.asList(new Point(0, 0), new Point(1, 1));
        assertEquals("[{\"x\":0,\"y\":0},{\"x\":1,\"y\":1}]", gson.toJson(pts));
    }

    // ----------------------------------------------------------------
    // 4. Maps (LinkedHashMap => stable key order)
    // ----------------------------------------------------------------

    @Test
    public void testSerializeMapExactJson() {
        Gson gson = new Gson();
        Map<String, Integer> m = new LinkedHashMap<String, Integer>();
        m.put("a", 1);
        m.put("b", 2);
        m.put("c", 3);
        assertEquals("{\"a\":1,\"b\":2,\"c\":3}", gson.toJson(m));
    }

    @Test
    public void testDeserializeMapWithTypeToken() {
        Gson gson = new Gson();
        Type t = new TypeToken<Map<String, Integer>>() {}.getType();
        Map<String, Integer> m = gson.fromJson("{\"x\":10,\"y\":20}", t);
        assertEquals(Integer.valueOf(10), m.get("x"));
        assertEquals(Integer.valueOf(20), m.get("y"));
        assertEquals(2, m.size());
    }

    // ----------------------------------------------------------------
    // 5. Enums
    // ----------------------------------------------------------------

    @Test
    public void testSerializeEnum() {
        Gson gson = new Gson();
        assertEquals("\"GREEN\"", gson.toJson(Color.GREEN));
    }

    @Test
    public void testSerializeEnumInsidePojo() {
        Gson gson = new Gson();
        WithEnum w = new WithEnum("flag", Color.BLUE);
        assertEquals("{\"name\":\"flag\",\"color\":\"BLUE\"}", gson.toJson(w));
    }

    @Test
    public void testDeserializeEnum() {
        Gson gson = new Gson();
        WithEnum w = gson.fromJson("{\"name\":\"q\",\"color\":\"RED\"}", WithEnum.class);
        assertEquals("q", w.name);
        assertEquals(Color.RED, w.color);
    }

    @Test
    public void testDeserializeEnumDirect() {
        Gson gson = new Gson();
        assertEquals(Color.BLUE, gson.fromJson("\"BLUE\"", Color.class));
    }

    // ----------------------------------------------------------------
    // 6. @SerializedName (rename + alternate)
    // ----------------------------------------------------------------

    @Test
    public void testSerializedNameSerialize() {
        Gson gson = new Gson();
        Renamed r = new Renamed("Joan", 7);
        assertEquals("{\"first_name\":\"Joan\",\"id\":7}", gson.toJson(r));
    }

    @Test
    public void testSerializedNameDeserialize() {
        Gson gson = new Gson();
        Renamed r = gson.fromJson("{\"first_name\":\"Edsger\",\"id\":99}", Renamed.class);
        assertEquals("Edsger", r.firstName);
        assertEquals(99, r.id);
    }

    @Test
    public void testSerializedNameAlternate() {
        Gson gson = new Gson();
        Renamed r1 = gson.fromJson("{\"first_name\":\"A\",\"identifier\":11}", Renamed.class);
        assertEquals(11, r1.id);
        Renamed r2 = gson.fromJson("{\"first_name\":\"B\",\"key\":22}", Renamed.class);
        assertEquals(22, r2.id);
    }

    // ----------------------------------------------------------------
    // 7. Pretty printing
    // ----------------------------------------------------------------

    @Test
    public void testPrettyPrinting() {
        Gson gson = new GsonBuilder().setPrettyPrinting().create();
        String json = gson.toJson(new Point(8, 9));
        // Gson pretty printer uses 2-space indent and \n newlines.
        assertEquals("{\n  \"x\": 8,\n  \"y\": 9\n}", json);
    }

    @Test
    public void testPrettyPrintingNested() {
        Gson gson = new GsonBuilder().setPrettyPrinting().create();
        Nested n = new Nested("p", new Point(1, 2), Arrays.asList(3, 4));
        String expected =
                "{\n" +
                "  \"label\": \"p\",\n" +
                "  \"origin\": {\n" +
                "    \"x\": 1,\n" +
                "    \"y\": 2\n" +
                "  },\n" +
                "  \"values\": [\n" +
                "    3,\n" +
                "    4\n" +
                "  ]\n" +
                "}";
        assertEquals(expected, gson.toJson(n));
    }

    // ----------------------------------------------------------------
    // 8. serializeNulls
    // ----------------------------------------------------------------

    @Test
    public void testNullsOmittedByDefault() {
        Gson gson = new Gson();
        assertEquals("{\"present\":\"v\"}", gson.toJson(new NullableModel("v", null)));
    }

    @Test
    public void testSerializeNullsEnabled() {
        Gson gson = new GsonBuilder().serializeNulls().create();
        assertEquals("{\"present\":\"v\",\"absent\":null}", gson.toJson(new NullableModel("v", null)));
    }

    // ----------------------------------------------------------------
    // 9. transient / static fields excluded
    // ----------------------------------------------------------------

    @Test
    public void testTransientAndStaticExcluded() {
        Gson gson = new Gson();
        assertEquals("{\"kept\":5}", gson.toJson(new TransientModel(5, 6)));
    }

    @Test
    public void testExcludeFieldsWithModifiers() {
        // explicitly exclude transient + static (default Gson behaviour, verified)
        Gson gson = new GsonBuilder()
                .excludeFieldsWithModifiers(java.lang.reflect.Modifier.TRANSIENT,
                                            java.lang.reflect.Modifier.STATIC)
                .create();
        assertEquals("{\"kept\":12}", gson.toJson(new TransientModel(12, 34)));
    }

    // ----------------------------------------------------------------
    // 10. FieldNamingPolicy
    // ----------------------------------------------------------------

    static class CamelModel {
        String firstName = "v1";
        int totalCount = 5;
    }

    @Test
    public void testFieldNamingLowerCaseWithUnderscores() {
        Gson gson = new GsonBuilder()
                .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
                .create();
        assertEquals("{\"first_name\":\"v1\",\"total_count\":5}", gson.toJson(new CamelModel()));
    }

    @Test
    public void testFieldNamingUpperCamelCase() {
        Gson gson = new GsonBuilder()
                .setFieldNamingPolicy(FieldNamingPolicy.UPPER_CAMEL_CASE)
                .create();
        assertEquals("{\"FirstName\":\"v1\",\"TotalCount\":5}", gson.toJson(new CamelModel()));
    }

    @Test
    public void testFieldNamingUpperCamelCaseWithSpaces() {
        Gson gson = new GsonBuilder()
                .setFieldNamingPolicy(FieldNamingPolicy.UPPER_CAMEL_CASE_WITH_SPACES)
                .create();
        assertEquals("{\"First Name\":\"v1\",\"Total Count\":5}", gson.toJson(new CamelModel()));
    }

    @Test
    public void testFieldNamingLowerCaseWithDashes() {
        Gson gson = new GsonBuilder()
                .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_DASHES)
                .create();
        assertEquals("{\"first-name\":\"v1\",\"total-count\":5}", gson.toJson(new CamelModel()));
    }

    // ----------------------------------------------------------------
    // 11. @Expose with excludeFieldsWithoutExposeAnnotation
    // ----------------------------------------------------------------

    @Test
    public void testExposeSerialize() {
        Gson gson = new GsonBuilder().excludeFieldsWithoutExposeAnnotation().create();
        // serialize=true fields: shown, outOnly ; inOnly is serialize=false ; hidden not exposed
        assertEquals("{\"shown\":\"s\",\"outOnly\":\"o\"}",
                gson.toJson(new ExposeModel("s", "i", "o", "h")));
    }

    @Test
    public void testExposeDeserialize() {
        Gson gson = new GsonBuilder().excludeFieldsWithoutExposeAnnotation().create();
        ExposeModel m = gson.fromJson(
                "{\"shown\":\"S\",\"inOnly\":\"I\",\"outOnly\":\"O\",\"hidden\":\"H\"}",
                ExposeModel.class);
        assertEquals("S", m.shown);   // deserialize=true
        assertEquals("I", m.inOnly);  // deserialize=true
        assertNull(m.outOnly);        // deserialize=false
        assertNull(m.hidden);         // not exposed
    }

    // ----------------------------------------------------------------
    // 12. @Since + version
    // ----------------------------------------------------------------

    @Test
    public void testVersionExcludesNewerField() {
        Gson gson = new GsonBuilder().setVersion(1.0).create();
        assertEquals("{\"stable\":\"a\"}", gson.toJson(new VersionedModel("a", "b")));
    }

    @Test
    public void testVersionIncludesNewerField() {
        Gson gson = new GsonBuilder().setVersion(2.0).create();
        assertEquals("{\"stable\":\"a\",\"newer\":\"b\"}", gson.toJson(new VersionedModel("a", "b")));
    }

    // ----------------------------------------------------------------
    // 13. ExclusionStrategy
    // ----------------------------------------------------------------

    @Test
    public void testCustomExclusionStrategy() {
        ExclusionStrategy strat = new ExclusionStrategy() {
            @Override public boolean shouldSkipField(FieldAttributes f) {
                return "age".equals(f.getName());
            }
            @Override public boolean shouldSkipClass(Class<?> clazz) {
                return false;
            }
        };
        Gson gson = new GsonBuilder().setExclusionStrategies(strat).create();
        assertEquals("{\"name\":\"Z\",\"active\":true}", gson.toJson(new Person("Z", 50, true)));
    }

    // ----------------------------------------------------------------
    // 14. TypeToken for generic collections (round-trip)
    // ----------------------------------------------------------------

    @Test
    public void testTypeTokenListOfStrings() {
        Gson gson = new Gson();
        Type t = new TypeToken<List<String>>() {}.getType();
        List<String> list = gson.fromJson("[\"a\",\"b\",\"c\"]", t);
        assertEquals(Arrays.asList("a", "b", "c"), list);
    }

    @Test
    public void testTypeTokenListOfPojos() {
        Gson gson = new Gson();
        Type t = new TypeToken<List<Point>>() {}.getType();
        List<Point> list = gson.fromJson("[{\"x\":1,\"y\":2},{\"x\":3,\"y\":4}]", t);
        assertEquals(2, list.size());
        assertEquals(1, list.get(0).x);
        assertEquals(4, list.get(1).y);
    }

    @Test
    public void testTypeTokenNestedGenerics() {
        Gson gson = new Gson();
        Type t = new TypeToken<Map<String, List<Integer>>>() {}.getType();
        Map<String, List<Integer>> m = gson.fromJson("{\"a\":[1,2],\"b\":[3]}", t);
        assertEquals(Arrays.asList(1, 2), m.get("a"));
        assertEquals(Arrays.asList(3), m.get("b"));
    }

    @Test
    public void testTypeTokenGetType() {
        TypeToken<List<Long>> tt = new TypeToken<List<Long>>() {};
        assertEquals("java.util.List<java.lang.Long>", tt.getType().toString());
        assertEquals(List.class, tt.getRawType());
    }

    // ----------------------------------------------------------------
    // 15. Custom TypeAdapter (streaming) registration
    // ----------------------------------------------------------------

    static class MoneyAdapter extends TypeAdapter<Money> {
        @Override public void write(JsonWriter out, Money value) throws IOException {
            if (value == null) { out.nullValue(); return; }
            // serialize as decimal string "1.23"
            out.value(String.format("%d.%02d", value.cents / 100, Math.abs(value.cents % 100)));
        }
        @Override public Money read(JsonReader in) throws IOException {
            String s = in.nextString();
            int dot = s.indexOf('.');
            long whole = Long.parseLong(s.substring(0, dot));
            long frac = Long.parseLong(s.substring(dot + 1));
            return new Money(whole * 100 + frac);
        }
    }

    @Test
    public void testCustomTypeAdapterSerialize() {
        Gson gson = new GsonBuilder().registerTypeAdapter(Money.class, new MoneyAdapter()).create();
        assertEquals("\"12.34\"", gson.toJson(new Money(1234)));
    }

    @Test
    public void testCustomTypeAdapterDeserialize() {
        Gson gson = new GsonBuilder().registerTypeAdapter(Money.class, new MoneyAdapter()).create();
        Money m = gson.fromJson("\"5.06\"", Money.class);
        assertEquals(506, m.cents);
    }

    // ----------------------------------------------------------------
    // 16. Custom JsonSerializer / JsonDeserializer (tree model)
    // ----------------------------------------------------------------

    @Test
    public void testCustomJsonSerializer() {
        JsonSerializer<Money> ser = new JsonSerializer<Money>() {
            @Override public JsonElement serialize(Money src, Type t, JsonSerializationContext ctx) {
                return new JsonPrimitive(src.cents);
            }
        };
        Gson gson = new GsonBuilder().registerTypeAdapter(Money.class, ser).create();
        assertEquals("789", gson.toJson(new Money(789)));
    }

    @Test
    public void testCustomJsonDeserializer() {
        JsonDeserializer<Money> de = new JsonDeserializer<Money>() {
            @Override public Money deserialize(JsonElement json, Type t, JsonDeserializationContext ctx)
                    throws JsonParseException {
                return new Money(json.getAsLong());
            }
        };
        Gson gson = new GsonBuilder().registerTypeAdapter(Money.class, de).create();
        assertEquals(4242, gson.fromJson("4242", Money.class).cents);
    }

    // ----------------------------------------------------------------
    // 17. JsonParser + tree model (JsonObject / JsonArray / JsonPrimitive)
    // ----------------------------------------------------------------

    @Test
    public void testJsonParserParseObject() {
        JsonElement el = JsonParser.parseString("{\"k\":\"v\",\"n\":42}");
        assertTrue(el.isJsonObject());
        JsonObject obj = el.getAsJsonObject();
        assertEquals("v", obj.get("k").getAsString());
        assertEquals(42, obj.get("n").getAsInt());
    }

    @Test
    public void testJsonParserParseArray() {
        JsonElement el = JsonParser.parseString("[1,2,3]");
        assertTrue(el.isJsonArray());
        JsonArray arr = el.getAsJsonArray();
        assertEquals(3, arr.size());
        assertEquals(1, arr.get(0).getAsInt());
        assertEquals(3, arr.get(2).getAsInt());
    }

    @Test
    public void testJsonObjectBuildAndSerialize() {
        JsonObject obj = new JsonObject();
        obj.addProperty("name", "Tim");
        obj.addProperty("age", 30);
        obj.addProperty("ok", true);
        obj.add("nullField", JsonNull.INSTANCE);
        assertEquals("{\"name\":\"Tim\",\"age\":30,\"ok\":true,\"nullField\":null}", obj.toString());
    }

    @Test
    public void testJsonArrayBuild() {
        JsonArray arr = new JsonArray();
        arr.add(1);
        arr.add("two");
        arr.add(true);
        assertEquals("[1,\"two\",true]", arr.toString());
        assertEquals(3, arr.size());
    }

    @Test
    public void testJsonPrimitiveTypes() {
        JsonPrimitive pInt = new JsonPrimitive(42);
        assertTrue(pInt.isNumber());
        assertEquals(42, pInt.getAsInt());

        JsonPrimitive pStr = new JsonPrimitive("hello");
        assertTrue(pStr.isString());
        assertEquals("hello", pStr.getAsString());

        JsonPrimitive pBool = new JsonPrimitive(true);
        assertTrue(pBool.isBoolean());
        assertTrue(pBool.getAsBoolean());
    }

    @Test
    public void testJsonNull() {
        assertTrue(JsonNull.INSTANCE.isJsonNull());
        assertEquals("null", JsonParser.parseString("null").toString());
    }

    @Test
    public void testJsonObjectHasAndRemove() {
        JsonObject obj = new JsonObject();
        obj.addProperty("a", 1);
        obj.addProperty("b", 2);
        assertTrue(obj.has("a"));
        assertFalse(obj.has("z"));
        obj.remove("a");
        assertFalse(obj.has("a"));
        assertEquals(1, obj.entrySet().size());
    }

    @Test
    public void testNestedTreeNavigation() {
        JsonElement el = JsonParser.parseString(
                "{\"user\":{\"name\":\"Q\",\"roles\":[\"admin\",\"dev\"]}}");
        JsonObject root = el.getAsJsonObject();
        JsonObject user = root.getAsJsonObject("user");
        assertEquals("Q", user.get("name").getAsString());
        JsonArray roles = user.getAsJsonArray("roles");
        assertEquals("admin", roles.get(0).getAsString());
        assertEquals("dev", roles.get(1).getAsString());
    }

    // ----------------------------------------------------------------
    // 18. toJsonTree / fromJson(JsonElement)
    // ----------------------------------------------------------------

    @Test
    public void testToJsonTree() {
        Gson gson = new Gson();
        JsonElement tree = gson.toJsonTree(new Point(11, 22));
        assertTrue(tree.isJsonObject());
        assertEquals(11, tree.getAsJsonObject().get("x").getAsInt());
        assertEquals(22, tree.getAsJsonObject().get("y").getAsInt());
    }

    @Test
    public void testFromJsonElement() {
        Gson gson = new Gson();
        JsonObject obj = new JsonObject();
        obj.addProperty("x", 100);
        obj.addProperty("y", 200);
        Point p = gson.fromJson(obj, Point.class);
        assertEquals(100, p.x);
        assertEquals(200, p.y);
    }

    // ----------------------------------------------------------------
    // 19. JsonWriter streaming (manual)
    // ----------------------------------------------------------------

    @Test
    public void testJsonWriterStreaming() throws IOException {
        StringWriter sw = new StringWriter();
        JsonWriter w = new JsonWriter(sw);
        w.beginObject();
        w.name("id").value(7);
        w.name("tags").beginArray().value("x").value("y").endArray();
        w.name("flag").value(true);
        w.name("nothing").nullValue();
        w.endObject();
        w.close();
        assertEquals("{\"id\":7,\"tags\":[\"x\",\"y\"],\"flag\":true,\"nothing\":null}", sw.toString());
    }

    @Test
    public void testJsonWriterIndented() throws IOException {
        StringWriter sw = new StringWriter();
        JsonWriter w = new JsonWriter(sw);
        w.setIndent("  ");
        w.beginObject();
        w.name("a").value(1);
        w.endObject();
        w.close();
        assertEquals("{\n  \"a\": 1\n}", sw.toString());
    }

    // ----------------------------------------------------------------
    // 20. JsonReader streaming (manual)
    // ----------------------------------------------------------------

    @Test
    public void testJsonReaderStreaming() throws IOException {
        JsonReader r = new JsonReader(new StringReader("{\"name\":\"Bob\",\"age\":50}"));
        r.beginObject();
        assertEquals(JsonToken.NAME, r.peek());
        assertEquals("name", r.nextName());
        assertEquals("Bob", r.nextString());
        assertEquals("age", r.nextName());
        assertEquals(50, r.nextInt());
        r.endObject();
        assertEquals(JsonToken.END_DOCUMENT, r.peek());
        r.close();
    }

    @Test
    public void testJsonReaderArray() throws IOException {
        JsonReader r = new JsonReader(new StringReader("[10,20,30]"));
        r.beginArray();
        List<Integer> got = new ArrayList<Integer>();
        while (r.hasNext()) {
            got.add(r.nextInt());
        }
        r.endArray();
        r.close();
        assertEquals(Arrays.asList(10, 20, 30), got);
    }

    // ----------------------------------------------------------------
    // 21. Numbers: int, long, double, BigInteger, BigDecimal
    // ----------------------------------------------------------------

    @Test
    public void testSerializeNumbers() {
        Gson gson = new Gson();
        assertEquals("42", gson.toJson(42));
        assertEquals("42", gson.toJson(42L));
        assertEquals("3.14", gson.toJson(3.14));
        assertEquals("1234567890123456789", gson.toJson(new BigInteger("1234567890123456789")));
        assertEquals("123.456", gson.toJson(new BigDecimal("123.456")));
    }

    @Test
    public void testDeserializeNumbers() {
        Gson gson = new Gson();
        assertEquals(Integer.valueOf(7), gson.fromJson("7", Integer.class));
        assertEquals(Long.valueOf(9999999999L), gson.fromJson("9999999999", Long.class));
        assertEquals(2.5, gson.fromJson("2.5", Double.class), 0.0);
        assertEquals(new BigInteger("98765432109876543210"),
                gson.fromJson("98765432109876543210", BigInteger.class));
        assertEquals(new BigDecimal("0.001"), gson.fromJson("0.001", BigDecimal.class));
    }

    @Test
    public void testLongSerializationPolicyString() {
        Gson gson = new GsonBuilder()
                .setLongSerializationPolicy(LongSerializationPolicy.STRING)
                .create();
        assertEquals("\"123\"", gson.toJson(123L));
    }

    // ----------------------------------------------------------------
    // 22. HTML escaping
    // ----------------------------------------------------------------

    @Test
    public void testHtmlEscapingDefault() {
        Gson gson = new Gson();
        // default: HTML chars escaped
        assertEquals("\"\\u003ca\\u003e\"", gson.toJson("<a>"));
        // Gson's HTML-safe escaping also escapes '=' as = and '&' as &.
        assertEquals("\"a\\u003db\\u0026c\"", gson.toJson("a=b&c"));
    }

    @Test
    public void testHtmlEscapingDisabled() {
        Gson gson = new GsonBuilder().disableHtmlEscaping().create();
        assertEquals("\"<a>\"", gson.toJson("<a>"));
        assertEquals("\"a=b&c\"", gson.toJson("a=b&c"));
    }

    @Test
    public void testStringEscaping() {
        Gson gson = new Gson();
        // quotes, backslash, newline, tab
        assertEquals("\"a\\\"b\\\\c\\nd\\te\"", gson.toJson("a\"b\\c\nd\te"));
    }

    @Test
    public void testUnicodeRoundTrip() {
        Gson gson = new Gson();
        String s = gson.toJson("héllo");
        assertEquals("héllo", gson.fromJson(s, String.class));
    }

    // ----------------------------------------------------------------
    // 23. Errors / exceptions
    // ----------------------------------------------------------------

    @Test(expected = JsonSyntaxException.class)
    public void testMalformedJsonThrows() {
        new Gson().fromJson("{not valid json", Person.class);
    }

    @Test(expected = JsonSyntaxException.class)
    public void testTrailingDataThrows() {
        new Gson().fromJson("{\"x\":1,\"y\":2} extra", Point.class);
    }

    @Test
    public void testNullStringDeserializesToNull() {
        Gson gson = new Gson();
        assertNull(gson.fromJson("null", Person.class));
    }

    // ----------------------------------------------------------------
    // 24. Arrays (primitive + object)
    // ----------------------------------------------------------------

    @Test
    public void testSerializeIntArray() {
        Gson gson = new Gson();
        assertEquals("[1,2,3,4]", gson.toJson(new int[] {1, 2, 3, 4}));
    }

    @Test
    public void testDeserializeIntArray() {
        Gson gson = new Gson();
        int[] arr = gson.fromJson("[5,6,7]", int[].class);
        assertArrayEquals(new int[] {5, 6, 7}, arr);
    }

    @Test
    public void testSerializeStringArray() {
        Gson gson = new Gson();
        assertEquals("[\"a\",\"b\"]", gson.toJson(new String[] {"a", "b"}));
    }

    @Test
    public void testDeserializeStringArray() {
        Gson gson = new Gson();
        String[] arr = gson.fromJson("[\"p\",\"q\",\"r\"]", String[].class);
        assertArrayEquals(new String[] {"p", "q", "r"}, arr);
    }

    @Test
    public void testTwoDimensionalArray() {
        Gson gson = new Gson();
        int[][] grid = {{1, 2}, {3, 4}};
        assertEquals("[[1,2],[3,4]]", gson.toJson(grid));
        int[][] back = gson.fromJson("[[5,6],[7,8]]", int[][].class);
        assertEquals(5, back[0][0]);
        assertEquals(8, back[1][1]);
    }

    // ----------------------------------------------------------------
    // 25. newBuilder / Gson config introspection
    // ----------------------------------------------------------------

    @Test
    public void testNewBuilderInheritsConfig() {
        Gson base = new GsonBuilder().serializeNulls().create();
        assertTrue(base.serializeNulls());
        Gson derived = base.newBuilder().create();
        assertTrue(derived.serializeNulls());
    }

    @Test
    public void testHtmlSafeFlag() {
        assertTrue(new Gson().htmlSafe());
        assertFalse(new GsonBuilder().disableHtmlEscaping().create().htmlSafe());
    }

    // ----------------------------------------------------------------
    // 26. getAdapter + TypeAdapter.toJson / fromJson
    // ----------------------------------------------------------------

    @Test
    public void testGetAdapterToJsonAndFromJson() throws IOException {
        Gson gson = new Gson();
        TypeAdapter<Point> adapter = gson.getAdapter(Point.class);
        assertEquals("{\"x\":1,\"y\":2}", adapter.toJson(new Point(1, 2)));
        Point p = adapter.fromJson("{\"x\":9,\"y\":8}");
        assertEquals(9, p.x);
        assertEquals(8, p.y);
    }

    @Test
    public void testGetAdapterViaTypeToken() throws IOException {
        Gson gson = new Gson();
        TypeAdapter<List<Integer>> adapter =
                gson.getAdapter(new TypeToken<List<Integer>>() {});
        assertEquals("[1,2,3]", adapter.toJson(Arrays.asList(1, 2, 3)));
    }

    @Test
    public void testTypeAdapterNullSafe() throws IOException {
        Gson gson = new Gson();
        TypeAdapter<Point> adapter = gson.getAdapter(Point.class).nullSafe();
        assertEquals("null", adapter.toJson(null));
        assertNull(adapter.fromJson("null"));
    }

    // ----------------------------------------------------------------
    // 27. registerTypeHierarchyAdapter
    // ----------------------------------------------------------------

    interface Shape {}
    static class Circle implements Shape { int r = 5; }

    @Test
    public void testTypeHierarchyAdapter() {
        JsonSerializer<Shape> ser = new JsonSerializer<Shape>() {
            @Override public JsonElement serialize(Shape src, Type t, JsonSerializationContext ctx) {
                return new JsonPrimitive("shape");
            }
        };
        Gson gson = new GsonBuilder().registerTypeHierarchyAdapter(Shape.class, ser).create();
        assertEquals("\"shape\"", gson.toJson(new Circle(), Shape.class));
    }

    // ----------------------------------------------------------------
    // 28. Map round-trip with complex values via TypeToken
    // ----------------------------------------------------------------

    @Test
    public void testMapOfPojoRoundTrip() {
        Gson gson = new Gson();
        Map<String, Point> m = new LinkedHashMap<String, Point>();
        m.put("a", new Point(1, 2));
        m.put("b", new Point(3, 4));
        String json = gson.toJson(m);
        assertEquals("{\"a\":{\"x\":1,\"y\":2},\"b\":{\"x\":3,\"y\":4}}", json);

        Type t = new TypeToken<Map<String, Point>>() {}.getType();
        Map<String, Point> back = gson.fromJson(json, t);
        assertEquals(1, back.get("a").x);
        assertEquals(4, back.get("b").y);
    }

    // ----------------------------------------------------------------
    // 29. JsonObject deepCopy + JsonElement equality
    // ----------------------------------------------------------------

    @Test
    public void testJsonElementEquals() {
        JsonElement a = JsonParser.parseString("{\"x\":1,\"y\":2}");
        JsonElement b = JsonParser.parseString("{\"x\":1,\"y\":2}");
        assertEquals(a, b);
        assertEquals(a.hashCode(), b.hashCode());
        JsonElement c = JsonParser.parseString("{\"x\":1,\"y\":3}");
        assertNotEquals(a, c);
    }

    @Test
    public void testJsonObjectDeepCopy() {
        JsonObject orig = new JsonObject();
        orig.addProperty("k", 1);
        JsonObject copy = orig.deepCopy();
        copy.addProperty("k", 2);
        assertEquals(1, orig.get("k").getAsInt());
        assertEquals(2, copy.get("k").getAsInt());
    }

    // ----------------------------------------------------------------
    // 30. Empty / edge structures
    // ----------------------------------------------------------------

    @Test
    public void testEmptyObjectAndArray() {
        Gson gson = new Gson();
        assertEquals("{}", gson.toJson(new LinkedHashMap<String, Object>()));
        assertEquals("[]", gson.toJson(new ArrayList<Object>()));
        assertEquals("{}", new JsonObject().toString());
        assertEquals("[]", new JsonArray().toString());
    }

    @Test
    public void testGetAsNumberConversions() {
        JsonPrimitive p = new JsonPrimitive(255);
        assertEquals(255L, p.getAsLong());
        assertEquals(255.0, p.getAsDouble(), 0.0);
        assertEquals((short) 255, p.getAsShort());
        assertEquals("255", p.getAsString());
    }
}
