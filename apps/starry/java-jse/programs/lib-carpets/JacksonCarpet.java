package org.starry.dod;

import com.fasterxml.jackson.annotation.*;
import com.fasterxml.jackson.core.*;
import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.*;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.module.SimpleModule;
import com.fasterxml.jackson.databind.node.*;

import java.io.IOException;
import java.io.StringWriter;
import java.math.BigDecimal;
import java.math.BigInteger;
import java.util.*;

/**
 * Carpet-grade, offline, deterministic exercise of the jackson-databind 2.17 API surface
 * (jackson-core + jackson-annotations + jackson-databind). No external network/files; all
 * data is synthesized in-memory and asserted to exact values.
 *
 * Note: the jackson-datatype-jsr310 / jackson-datatype-jdk8 modules are NOT bundled in the
 * classpath jar, so JavaTimeModule (LocalDate/LocalDateTime) is intentionally skipped and
 * those would-be assertions are not counted (see report). Optional is exercised through the
 * default bean-introspection path that databind uses when the jdk8 module is absent.
 */
public class JacksonCarpet {

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

    static void checkEq(String name, Object expected, Object actual) {
        boolean eq = Objects.equals(expected, actual);
        if (eq) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + expected + "] actual=[" + actual + "]");
        }
    }

    // ----------------------------------------------------------------- model types

    @JsonPropertyOrder({"x", "y"})
    public static class Point {
        public int x;
        public int y;
        public Point() {}
        public Point(int x, int y) { this.x = x; this.y = y; }
        @Override public boolean equals(Object o) {
            if (!(o instanceof Point)) return false;
            Point p = (Point) o; return p.x == x && p.y == y;
        }
        @Override public int hashCode() { return x * 31 + y; }
    }

    @JsonPropertyOrder({"full_name", "age"})
    public static class Person {
        @JsonProperty("full_name")
        public String name;
        public int age;
        @JsonIgnore
        public String password;
        public Person() {}
        public Person(String name, int age, String password) {
            this.name = name; this.age = age; this.password = password;
        }
    }

    // @JsonCreator constructor-based deserialization
    public static class CreatorBean {
        private final String id;
        private final int count;
        @JsonCreator
        public CreatorBean(@JsonProperty("id") String id, @JsonProperty("count") int count) {
            this.id = id; this.count = count;
        }
        public String getId() { return id; }
        public int getCount() { return count; }
    }

    // @JsonAlias accepts several incoming names for one property
    public static class AliasBean {
        @JsonAlias({"login", "user_name"})
        public String username;
    }

    // @JsonGetter / @JsonSetter customizing accessor names
    public static class AccessorBean {
        private String value;
        @JsonGetter("v")
        public String readValue() { return value; }
        @JsonSetter("v")
        public void writeValue(String v) { this.value = v; }
    }

    // @JsonInclude(NON_NULL): null-valued properties are omitted from output
    @JsonInclude(JsonInclude.Include.NON_NULL)
    @JsonPropertyOrder({"a", "b", "c"})
    public static class IncludeBean {
        public String a;
        public String b;
        public String c;
    }

    // @JsonFormat shape=STRING renders the numeric field as a quoted string
    public static class FormatBean {
        @JsonFormat(shape = JsonFormat.Shape.STRING)
        public int amount;
        public FormatBean() {}
        public FormatBean(int amount) { this.amount = amount; }
    }

    // @JsonAnyGetter / @JsonAnySetter dynamic property bag
    public static class AnyBean {
        public String fixed;
        private final Map<String, Object> extra = new LinkedHashMap<>();
        @JsonAnyGetter
        public Map<String, Object> getExtra() { return extra; }
        @JsonAnySetter
        public void setExtra(String k, Object v) { extra.put(k, v); }
    }

    // Polymorphism via @JsonTypeInfo + @JsonSubTypes
    @JsonTypeInfo(use = JsonTypeInfo.Id.NAME, include = JsonTypeInfo.As.PROPERTY, property = "type")
    @JsonSubTypes({
        @JsonSubTypes.Type(value = Circle.class, name = "circle"),
        @JsonSubTypes.Type(value = Square.class, name = "square")
    })
    public static abstract class Shape {
        public String label;
    }
    public static class Circle extends Shape {
        public double radius;
        public Circle() {}
        public Circle(String label, double radius) { this.label = label; this.radius = radius; }
    }
    public static class Square extends Shape {
        public double side;
        public Square() {}
        public Square(String label, double side) { this.label = label; this.side = side; }
    }

    public enum Color {
        RED, GREEN, BLUE;
        @Override public String toString() { return "color-" + name().toLowerCase(); }
    }

    public static class EnumBean {
        public Color color;
        public EnumBean() {}
        public EnumBean(Color c) { this.color = c; }
    }

    // Custom serializer/deserializer target: money stored as integer cents
    public static class Money {
        public long cents;
        public Money() {}
        public Money(long cents) { this.cents = cents; }
        @Override public boolean equals(Object o) {
            return (o instanceof Money) && ((Money) o).cents == cents;
        }
        @Override public int hashCode() { return Long.hashCode(cents); }
    }

    public static class MoneySerializer extends JsonSerializer<Money> {
        @Override public void serialize(Money v, JsonGenerator gen, SerializerProvider sp) throws IOException {
            gen.writeString("$" + (v.cents / 100) + "." + String.format("%02d", v.cents % 100));
        }
    }
    public static class MoneyDeserializer extends JsonDeserializer<Money> {
        @Override public Money deserialize(JsonParser p, DeserializationContext ctxt) throws IOException {
            String s = p.getValueAsString();          // e.g. "$1.50"
            String digits = s.replace("$", "");
            String[] parts = digits.split("\\.");
            long whole = Long.parseLong(parts[0]);
            long frac = Long.parseLong(parts[1]);
            return new Money(whole * 100 + frac);
        }
    }

    // No @JsonPropertyOrder: declaration order is zebra, apple; alpha sort -> apple, zebra
    public static class SortBean {
        public int zebra;
        public int apple;
        public SortBean() {}
        public SortBean(int zebra, int apple) { this.zebra = zebra; this.apple = apple; }
    }

    public static class Wallet {
        public String owner;
        public Money balance;
        public Wallet() {}
        public Wallet(String owner, Money balance) { this.owner = owner; this.balance = balance; }
    }

    // ----------------------------------------------------------------- tests

    static void testPojoRoundTrip(ObjectMapper m) throws Exception {
        Point p = new Point(3, 4);
        String json = m.writeValueAsString(p);
        checkEq("pojo.write.exact", "{\"x\":3,\"y\":4}", json);
        Point back = m.readValue(json, Point.class);
        checkEq("pojo.read.equals", p, back);
        checkEq("pojo.read.x", 3, back.x);
        checkEq("pojo.read.y", 4, back.y);
        // writeValueAsBytes / readValue(byte[])
        byte[] bytes = m.writeValueAsBytes(p);
        Point fromBytes = m.readValue(bytes, Point.class);
        checkEq("pojo.bytes.roundtrip", p, fromBytes);
    }

    static void testAnnotationsPropertyIgnore(ObjectMapper m) throws Exception {
        Person person = new Person("Ada", 36, "s3cr3t");
        String json = m.writeValueAsString(person);
        checkEq("ann.jsonproperty.rename", "{\"full_name\":\"Ada\",\"age\":36}", json);
        check("ann.jsonignore.absent", !json.contains("password") && !json.contains("s3cr3t"));
        Person back = m.readValue("{\"full_name\":\"Lin\",\"age\":7}", Person.class);
        checkEq("ann.jsonproperty.read", "Lin", back.name);
        checkEq("ann.jsonproperty.read.age", 7, back.age);
        check("ann.jsonignore.read.null", back.password == null);
    }

    static void testCreator(ObjectMapper m) throws Exception {
        CreatorBean b = m.readValue("{\"id\":\"X9\",\"count\":12}", CreatorBean.class);
        checkEq("ann.creator.id", "X9", b.getId());
        checkEq("ann.creator.count", 12, b.getCount());
        String json = m.writeValueAsString(b);
        JsonNode n = m.readTree(json);
        checkEq("ann.creator.write.id", "X9", n.get("id").asText());
        checkEq("ann.creator.write.count", 12, n.get("count").asInt());
    }

    static void testAlias(ObjectMapper m) throws Exception {
        AliasBean a = m.readValue("{\"login\":\"root\"}", AliasBean.class);
        checkEq("ann.alias.login", "root", a.username);
        AliasBean b = m.readValue("{\"user_name\":\"guest\"}", AliasBean.class);
        checkEq("ann.alias.user_name", "guest", b.username);
        AliasBean c = m.readValue("{\"username\":\"primary\"}", AliasBean.class);
        checkEq("ann.alias.primary", "primary", c.username);
    }

    static void testAccessor(ObjectMapper m) throws Exception {
        AccessorBean bean = m.readValue("{\"v\":\"hello\"}", AccessorBean.class);
        checkEq("ann.setter.read", "hello", bean.readValue());
        String json = m.writeValueAsString(bean);
        checkEq("ann.getter.write", "{\"v\":\"hello\"}", json);
    }

    static void testInclude(ObjectMapper m) throws Exception {
        IncludeBean b = new IncludeBean();
        b.a = "x";
        b.b = null;
        b.c = "z";
        String json = m.writeValueAsString(b);
        checkEq("ann.include.non_null", "{\"a\":\"x\",\"c\":\"z\"}", json);
        // Mapper-wide NON_NULL via setSerializationInclusion
        ObjectMapper m2 = new ObjectMapper().setSerializationInclusion(JsonInclude.Include.NON_NULL);
        Map<String, Object> map = new LinkedHashMap<>();
        map.put("present", 1);
        map.put("missing", null);
        checkEq("include.mapper_wide", "{\"present\":1}", m2.writeValueAsString(map));
    }

    static void testFormat(ObjectMapper m) throws Exception {
        FormatBean b = new FormatBean(42);
        String json = m.writeValueAsString(b);
        checkEq("ann.format.shape_string.write", "{\"amount\":\"42\"}", json);
        FormatBean back = m.readValue("{\"amount\":\"77\"}", FormatBean.class);
        checkEq("ann.format.shape_string.read", 77, back.amount);
    }

    static void testAnyGetterSetter(ObjectMapper m) throws Exception {
        AnyBean b = new AnyBean();
        b.fixed = "F";
        b.getExtra().put("dyn1", 10);
        b.getExtra().put("dyn2", "v");
        String json = m.writeValueAsString(b);
        JsonNode n = m.readTree(json);
        checkEq("ann.anygetter.fixed", "F", n.get("fixed").asText());
        checkEq("ann.anygetter.dyn1", 10, n.get("dyn1").asInt());
        checkEq("ann.anygetter.dyn2", "v", n.get("dyn2").asText());
        check("ann.anygetter.flat", !n.has("extra"));
        AnyBean back = m.readValue("{\"fixed\":\"G\",\"k1\":1,\"k2\":2}", AnyBean.class);
        checkEq("ann.anysetter.fixed", "G", back.fixed);
        checkEq("ann.anysetter.k1", 1, back.getExtra().get("k1"));
        checkEq("ann.anysetter.k2", 2, back.getExtra().get("k2"));
    }

    static void testPolymorphism(ObjectMapper m) throws Exception {
        Circle c = new Circle("c1", 2.5);
        String json = m.writeValueAsString(c);
        JsonNode n = m.readTree(json);
        checkEq("poly.write.type", "circle", n.get("type").asText());
        checkEq("poly.write.radius", 2.5, n.get("radius").asDouble());
        Shape back = m.readValue(json, Shape.class);
        check("poly.read.instanceof_circle", back instanceof Circle);
        checkEq("poly.read.radius", 2.5, ((Circle) back).radius);
        Shape sq = m.readValue("{\"type\":\"square\",\"label\":\"s1\",\"side\":4.0}", Shape.class);
        check("poly.read.instanceof_square", sq instanceof Square);
        checkEq("poly.read.side", 4.0, ((Square) sq).side);
        checkEq("poly.read.label", "s1", sq.label);
        // round-trip a list of base type with type info preserved (typed writer so the
        // element type is Shape and the polymorphic type id is emitted for each entry)
        List<Shape> list = Arrays.asList(new Circle("a", 1.0), new Square("b", 2.0));
        String listJson = m.writerFor(new TypeReference<List<Shape>>() {}).writeValueAsString(list);
        check("poly.list.write.has_type", listJson.contains("circle") && listJson.contains("square"));
        List<Shape> rl = m.readValue(listJson, new TypeReference<List<Shape>>() {});
        check("poly.list.0_circle", rl.get(0) instanceof Circle);
        check("poly.list.1_square", rl.get(1) instanceof Square);
    }

    static void testMapList(ObjectMapper m) throws Exception {
        // Map<String,Integer> via TypeReference
        Map<String, Integer> src = new LinkedHashMap<>();
        src.put("a", 1);
        src.put("b", 2);
        String json = m.writeValueAsString(src);
        checkEq("map.write.ordered", "{\"a\":1,\"b\":2}", json);
        Map<String, Integer> back = m.readValue(json, new TypeReference<Map<String, Integer>>() {});
        checkEq("map.read.size", 2, back.size());
        checkEq("map.read.a", Integer.valueOf(1), back.get("a"));
        checkEq("map.read.b", Integer.valueOf(2), back.get("b"));
        check("map.read.value_type_integer", back.get("a") instanceof Integer);

        // List<String>
        List<String> ls = Arrays.asList("x", "y", "z");
        String lj = m.writeValueAsString(ls);
        checkEq("list.write", "[\"x\",\"y\",\"z\"]", lj);
        List<String> lback = m.readValue(lj, new TypeReference<List<String>>() {});
        checkEq("list.read.size", 3, lback.size());
        checkEq("list.read.1", "y", lback.get(1));

        // List<Point> generic of POJO
        List<Point> pts = Arrays.asList(new Point(1, 2), new Point(3, 4));
        String pj = m.writeValueAsString(pts);
        checkEq("list.pojo.write", "[{\"x\":1,\"y\":2},{\"x\":3,\"y\":4}]", pj);
        List<Point> pback = m.readValue(pj, new TypeReference<List<Point>>() {});
        checkEq("list.pojo.read.size", 2, pback.size());
        checkEq("list.pojo.read.0", new Point(1, 2), pback.get(0));

        // nested generic Map<String,List<Integer>>
        Map<String, List<Integer>> nested = new LinkedHashMap<>();
        nested.put("evens", Arrays.asList(2, 4));
        nested.put("odds", Arrays.asList(1, 3));
        String nj = m.writeValueAsString(nested);
        checkEq("map.nested.write", "{\"evens\":[2,4],\"odds\":[1,3]}", nj);
        Map<String, List<Integer>> nback =
                m.readValue(nj, new TypeReference<Map<String, List<Integer>>>() {});
        checkEq("map.nested.read.evens.0", Integer.valueOf(2), nback.get("evens").get(0));
        checkEq("map.nested.read.odds.1", Integer.valueOf(3), nback.get("odds").get(1));
    }

    static void testArrays(ObjectMapper m) throws Exception {
        int[] ints = {5, 6, 7};
        String ij = m.writeValueAsString(ints);
        checkEq("array.int.write", "[5,6,7]", ij);
        int[] iback = m.readValue(ij, int[].class);
        checkEq("array.int.read.len", 3, iback.length);
        checkEq("array.int.read.2", 7, iback[2]);

        String[] strs = {"p", "q"};
        String sj = m.writeValueAsString(strs);
        checkEq("array.str.write", "[\"p\",\"q\"]", sj);
        String[] sback = m.readValue(sj, String[].class);
        checkEq("array.str.read.0", "p", sback[0]);

        Point[] pts = {new Point(1, 1), new Point(2, 2)};
        String pj = m.writeValueAsString(pts);
        checkEq("array.pojo.write", "[{\"x\":1,\"y\":1},{\"x\":2,\"y\":2}]", pj);
        Point[] pback = m.readValue(pj, Point[].class);
        checkEq("array.pojo.read.1", new Point(2, 2), pback[1]);
    }

    static void testConvertValue(ObjectMapper m) throws Exception {
        Point p = new Point(8, 9);
        Map<String, Object> asMap = m.convertValue(p, new TypeReference<Map<String, Object>>() {});
        checkEq("convert.pojo_to_map.x", 8, asMap.get("x"));
        checkEq("convert.pojo_to_map.y", 9, asMap.get("y"));
        Map<String, Object> src = new LinkedHashMap<>();
        src.put("x", 11);
        src.put("y", 22);
        Point back = m.convertValue(src, Point.class);
        checkEq("convert.map_to_pojo", new Point(11, 22), back);
        // primitive widening via convertValue
        Integer iv = m.convertValue("123", Integer.class);
        checkEq("convert.string_to_int", Integer.valueOf(123), iv);
        // convert to JsonNode
        JsonNode node = m.convertValue(p, JsonNode.class);
        checkEq("convert.pojo_to_node", 8, node.get("x").asInt());
    }

    static void testTreeModel(ObjectMapper m) throws Exception {
        String json = "{\"name\":\"root\",\"nums\":[10,20,30],\"child\":{\"flag\":true,\"score\":4.5},\"nil\":null}";
        JsonNode tree = m.readTree(json);
        check("tree.is_object", tree.isObject());
        checkEq("tree.name", "root", tree.get("name").asText());
        check("tree.nums_is_array", tree.get("nums").isArray());
        checkEq("tree.nums.size", 3, tree.get("nums").size());
        checkEq("tree.nums.1", 20, tree.get("nums").get(1).asInt());
        checkEq("tree.child.flag", true, tree.get("child").get("flag").asBoolean());
        checkEq("tree.child.score", 4.5, tree.get("child").get("score").asDouble());
        check("tree.nil.isNull", tree.get("nil").isNull());

        // get vs path: missing field
        check("tree.get_missing_null", tree.get("nope") == null);
        check("tree.path_missing_node", tree.path("nope").isMissingNode());
        checkEq("tree.path_default_text", "DEF", tree.path("nope").asText("DEF"));
        checkEq("tree.path_default_int", 99, tree.path("nope").asInt(99));

        // JsonPointer / at
        checkEq("tree.at.nums1", 20, tree.at("/nums/1").asInt());
        checkEq("tree.at.child_score", 4.5, tree.at("/child/score").asDouble());
        check("tree.at.missing", tree.at("/child/missing").isMissingNode());
        JsonPointer ptr = JsonPointer.compile("/nums/2");
        checkEq("tree.pointer.compile", 30, tree.at(ptr).asInt());

        // node type predicates
        check("tree.score.isNumber", tree.at("/child/score").isNumber());
        check("tree.score.isDouble", tree.at("/child/score").isDouble());
        check("tree.nums0.isInt", tree.at("/nums/0").isInt());
        check("tree.name.isTextual", tree.get("name").isTextual());
        check("tree.flag.isBoolean", tree.get("child").get("flag").isBoolean());

        // fieldNames
        Set<String> names = new TreeSet<>();
        tree.fieldNames().forEachRemaining(names::add);
        checkEq("tree.fieldNames", "[child, name, nil, nums]", names.toString());

        // has / hasNonNull
        check("tree.has_nil", tree.has("nil"));
        check("tree.hasNonNull_nil_false", !tree.hasNonNull("nil"));
        check("tree.hasNonNull_name", tree.hasNonNull("name"));
    }

    static void testTreeBuild(ObjectMapper m) throws Exception {
        ObjectNode root = m.createObjectNode();
        root.put("id", 7);
        root.put("name", "node");
        root.put("ratio", 0.25);
        root.put("active", true);
        root.putNull("opt");
        ArrayNode arr = root.putArray("tags");
        arr.add("a").add("b").add("c");
        ObjectNode child = root.putObject("meta");
        child.put("k", "v");

        checkEq("build.id", 7, root.get("id").asInt());
        checkEq("build.tags.size", 3, root.get("tags").size());
        checkEq("build.tags.2", "c", root.get("tags").get(2).asText());
        checkEq("build.meta.k", "v", root.at("/meta/k").asText());
        check("build.opt.isNull", root.get("opt").isNull());

        // serialize the built tree and read it back to confirm fidelity
        String json = m.writeValueAsString(root);
        JsonNode reparsed = m.readTree(json);
        checkEq("build.roundtrip.ratio", 0.25, reparsed.get("ratio").asDouble());
        checkEq("build.roundtrip.active", true, reparsed.get("active").asBoolean());

        // JsonNodeFactory direct construction
        ArrayNode a2 = JsonNodeFactory.instance.arrayNode();
        a2.add(1).add(2).add(3);
        checkEq("build.factory.array.sum_size", 3, a2.size());
        TextNode tn = JsonNodeFactory.instance.textNode("hi");
        checkEq("build.factory.textnode", "hi", tn.asText());

        // remove / replace
        root.remove("opt");
        check("build.remove", !root.has("opt"));
        root.set("name", JsonNodeFactory.instance.textNode("renamed"));
        checkEq("build.set", "renamed", root.get("name").asText());
    }

    static void testStreamingGenerator(ObjectMapper m) throws Exception {
        JsonFactory f = m.getFactory();
        StringWriter sw = new StringWriter();
        try (JsonGenerator g = f.createGenerator(sw)) {
            g.writeStartObject();
            g.writeStringField("name", "stream");
            g.writeNumberField("count", 3);
            g.writeBooleanField("ready", true);
            g.writeArrayFieldStart("vals");
            g.writeNumber(1);
            g.writeNumber(2);
            g.writeEndArray();
            g.writeFieldName("inner");
            g.writeStartObject();
            g.writeStringField("k", "v");
            g.writeEndObject();
            g.writeEndObject();
        }
        String json = sw.toString();
        checkEq("stream.gen.exact",
                "{\"name\":\"stream\",\"count\":3,\"ready\":true,\"vals\":[1,2],\"inner\":{\"k\":\"v\"}}",
                json);
        // reparse to confirm well-formedness
        JsonNode n = m.readTree(json);
        checkEq("stream.gen.reparse.count", 3, n.get("count").asInt());
        checkEq("stream.gen.reparse.inner", "v", n.at("/inner/k").asText());
    }

    static void testStreamingParser(ObjectMapper m) throws Exception {
        String json = "{\"a\":1,\"b\":\"two\",\"c\":[3,4],\"d\":true}";
        JsonFactory f = m.getFactory();
        List<String> fields = new ArrayList<>();
        int intCount = 0;
        long sumInts = 0;
        boolean sawString = false;
        boolean sawTrue = false;
        try (JsonParser p = f.createParser(json)) {
            JsonToken first = p.nextToken();
            check("parse.first_start_object", first == JsonToken.START_OBJECT);
            while (p.nextToken() != JsonToken.END_OBJECT) {
                JsonToken tok = p.currentToken();
                if (tok == JsonToken.FIELD_NAME) {
                    fields.add(p.currentName());
                } else if (tok == JsonToken.VALUE_NUMBER_INT) {
                    intCount++;
                    sumInts += p.getLongValue();
                } else if (tok == JsonToken.VALUE_STRING) {
                    sawString = "two".equals(p.getText());
                } else if (tok == JsonToken.START_ARRAY) {
                    while (p.nextToken() != JsonToken.END_ARRAY) {
                        if (p.currentToken() == JsonToken.VALUE_NUMBER_INT) {
                            intCount++;
                            sumInts += p.getLongValue();
                        }
                    }
                } else if (tok == JsonToken.VALUE_TRUE) {
                    sawTrue = true;
                }
            }
        }
        checkEq("parse.fields", "[a, b, c, d]", fields.toString());
        checkEq("parse.int_count", 3, intCount);   // 1, 3, 4
        checkEq("parse.int_sum", 8L, sumInts);
        check("parse.saw_string", sawString);
        check("parse.saw_true", sawTrue);
    }

    static void testFeatures() throws Exception {
        // FAIL_ON_UNKNOWN_PROPERTIES default true -> throws on extra field
        ObjectMapper strict = new ObjectMapper();
        boolean threw = false;
        try {
            strict.readValue("{\"x\":1,\"y\":2,\"z\":3}", Point.class);
        } catch (Exception e) {
            threw = true;
        }
        check("feat.fail_on_unknown.default_throws", threw);

        // disabling the feature tolerates the extra field
        ObjectMapper lenient = new ObjectMapper()
                .configure(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES, false);
        Point p = lenient.readValue("{\"x\":1,\"y\":2,\"z\":3}", Point.class);
        checkEq("feat.fail_on_unknown.disabled", new Point(1, 2), p);

        // ORDER_MAP_ENTRIES_BY_KEYS
        ObjectMapper ordered = new ObjectMapper()
                .configure(SerializationFeature.ORDER_MAP_ENTRIES_BY_KEYS, true);
        Map<String, Integer> hm = new HashMap<>();
        hm.put("c", 3);
        hm.put("a", 1);
        hm.put("b", 2);
        checkEq("feat.order_map_by_keys", "{\"a\":1,\"b\":2,\"c\":3}", ordered.writeValueAsString(hm));

        // INDENT_OUTPUT produces multi-line, structurally identical JSON
        ObjectMapper indent = new ObjectMapper().enable(SerializationFeature.INDENT_OUTPUT);
        String pretty = indent.writeValueAsString(new Point(1, 2));
        check("feat.indent_has_newline", pretty.contains("\n"));
        checkEq("feat.indent.reparse", new Point(1, 2), indent.readValue(pretty, Point.class));

        // WRITE_ENUMS_USING_TO_STRING toggles enum textual form
        ObjectMapper enumName = new ObjectMapper();
        checkEq("feat.enum.as_name", "\"RED\"", enumName.writeValueAsString(Color.RED));
        ObjectMapper enumToStr = new ObjectMapper()
                .enable(SerializationFeature.WRITE_ENUMS_USING_TO_STRING);
        checkEq("feat.enum.as_tostring", "\"color-red\"", enumToStr.writeValueAsString(Color.RED));

        // READ_ENUMS_USING_TO_STRING
        ObjectMapper enumReadToStr = new ObjectMapper()
                .enable(DeserializationFeature.READ_ENUMS_USING_TO_STRING);
        checkEq("feat.enum.read_tostring", Color.GREEN,
                enumReadToStr.readValue("\"color-green\"", Color.class));

        // ACCEPT_SINGLE_VALUE_AS_ARRAY
        ObjectMapper single = new ObjectMapper()
                .enable(DeserializationFeature.ACCEPT_SINGLE_VALUE_AS_ARRAY);
        List<Integer> one = single.readValue("5", new TypeReference<List<Integer>>() {});
        checkEq("feat.single_as_array.size", 1, one.size());
        checkEq("feat.single_as_array.0", Integer.valueOf(5), one.get(0));

        // MapperFeature.SORT_PROPERTIES_ALPHABETICALLY via JsonMapper builder
        ObjectMapper sorted = JsonMapper.builder()
                .enable(MapperFeature.SORT_PROPERTIES_ALPHABETICALLY)
                .build();
        // SortBean declares zebra then apple; alphabetic order -> apple, zebra
        checkEq("feat.sort_props_alpha",
                "{\"apple\":2,\"zebra\":1}",
                sorted.writeValueAsString(new SortBean(1, 2)));
        // default (declaration) order without the feature
        checkEq("feat.declaration_order",
                "{\"zebra\":1,\"apple\":2}",
                new ObjectMapper().writeValueAsString(new SortBean(1, 2)));
    }

    static void testEnum(ObjectMapper m) throws Exception {
        EnumBean b = new EnumBean(Color.BLUE);
        String json = m.writeValueAsString(b);
        checkEq("enum.bean.write", "{\"color\":\"BLUE\"}", json);
        EnumBean back = m.readValue("{\"color\":\"GREEN\"}", EnumBean.class);
        checkEq("enum.bean.read", Color.GREEN, back.color);
        // enum array round-trip
        Color[] arr = {Color.RED, Color.BLUE};
        String aj = m.writeValueAsString(arr);
        checkEq("enum.array.write", "[\"RED\",\"BLUE\"]", aj);
        Color[] aback = m.readValue(aj, Color[].class);
        checkEq("enum.array.read.1", Color.BLUE, aback[1]);
        // enum as map key
        Map<Color, Integer> mk = new EnumMap<>(Color.class);
        mk.put(Color.RED, 1);
        checkEq("enum.map_key.write", "{\"RED\":1}", m.writeValueAsString(mk));
    }

    static void testBigNumbersAndBytes(ObjectMapper m) throws Exception {
        BigDecimal bd = new BigDecimal("3.141592653589793238462643383279502884");
        String bj = m.writeValueAsString(bd);
        checkEq("bignum.bigdecimal.write", "3.141592653589793238462643383279502884", bj);
        BigDecimal bdBack = m.readValue(bj, BigDecimal.class);
        checkEq("bignum.bigdecimal.read", bd, bdBack);

        BigInteger bi = new BigInteger("123456789012345678901234567890");
        String ij = m.writeValueAsString(bi);
        checkEq("bignum.biginteger.write", "123456789012345678901234567890", ij);
        BigInteger biBack = m.readValue(ij, BigInteger.class);
        checkEq("bignum.biginteger.read", bi, biBack);

        // USE_BIG_DECIMAL_FOR_FLOATS: untyped floats become BigDecimal
        ObjectMapper bigm = new ObjectMapper()
                .enable(DeserializationFeature.USE_BIG_DECIMAL_FOR_FLOATS);
        Object num = bigm.readValue("0.1", Object.class);
        check("bignum.use_bigdecimal_for_floats", num instanceof BigDecimal);
        checkEq("bignum.use_bigdecimal.value", new BigDecimal("0.1"), num);

        // byte[] <-> base64 string
        byte[] raw = {1, 2, 3};
        String b64 = m.writeValueAsString(raw);
        checkEq("bytes.base64.write", "\"AQID\"", b64);
        byte[] decoded = m.readValue(b64, byte[].class);
        check("bytes.base64.read", Arrays.equals(new byte[]{1, 2, 3}, decoded));
    }

    static void testCustomSerDe() throws Exception {
        SimpleModule module = new SimpleModule("money");
        module.addSerializer(Money.class, new MoneySerializer());
        module.addDeserializer(Money.class, new MoneyDeserializer());
        ObjectMapper m = new ObjectMapper();
        m.registerModule(module);

        Wallet w = new Wallet("Lin", new Money(150));
        String json = m.writeValueAsString(w);
        JsonNode n = m.readTree(json);
        checkEq("custom.ser.owner", "Lin", n.get("owner").asText());
        checkEq("custom.ser.balance", "$1.50", n.get("balance").asText());
        check("custom.ser.is_string", n.get("balance").isTextual());

        Wallet back = m.readValue("{\"owner\":\"Ada\",\"balance\":\"$2.05\"}", Wallet.class);
        checkEq("custom.deser.owner", "Ada", back.owner);
        checkEq("custom.deser.balance", new Money(205), back.balance);

        // direct value serialize/deserialize
        checkEq("custom.direct.write", "\"$0.07\"", m.writeValueAsString(new Money(7)));
        checkEq("custom.direct.read", new Money(99), m.readValue("\"$0.99\"", Money.class));
    }

    static void testReaderWriter(ObjectMapper m) throws Exception {
        // ObjectWriter: pretty printer
        ObjectWriter pretty = m.writerWithDefaultPrettyPrinter();
        String prettyJson = pretty.writeValueAsString(new Point(1, 2));
        check("rw.writer.pretty.newline", prettyJson.contains("\n"));
        checkEq("rw.writer.pretty.reparse", new Point(1, 2), m.readValue(prettyJson, Point.class));

        // ObjectWriter forType
        ObjectWriter wf = m.writerFor(Point.class);
        checkEq("rw.writer.forType", "{\"x\":3,\"y\":4}", wf.writeValueAsString(new Point(3, 4)));

        // ObjectWriter with a SerializationFeature
        ObjectWriter wfeat = m.writer().with(SerializationFeature.INDENT_OUTPUT);
        check("rw.writer.with_feature", wfeat.writeValueAsString(new Point(0, 0)).contains("\n"));

        // ObjectReader for a type
        ObjectReader r = m.readerFor(Point.class);
        checkEq("rw.reader.forType", new Point(5, 6), r.readValue("{\"x\":5,\"y\":6}"));

        // ObjectReader for a List
        ObjectReader rl = m.readerForListOf(String.class);
        List<String> list = rl.readValue("[\"a\",\"b\"]");
        checkEq("rw.reader.listOf.size", 2, list.size());
        checkEq("rw.reader.listOf.0", "a", list.get(0));

        // ObjectReader.withFeatures lenient unknowns
        ObjectReader lenient = m.readerFor(Point.class)
                .with(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES)
                .without(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES);
        checkEq("rw.reader.without_feature", new Point(1, 1),
                lenient.readValue("{\"x\":1,\"y\":1,\"extra\":9}"));

        // readTree via ObjectReader
        JsonNode tree = m.reader().readTree("{\"q\":42}");
        checkEq("rw.reader.readTree", 42, tree.get("q").asInt());
    }

    static void testOptional(ObjectMapper m) throws Exception {
        // The jackson-datatype-jdk8 module is NOT on the classpath, so java.util.Optional is
        // unsupported by default and serialization throws InvalidDefinitionException. Assert that
        // documented behavior deterministically (exception path is part of the coverage matrix).
        boolean threw = false;
        String msg = "";
        try {
            m.writeValueAsString(Optional.of("hi"));
        } catch (JsonMappingException e) {
            threw = true;
            msg = String.valueOf(e.getMessage());
        }
        check("optional.unsupported.throws", threw);
        check("optional.unsupported.message", msg.contains("jackson-datatype-jdk8"));
    }

    static void testMiscValueTypes(ObjectMapper m) throws Exception {
        // scalars
        checkEq("misc.int.write", "10", m.writeValueAsString(10));
        checkEq("misc.bool.write", "true", m.writeValueAsString(true));
        checkEq("misc.double.write", "1.5", m.writeValueAsString(1.5));
        checkEq("misc.null.write", "null", m.writeValueAsString(null));
        checkEq("misc.string.escape", "\"a\\\"b\\nc\"", m.writeValueAsString("a\"b\nc"));
        checkEq("misc.int.read", Integer.valueOf(42), m.readValue("42", Integer.class));
        checkEq("misc.bool.read", Boolean.TRUE, m.readValue("true", Boolean.class));
        checkEq("misc.string.read", "x\"y", m.readValue("\"x\\\"y\"", String.class));
        checkEq("misc.long.read", Long.valueOf(9999999999L),
                m.readValue("9999999999", Long.class));

        // Object (untyped) -> LinkedHashMap / ArrayList / Integer / String / Boolean
        Object obj = m.readValue("{\"k\":[1,\"s\",true]}", Object.class);
        check("misc.untyped.is_map", obj instanceof Map);
        Object inner = ((Map<?, ?>) obj).get("k");
        check("misc.untyped.is_list", inner instanceof List);
        checkEq("misc.untyped.list.0", Integer.valueOf(1), ((List<?>) inner).get(0));
        checkEq("misc.untyped.list.1", "s", ((List<?>) inner).get(1));
        checkEq("misc.untyped.list.2", Boolean.TRUE, ((List<?>) inner).get(2));

        // writeValueAsString of a nested collection literal
        Map<String, Object> doc = new LinkedHashMap<>();
        doc.put("title", "t");
        doc.put("count", 2);
        doc.put("items", Arrays.asList("a", "b"));
        checkEq("misc.collection.write",
                "{\"title\":\"t\",\"count\":2,\"items\":[\"a\",\"b\"]}",
                m.writeValueAsString(doc));
    }

    // ----------------------------------------------------------------- driver

    interface Section { void run(ObjectMapper m) throws Exception; }

    static void runSection(String label, ObjectMapper m, Section s) {
        try {
            s.run(m);
        } catch (Throwable t) {
            fail++;
            System.out.println("FAIL section:" + label + " threw " + t);
        }
    }

    public static void main(String[] args) {
        ObjectMapper m = new ObjectMapper();

        runSection("pojoRoundTrip", m, JacksonCarpet::testPojoRoundTrip);
        runSection("annotations", m, JacksonCarpet::testAnnotationsPropertyIgnore);
        runSection("creator", m, JacksonCarpet::testCreator);
        runSection("alias", m, JacksonCarpet::testAlias);
        runSection("accessor", m, JacksonCarpet::testAccessor);
        runSection("include", m, JacksonCarpet::testInclude);
        runSection("format", m, JacksonCarpet::testFormat);
        runSection("anyGetterSetter", m, JacksonCarpet::testAnyGetterSetter);
        runSection("polymorphism", m, JacksonCarpet::testPolymorphism);
        runSection("mapList", m, JacksonCarpet::testMapList);
        runSection("arrays", m, JacksonCarpet::testArrays);
        runSection("convertValue", m, JacksonCarpet::testConvertValue);
        runSection("treeModel", m, JacksonCarpet::testTreeModel);
        runSection("treeBuild", m, JacksonCarpet::testTreeBuild);
        runSection("streamingGenerator", m, JacksonCarpet::testStreamingGenerator);
        runSection("streamingParser", m, JacksonCarpet::testStreamingParser);
        runSection("enum", m, JacksonCarpet::testEnum);
        runSection("bigNumbersAndBytes", m, JacksonCarpet::testBigNumbersAndBytes);
        runSection("readerWriter", m, JacksonCarpet::testReaderWriter);
        runSection("optional", m, JacksonCarpet::testOptional);
        runSection("miscValueTypes", m, JacksonCarpet::testMiscValueTypes);
        runSection("features", m, (mm) -> testFeatures());
        runSection("customSerDe", m, (mm) -> testCustomSerDe());

        System.out.println("JACKSON_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("JACKSON_DONE");
        }
    }
}
