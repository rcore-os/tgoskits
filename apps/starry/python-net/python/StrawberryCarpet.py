#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
INDUSTRIAL carpet for strawberry-graphql 0.316.0 (graphql-core 3.2.6).

Deterministic, self-contained, no network, no datetime/random.
Exercises the public strawberry API surface with exact-value assertions
on result.data / result.errors and on the generated SDL.

Run:  <venv>/bin/python StrawberryCarpet.py
Pass: prints "STRAWBERRY_RESULT ok=N fail=0" then "STRAWBERRY_DONE", exit 0.
"""
import sys
import enum
import json
import base64
import asyncio
import logging
import warnings
from typing import List, Optional, Generic, TypeVar, AsyncGenerator, NewType

# Keep stdout clean: silence deprecation warnings and the graphql error logger
# (strawberry/graphql-core log resolver exceptions to stderr by default).
warnings.filterwarnings("ignore")
logging.disable(logging.CRITICAL)

import strawberry
from strawberry.types import Info
from strawberry.permission import BasePermission
from strawberry.printer import print_schema

ok = 0
fail = 0


def chk(cond, name):
    global ok, fail
    if cond:
        ok += 1
    else:
        fail += 1
        print("FAIL " + name)


# ---------------------------------------------------------------------------
# Version sanity (frameworks must be the pinned ones on host + target)
# ---------------------------------------------------------------------------
import importlib.metadata as _md
chk(_md.version("strawberry-graphql") == "0.316.0", "version.strawberry")
chk(_md.version("graphql-core") == "3.2.6", "version.graphql_core")


# ===========================================================================
# 1. Basic @strawberry.type with typed fields + resolvers; execute_sync
# ===========================================================================
@strawberry.type
class Book:
    title: str
    pages: int


@strawberry.type
class Author:
    name: str

    @strawberry.field
    def books(self) -> List[Book]:
        return [Book(title="GraphQL", pages=42), Book(title="Strawberry", pages=7)]


@strawberry.type
class BasicQuery:
    @strawberry.field
    def book(self) -> Book:
        return Book(title="GraphQL", pages=42)

    @strawberry.field
    def author(self) -> Author:
        return Author(name="Leo")


basic_schema = strawberry.Schema(query=BasicQuery)

r = basic_schema.execute_sync("{ book { title pages } }")
chk(r.errors is None, "basic.errors_none")
chk(r.data == {"book": {"title": "GraphQL", "pages": 42}}, "basic.book_data")
chk(isinstance(r.data, dict), "basic.data_is_dict")

# Nested object + list resolver
r = basic_schema.execute_sync("{ author { name books { title pages } } }")
chk(r.errors is None, "basic.nested_errors_none")
chk(
    r.data
    == {
        "author": {
            "name": "Leo",
            "books": [
                {"title": "GraphQL", "pages": 42},
                {"title": "Strawberry", "pages": 7},
            ],
        }
    },
    "basic.nested_data",
)

# SDL tokens
sdl = basic_schema.as_str()
chk("type Book {" in sdl, "basic.sdl_book")
chk("title: String!" in sdl, "basic.sdl_title")
chk("pages: Int!" in sdl, "basic.sdl_pages")
chk("books: [Book!]!" in sdl, "basic.sdl_books_list")
# print_schema(schema) == schema.as_str()
chk(print_schema(basic_schema) == sdl, "basic.print_schema_eq")


# ===========================================================================
# 2. Arguments: scalars Int/Float/String/Boolean/ID, defaults, Optional, List
# ===========================================================================
@strawberry.input
class Filter:
    tags: List[str]
    limit: Optional[int] = 10


@strawberry.type
class ArgQuery:
    @strawberry.field
    def calc(self, i: int, f: float, s: str, b: bool, the_id: strawberry.ID) -> str:
        return "%d|%s|%s|%s|%s|%s" % (i, f, s, b, the_id, type(the_id).__name__)

    @strawberry.field
    def greet(self, name: str = "World") -> str:
        return "Hello " + name

    @strawberry.field
    def maybe(self, x: Optional[int] = None) -> str:
        return "none" if x is None else "got %d" % x

    @strawberry.field
    def total(self, nums: List[int]) -> int:
        return sum(nums)

    @strawberry.field
    def search(self, filt: Filter) -> str:
        return "%s:%d" % (",".join(filt.tags), filt.limit)


arg_schema = strawberry.Schema(query=ArgQuery)

r = arg_schema.execute_sync('{ calc(i: 5, f: 2.5, s: "hi", b: true, theId: "abc") }')
chk(r.errors is None, "arg.calc_errors_none")
chk(r.data == {"calc": "5|2.5|hi|True|abc|str"}, "arg.calc_data")

# default argument used
chk(arg_schema.execute_sync("{ greet }").data == {"greet": "Hello World"}, "arg.default")
chk(
    arg_schema.execute_sync('{ greet(name: "Leo") }').data == {"greet": "Hello Leo"},
    "arg.explicit",
)

# Optional argument
chk(arg_schema.execute_sync("{ maybe }").data == {"maybe": "none"}, "arg.opt_none")
chk(arg_schema.execute_sync("{ maybe(x: 5) }").data == {"maybe": "got 5"}, "arg.opt_val")

# List scalar argument
chk(
    arg_schema.execute_sync("{ total(nums: [1, 2, 3, 4]) }").data == {"total": 10},
    "arg.list",
)

# Input object with default + list field
chk(
    arg_schema.execute_sync('{ search(filt: {tags: ["x", "y"]}) }').data
    == {"search": "x,y:10"},
    "arg.input_default",
)
chk(
    arg_schema.execute_sync('{ search(filt: {tags: ["z"], limit: 3}) }').data
    == {"search": "z:3"},
    "arg.input_limit",
)

# SDL: input type rendering + arg signatures
asdl = arg_schema.as_str()
chk("input Filter {" in asdl, "arg.sdl_input")
chk("tags: [String!]!" in asdl, "arg.sdl_input_tags")
chk("limit: Int = 10" in asdl, "arg.sdl_input_default")
chk("calc(i: Int!, f: Float!, s: String!, b: Boolean!, theId: ID!): String!" in asdl, "arg.sdl_calc_sig")
chk("greet(name: String! = \"World\"): String!" in asdl, "arg.sdl_greet_default")


# ===========================================================================
# 3. @strawberry.enum + enum_value; enum return, enum arg, enum via variable
# ===========================================================================
@strawberry.enum
class Color(enum.Enum):
    RED = "red"
    GREEN = "green"
    BLUE = "blue"


@strawberry.enum
class Role(enum.Enum):
    ADMIN = strawberry.enum_value("admin", description="Administrator")
    USER = "user"


@strawberry.type
class EnumQuery:
    @strawberry.field
    def color(self) -> Color:
        return Color.GREEN

    @strawberry.field
    def role(self) -> Role:
        return Role.ADMIN

    @strawberry.field
    def move(self, c: Color) -> str:
        return "picked " + c.value


enum_schema = strawberry.Schema(query=EnumQuery)

chk(enum_schema.execute_sync("{ color }").data == {"color": "GREEN"}, "enum.return")
chk(enum_schema.execute_sync("{ role }").data == {"role": "ADMIN"}, "enum.value_return")
chk(
    enum_schema.execute_sync("{ move(c: RED) }").data == {"move": "picked red"},
    "enum.arg_literal",
)
chk(
    enum_schema.execute_sync(
        "query($c: Color!){ move(c: $c) }", variable_values={"c": "BLUE"}
    ).data
    == {"move": "picked blue"},
    "enum.arg_variable",
)
esdl = enum_schema.as_str()
chk("enum Color {" in esdl, "enum.sdl_color")
chk("  RED\n  GREEN\n  BLUE" in esdl, "enum.sdl_members")
chk('"""Administrator"""' in esdl, "enum.sdl_value_desc")


# ===========================================================================
# 4. @strawberry.interface + implementing types (inline fragments)
# ===========================================================================
@strawberry.interface
class Node:
    id: strawberry.ID


@strawberry.type
class Post(Node):
    title: str


@strawberry.type
class Comment(Node):
    body: str


@strawberry.type
class IfaceQuery:
    @strawberry.field
    def node(self, as_post: bool) -> Node:
        if as_post:
            return Post(id=strawberry.ID("1"), title="hello")
        return Comment(id=strawberry.ID("2"), body="hi there")


iface_schema = strawberry.Schema(query=IfaceQuery, types=[Post, Comment])

r = iface_schema.execute_sync(
    "{ node(asPost: true) { __typename id ... on Post { title } } }"
)
chk(r.errors is None, "iface.errors_none")
chk(
    r.data == {"node": {"__typename": "Post", "id": "1", "title": "hello"}},
    "iface.post",
)
r = iface_schema.execute_sync(
    "{ node(asPost: false) { __typename id ... on Comment { body } } }"
)
chk(
    r.data == {"node": {"__typename": "Comment", "id": "2", "body": "hi there"}},
    "iface.comment",
)
ifsdl = iface_schema.as_str()
chk("interface Node {" in ifsdl, "iface.sdl_interface")
chk("type Post implements Node {" in ifsdl, "iface.sdl_post_impl")
chk("type Comment implements Node {" in ifsdl, "iface.sdl_comment_impl")


# ===========================================================================
# 5. @strawberry.union
# ===========================================================================
@strawberry.type
class Audio:
    duration: int


@strawberry.type
class Video:
    length: int


from typing import Annotated, Union

Media = Annotated[Union[Audio, Video], strawberry.union("Media")]


@strawberry.type
class UnionQuery:
    @strawberry.field
    def media(self, kind: str) -> Media:
        if kind == "audio":
            return Audio(duration=10)
        return Video(length=20)


union_schema = strawberry.Schema(query=UnionQuery)

r = union_schema.execute_sync(
    '{ media(kind: "audio") { __typename ... on Audio { duration } ... on Video { length } } }'
)
chk(r.errors is None, "union.errors_none")
chk(r.data == {"media": {"__typename": "Audio", "duration": 10}}, "union.audio")
r = union_schema.execute_sync(
    '{ media(kind: "video") { __typename ... on Audio { duration } ... on Video { length } } }'
)
chk(r.data == {"media": {"__typename": "Video", "length": 20}}, "union.video")
chk("union Media = Audio | Video" in union_schema.as_str(), "union.sdl")


# ===========================================================================
# 6. Custom scalars (strawberry.scalar with serialize / parse_value)
#    Base64 (bytes) + JSON (object)
# ===========================================================================
Base64 = strawberry.scalar(
    NewType("Base64", bytes),
    name="Base64",
    serialize=lambda v: base64.b64encode(v).decode("ascii"),
    parse_value=lambda v: base64.b64decode(v),
)

JSON = strawberry.scalar(
    NewType("JSON", object),
    name="JSON",
    serialize=lambda v: v,
    parse_value=lambda v: v,
)


@strawberry.type
class ScalarQuery:
    @strawberry.field
    def echo_b64(self, data: Base64) -> Base64:
        # data is decoded bytes; serialize re-encodes -> round trips input
        return data

    @strawberry.field
    def upper_b64(self, data: Base64) -> Base64:
        return data.upper()

    @strawberry.field
    def echo_json(self, data: JSON) -> JSON:
        return data


scalar_schema = strawberry.Schema(query=ScalarQuery)

# "aGVsbG8=" base64-decodes to b"hello"; echo re-encodes to the same string
chk(
    scalar_schema.execute_sync('{ echoB64(data: "aGVsbG8=") }').data
    == {"echoB64": "aGVsbG8="},
    "scalar.b64_roundtrip",
)
# upper: b"hello" -> b"HELLO" -> base64 "SEVMTE8="
chk(
    base64.b64encode(b"HELLO").decode("ascii") == "SEVMTE8=",
    "scalar.b64_expected_const",
)
chk(
    scalar_schema.execute_sync('{ upperB64(data: "aGVsbG8=") }').data
    == {"upperB64": "SEVMTE8="},
    "scalar.b64_transform",
)
# JSON scalar: object literal in / out
chk(
    scalar_schema.execute_sync("{ echoJson(data: {a: 1, b: [2, 3], c: \"x\"}) }").data
    == {"echoJson": {"a": 1, "b": [2, 3], "c": "x"}},
    "scalar.json_roundtrip",
)
ssdl = scalar_schema.as_str()
chk("scalar Base64" in ssdl, "scalar.sdl_base64")
chk("scalar JSON" in ssdl, "scalar.sdl_json")


# ===========================================================================
# 7. Mutation type + execute a mutation
# ===========================================================================
@strawberry.input
class CreateUserInput:
    name: str
    age: int = 0


@strawberry.type
class UserT:
    name: str
    age: int


@strawberry.type
class Query:
    @strawberry.field
    def ping(self) -> str:
        return "pong"


@strawberry.type
class Mutation:
    @strawberry.mutation
    def add(self, a: int, b: int) -> int:
        return a + b

    @strawberry.mutation
    def create_user(self, data: CreateUserInput) -> UserT:
        return UserT(name=data.name, age=data.age)


mut_schema = strawberry.Schema(query=Query, mutation=Mutation)

r = mut_schema.execute_sync("mutation { add(a: 2, b: 3) }")
chk(r.errors is None, "mut.add_errors_none")
chk(r.data == {"add": 5}, "mut.add_data")

r = mut_schema.execute_sync(
    'mutation { createUser(data: {name: "Ann", age: 30}) { name age } }'
)
chk(r.data == {"createUser": {"name": "Ann", "age": 30}}, "mut.create_user")
# default field in input
r = mut_schema.execute_sync(
    'mutation { createUser(data: {name: "Bob"}) { name age } }'
)
chk(r.data == {"createUser": {"name": "Bob", "age": 0}}, "mut.create_user_default")

# Mutation via variables
r = mut_schema.execute_sync(
    "mutation($a: Int!, $b: Int!){ add(a: $a, b: $b) }",
    variable_values={"a": 10, "b": 7},
)
chk(r.data == {"add": 17}, "mut.add_variables")

msdl = mut_schema.as_str()
chk("type Mutation {" in msdl, "mut.sdl_mutation")
chk("type Query {" in msdl, "mut.sdl_query")
# Canonical root names (Query/Mutation) -> graphql-core omits the explicit schema block.
chk("schema {" not in msdl, "mut.sdl_no_schema_block")
chk("add(a: Int!, b: Int!): Int!" in msdl, "mut.sdl_add_sig")


# ===========================================================================
# 8. Field aliases, name=, description, deprecation_reason, resolver=
# ===========================================================================
def answer_resolver(self) -> int:
    return 42


@strawberry.type
class MetaQuery:
    answer: int = strawberry.field(resolver=answer_resolver)

    @strawberry.field
    def greet(self) -> str:
        return "hi"

    @strawberry.field(name="customName", description="a field", deprecation_reason="old")
    def renamed(self) -> int:
        return 7


meta_schema = strawberry.Schema(query=MetaQuery)

# resolver= function form
chk(meta_schema.execute_sync("{ answer }").data == {"answer": 42}, "meta.resolver_fn")
# alias
chk(
    meta_schema.execute_sync("{ g: greet }").data == {"g": "hi"},
    "meta.alias",
)
# two aliases of the same field
chk(
    meta_schema.execute_sync("{ a: greet b: greet }").data == {"a": "hi", "b": "hi"},
    "meta.alias_two",
)
# renamed field
chk(
    meta_schema.execute_sync("{ customName }").data == {"customName": 7},
    "meta.renamed",
)
metasdl = meta_schema.as_str()
chk('"""a field"""' in metasdl, "meta.sdl_description")
chk('customName: Int! @deprecated(reason: "old")' in metasdl, "meta.sdl_deprecated")


# ===========================================================================
# 9. Info / context_value (resolver reads context + field metadata)
# ===========================================================================
@strawberry.type
class CtxQuery:
    @strawberry.field
    def whoami(self, info: Info) -> str:
        return info.context["user"]

    @strawberry.field
    def field_meta(self, info: Info) -> str:
        return "%s|%s" % (info.field_name, info.python_name)


ctx_schema = strawberry.Schema(query=CtxQuery)

r = ctx_schema.execute_sync("{ whoami }", context_value={"user": "leo"})
chk(r.errors is None, "ctx.errors_none")
chk(r.data == {"whoami": "leo"}, "ctx.whoami")
# field_name is the GraphQL (camelCase) name, python_name is the snake_case name
r = ctx_schema.execute_sync("{ fieldMeta }")
chk(r.data == {"fieldMeta": "fieldMeta|field_meta"}, "ctx.field_meta")


# ===========================================================================
# 10. Permission classes (BasePermission)
# ===========================================================================
class IsAuthenticated(BasePermission):
    message = "User is not authenticated"

    def has_permission(self, source, info, **kwargs) -> bool:
        return info.context.get("user") == "admin"


@strawberry.type
class PermQuery:
    @strawberry.field(permission_classes=[IsAuthenticated])
    def secret(self) -> str:
        return "top-secret"


perm_schema = strawberry.Schema(query=PermQuery)

r = perm_schema.execute_sync("{ secret }", context_value={"user": "admin"})
chk(r.errors is None, "perm.allow_errors_none")
chk(r.data == {"secret": "top-secret"}, "perm.allow_data")

r = perm_schema.execute_sync("{ secret }", context_value={"user": "guest"})
chk(r.data is None, "perm.deny_data_none")
chk(r.errors is not None and len(r.errors) == 1, "perm.deny_one_error")
chk(r.errors[0].message == "User is not authenticated", "perm.deny_message")


# ===========================================================================
# 11. Error handling: resolver raising, validation error, arg type error
# ===========================================================================
@strawberry.type
class ErrQuery:
    @strawberry.field
    def boom(self) -> str:
        raise ValueError("kaboom")

    @strawberry.field
    def calc(self, i: int) -> int:
        return i * 2


err_schema = strawberry.Schema(query=ErrQuery)

# resolver raises -> data None, single error, exact message + path
r = err_schema.execute_sync("{ boom }")
chk(r.data is None, "err.raise_data_none")
chk(len(r.errors) == 1, "err.raise_one")
chk(r.errors[0].message == "kaboom", "err.raise_message")
chk(r.errors[0].path == ["boom"], "err.raise_path")

# validation error: unknown field (never reaches a resolver)
r = err_schema.execute_sync("{ nope }")
chk(r.data is None, "err.validation_data_none")
# Strawberry uses the Python class name (ErrQuery) as the GraphQL root type name.
chk(r.errors[0].message == "Cannot query field 'nope' on type 'ErrQuery'.", "err.validation_msg")

# argument type error
r = err_schema.execute_sync('{ calc(i: "notint") }')
chk(r.data is None, "err.argtype_data_none")
chk(
    r.errors[0].message == 'Int cannot represent non-integer value: "notint"',
    "err.argtype_msg",
)


# ===========================================================================
# 12. Variables + variable coercion
# ===========================================================================
@strawberry.type
class VarQuery:
    @strawberry.field
    def add(self, a: int, b: int) -> int:
        return a + b

    @strawberry.field
    def join(self, parts: List[str]) -> str:
        return "-".join(parts)


var_schema = strawberry.Schema(query=VarQuery)

r = var_schema.execute_sync(
    "query($a: Int!, $b: Int!){ add(a: $a, b: $b) }", variable_values={"a": 4, "b": 6}
)
chk(r.data == {"add": 10}, "var.scalar")
# variable inside a list literal (coercion of nested values)
r = var_schema.execute_sync(
    'query($p: String!){ join(parts: [$p, "y", "z"]) }', variable_values={"p": "x"}
)
chk(r.data == {"join": "x-y-z"}, "var.in_list")
# whole-list variable
r = var_schema.execute_sync(
    "query($p: [String!]!){ join(parts: $p) }", variable_values={"p": ["a", "b"]}
)
chk(r.data == {"join": "a-b"}, "var.list_whole")
# missing required variable -> error
r = var_schema.execute_sync("query($a: Int!, $b: Int!){ add(a: $a, b: $b) }", variable_values={"a": 1})
chk(r.data is None, "var.missing_data_none")
chk(
    r.errors[0].message == "Variable '$b' of required type 'Int!' was not provided.",
    "var.missing_msg",
)


# ===========================================================================
# 13. Introspection
# ===========================================================================
introspect_schema = mut_schema  # has query + mutation
r = introspect_schema.execute_sync(
    "{ __schema { queryType { name } mutationType { name } subscriptionType { name } } }"
)
chk(
    r.data
    == {
        "__schema": {
            "queryType": {"name": "Query"},
            "mutationType": {"name": "Mutation"},
            "subscriptionType": None,
        }
    },
    "introspect.schema_roots",
)
# __typename on the root operation (root type keeps the Python class name)
chk(
    basic_schema.execute_sync("{ __typename }").data == {"__typename": "BasicQuery"},
    "introspect.root_typename",
)
# __type by name
r = enum_schema.execute_sync(
    '{ __type(name: "Color") { name kind enumValues { name } } }'
)
chk(
    r.data
    == {
        "__type": {
            "name": "Color",
            "kind": "ENUM",
            "enumValues": [
                {"name": "RED"},
                {"name": "GREEN"},
                {"name": "BLUE"},
            ],
        }
    },
    "introspect.type_enum",
)
# __type of an object: field names
r = basic_schema.execute_sync('{ __type(name: "Book") { kind fields { name } } }')
chk(
    r.data
    == {"__type": {"kind": "OBJECT", "fields": [{"name": "title"}, {"name": "pages"}]}},
    "introspect.type_object_fields",
)


# ===========================================================================
# 14. Generic types  ->  T-parametrised type naming (Connection[User] = UserConnection)
# ===========================================================================
T = TypeVar("T")


@strawberry.type
class Connection(Generic[T]):
    items: List[T]
    total: int


@strawberry.type
class GUser:
    name: str


@strawberry.type
class GenQuery:
    @strawberry.field
    def users(self) -> Connection[GUser]:
        return Connection(items=[GUser(name="a"), GUser(name="b")], total=2)


gen_schema = strawberry.Schema(query=GenQuery)
r = gen_schema.execute_sync("{ users { items { name } total } }")
chk(r.errors is None, "gen.errors_none")
chk(
    r.data == {"users": {"items": [{"name": "a"}, {"name": "b"}], "total": 2}},
    "gen.data",
)
gsdl = gen_schema.as_str()
chk("type GUserConnection {" in gsdl, "gen.sdl_concrete_name")
chk("users: GUserConnection!" in gsdl, "gen.sdl_field_type")


# ===========================================================================
# 15. Async resolvers via schema.execute (await) + subscription
# ===========================================================================
@strawberry.type
class AsyncQuery:
    @strawberry.field
    async def slow_hello(self, name: str = "World") -> str:
        await asyncio.sleep(0)
        return "Hello " + name

    @strawberry.field
    async def slow_sum(self, a: int, b: int) -> int:
        await asyncio.sleep(0)
        return a + b


@strawberry.type
class CounterSub:
    @strawberry.subscription
    async def counter(self, n: int = 3) -> AsyncGenerator[int, None]:
        for i in range(n):
            yield i


async_schema = strawberry.Schema(
    query=AsyncQuery, subscription=CounterSub
)


async def _run_async():
    out = {}
    r1 = await async_schema.execute("{ slowHello }")
    out["hello"] = (r1.errors, r1.data)
    r2 = await async_schema.execute(
        '{ slowHello(name: "Leo") sum: slowSum(a: 3, b: 4) }'
    )
    out["multi"] = (r2.errors, r2.data)
    # subscription stream
    sub = await async_schema.subscribe("subscription { counter(n: 4) }")
    vals = []
    async for res in sub:
        vals.append(res.data)
    out["sub"] = vals
    return out


_async = asyncio.run(_run_async())
chk(_async["hello"][0] is None, "async.hello_errors_none")
chk(_async["hello"][1] == {"slowHello": "Hello World"}, "async.hello_data")
chk(_async["multi"][1] == {"slowHello": "Hello Leo", "sum": 7}, "async.multi_data")
chk(
    _async["sub"]
    == [{"counter": 0}, {"counter": 1}, {"counter": 2}, {"counter": 3}],
    "async.subscription_stream",
)
asdl2 = async_schema.as_str()
chk("type CounterSub {" in asdl2, "async.sdl_subscription_type")
chk("subscription: CounterSub" in asdl2, "async.sdl_schema_block")


# ===========================================================================
# 16. Schema config: auto_camel_case behaviour (snake_case python -> camelCase GQL)
#     and disabling it via StrawberryConfig
# ===========================================================================
from strawberry.schema.config import StrawberryConfig


@strawberry.type
class SnakeQuery:
    @strawberry.field
    def full_name(self) -> str:
        return "Leo Cheng"


camel_schema = strawberry.Schema(query=SnakeQuery)
chk(
    camel_schema.execute_sync("{ fullName }").data == {"fullName": "Leo Cheng"},
    "config.camel_default",
)
chk("fullName: String!" in camel_schema.as_str(), "config.camel_sdl")

snake_schema = strawberry.Schema(
    query=SnakeQuery, config=StrawberryConfig(auto_camel_case=False)
)
chk(
    snake_schema.execute_sync("{ full_name }").data == {"full_name": "Leo Cheng"},
    "config.snake_data",
)
chk("full_name: String!" in snake_schema.as_str(), "config.snake_sdl")


# ===========================================================================
# 17. root_value: resolver receives the provided root object as `self`
# ===========================================================================
class RootObj:
    motd = "be excellent"


@strawberry.type
class RootQuery:
    @strawberry.field
    def motd(self, info: Info) -> str:
        return info.root_value.motd


root_schema = strawberry.Schema(query=RootQuery)
r = root_schema.execute_sync("{ motd }", root_value=RootObj())
chk(r.data == {"motd": "be excellent"}, "root.value")


# ===========================================================================
# 18. operation_name selects the right operation among multiple
# ===========================================================================
doc = "query A { ping } query B { pong: ping }"


@strawberry.type
class OpQuery:
    @strawberry.field
    def ping(self) -> str:
        return "pong"


op_schema = strawberry.Schema(query=OpQuery)
r = op_schema.execute_sync(doc, operation_name="A")
chk(r.data == {"ping": "pong"}, "op.select_a")
r = op_schema.execute_sync(doc, operation_name="B")
chk(r.data == {"pong": "pong"}, "op.select_b")


# ===========================================================================
# Finalise
# ===========================================================================
print("STRAWBERRY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("STRAWBERRY_DONE")
sys.exit(0 if fail == 0 else 1)
