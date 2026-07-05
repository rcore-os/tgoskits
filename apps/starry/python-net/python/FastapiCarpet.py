#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
INDUSTRIAL carpet for FastAPI 0.121.2 + uvicorn 0.38.0 + Pydantic 2.12.3.

TARGET RUNTIME: musl-native CPython 3.14.3 on StarryOS, run as
    python3 /root/.../FastapiCarpet.py

Two layers:
  (A) In-process ASGI driving of the FastAPI app (the same protocol the
      starlette TestClient speaks internally, but WITHOUT httpx -- httpx is
      not present on the target, so we speak ASGI directly). Deterministic.
  (B) A REAL uvicorn ASGI server leg: uvicorn.Server(uvicorn.Config(...))
      in a background thread + a real loopback urllib HTTP GET, proving
      bind/accept/serve on the target net stack.

Every assertion is an exact-value check. No network beyond 127.0.0.1 loopback
to our own uvicorn. No datetime/random/time-based values in assertions. All
paths resolved from tempfile -- nothing hardcoded.
"""

import sys
import os
import json
import socket
import threading
import asyncio
import time
import tempfile
import shutil
import urllib.request
import urllib.error
from contextlib import asynccontextmanager
from enum import Enum
from typing import Optional, List
import uuid as uuidmod

# ----------------------------------------------------------------------------
# self-check harness (mirrors the delivered carpets exactly)
# ----------------------------------------------------------------------------
ok = 0
fail = 0


def chk(cond, name):
    global ok, fail
    if cond:
        ok += 1
    else:
        fail += 1
        print("FAIL " + name)


# ----------------------------------------------------------------------------
# imports under test
# ----------------------------------------------------------------------------
import fastapi
import uvicorn
import pydantic
import starlette
from fastapi import (
    FastAPI,
    APIRouter,
    Path,
    Query,
    Header,
    Cookie,
    Depends,
    HTTPException,
    Request,
    Response,
    status as http_status,
)
from fastapi.responses import JSONResponse, PlainTextResponse, HTMLResponse
from fastapi.encoders import jsonable_encoder
from starlette.middleware.base import BaseHTTPMiddleware
from pydantic import (
    BaseModel,
    Field,
    ValidationError,
    field_validator,
    computed_field,
    ConfigDict,
)

# Version sanity — assert each framework reports a well-formed version (the exact patch floats
# with the Alpine edge repo the target provisions from, so pin the shape, not the string).
import re as _re
def _ver_ok(v):
    return isinstance(v, str) and _re.match(r"^\d+\.\d+", v) is not None
chk(_ver_ok(fastapi.__version__) and int(fastapi.__version__.split(".")[1]) >= 121, "fastapi version >=0.121")
chk(_ver_ok(uvicorn.__version__), "uvicorn version present")
chk(_ver_ok(pydantic.__version__) and pydantic.__version__.startswith("2."), "pydantic 2.x")
chk(_ver_ok(starlette.__version__), "starlette version present")
chk(pydantic.VERSION == pydantic.__version__, "pydantic.VERSION matches")


# ============================================================================
# In-process ASGI client (no httpx dependency)
# ============================================================================
from urllib.parse import urlencode


class _Resp:
    def __init__(self, status, raw_headers, body):
        self.status_code = status
        self.raw_headers = raw_headers  # list[(bytes,bytes)]
        self._body = body
        self.headers = {}
        for k, v in raw_headers:
            self.headers[k.decode("latin-1").lower()] = v.decode("latin-1")

    @property
    def text(self):
        return self._body.decode("utf-8")

    @property
    def content(self):
        return self._body

    def json(self):
        return json.loads(self._body)

    def set_cookies(self):
        return [v.decode("latin-1") for k, v in self.raw_headers
                if k.decode("latin-1").lower() == "set-cookie"]


class ASGIClient:
    """Drives an ASGI app in-process on a dedicated background event loop.
    Runs the lifespan protocol so startup/shutdown + lifespan state work."""

    def __init__(self, app):
        self.app = app
        self.lifespan_state = {}
        self.startup_failed = None
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self._run_loop, daemon=True)
        self.thread.start()
        self._submit(self._startup())

    def _run_loop(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_forever()

    def _submit(self, coro):
        return asyncio.run_coroutine_threadsafe(coro, self.loop).result()

    async def _startup(self):
        self._rq = asyncio.Queue()
        self._su = asyncio.Event()
        self._sd = asyncio.Event()

        scope = {"type": "lifespan", "asgi": {"version": "3.0"},
                 "state": self.lifespan_state}

        async def receive():
            return await self._rq.get()

        async def send(m):
            t = m["type"]
            if t == "lifespan.startup.complete":
                self._su.set()
            elif t == "lifespan.startup.failed":
                self.startup_failed = m
                self._su.set()
            elif t == "lifespan.shutdown.complete":
                self._sd.set()
            elif t == "lifespan.shutdown.failed":
                self._sd.set()

        self._ltask = self.loop.create_task(self.app(scope, receive, send))
        await self._rq.put({"type": "lifespan.startup"})
        await self._su.wait()

    async def _shutdown(self):
        await self._rq.put({"type": "lifespan.shutdown"})
        await self._sd.wait()
        await self._ltask

    def close(self):
        self._submit(self._shutdown())
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=5)

    async def _request(self, method, path, params, body_json, content, headers, cookies):
        hd = [(b"host", b"testserver")]
        body = b""
        if body_json is not None:
            body = json.dumps(body_json).encode("utf-8")
            hd.append((b"content-type", b"application/json"))
        elif content is not None:
            body = content if isinstance(content, bytes) else content.encode("utf-8")
        for k, v in (headers or {}).items():
            hd.append((k.lower().encode("latin-1"), str(v).encode("latin-1")))
        if cookies:
            cs = "; ".join("%s=%s" % (k, v) for k, v in cookies.items())
            hd.append((b"cookie", cs.encode("latin-1")))
        if body and not any(k == b"content-length" for k, _ in hd):
            hd.append((b"content-length", str(len(body)).encode("latin-1")))
        qs = urlencode(params, doseq=True) if params else ""
        scope = {
            "type": "http",
            "asgi": {"version": "3.0", "spec_version": "2.3"},
            "http_version": "1.1",
            "method": method,
            "scheme": "http",
            "path": path,
            "raw_path": path.encode("utf-8"),
            "query_string": qs.encode("latin-1"),
            "root_path": "",
            "headers": hd,
            "client": ("127.0.0.1", 50000),
            "server": ("testserver", 80),
            "state": dict(self.lifespan_state),
        }
        msgs = []
        sent = [False]

        async def receive():
            if not sent[0]:
                sent[0] = True
                return {"type": "http.request", "body": body, "more_body": False}
            return {"type": "http.disconnect"}

        async def send(m):
            msgs.append(m)

        await self.app(scope, receive, send)
        st = None
        rh = []
        rb = b""
        for m in msgs:
            if m["type"] == "http.response.start":
                st = m["status"]
                rh = m.get("headers", [])
            elif m["type"] == "http.response.body":
                rb += m.get("body", b"")
        return _Resp(st, rh, rb)

    def request(self, method, path, params=None, json=None, content=None,
                headers=None, cookies=None):
        return self._submit(self._request(method, path, params, json, content,
                                           headers, cookies))

    def get(self, p, **k):
        return self.request("GET", p, **k)

    def post(self, p, **k):
        return self.request("POST", p, **k)

    def put(self, p, **k):
        return self.request("PUT", p, **k)

    def delete(self, p, **k):
        return self.request("DELETE", p, **k)

    def patch(self, p, **k):
        return self.request("PATCH", p, **k)


# ============================================================================
# Application under test
# ============================================================================
LIFESPAN_EVENTS = []
DEP_TEARDOWN = []


@asynccontextmanager
async def lifespan(app):
    app.state.ready = True
    LIFESPAN_EVENTS.append("startup")
    yield {"lifespan_val": 42}
    LIFESPAN_EVENTS.append("shutdown")


app = FastAPI(title="CarpetAPI", version="3.2.1", lifespan=lifespan)


# ---- middleware: BaseHTTPMiddleware adds a header --------------------------
class HeaderMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request, call_next):
        resp = await call_next(request)
        resp.headers["X-Custom"] = "carpet"
        return resp


app.add_middleware(HeaderMiddleware)


# ---- custom exception + handler -------------------------------------------
class TeapotError(Exception):
    def __init__(self, flavor):
        self.flavor = flavor


@app.exception_handler(TeapotError)
async def teapot_handler(request: Request, exc: TeapotError):
    return JSONResponse(status_code=418, content={"teapot": exc.flavor})


# ---- pydantic models -------------------------------------------------------
class Address(BaseModel):
    city: str
    zipcode: str = Field(min_length=3, max_length=10)


class Item(BaseModel):
    name: str = Field(min_length=2, max_length=50)
    price: float = Field(gt=0)
    tags: List[str] = []
    address: Optional[Address] = None

    @field_validator("name")
    @classmethod
    def name_strip(cls, v):
        return v.strip()


class Rect(BaseModel):
    w: float
    h: float

    @computed_field
    @property
    def area(self) -> float:
        return self.w * self.h


class FullUser(BaseModel):
    name: str
    secret: str
    age: int


class PubUser(BaseModel):
    name: str
    age: int


class Profile(BaseModel):
    name: str
    nickname: Optional[str] = None
    age: int = 0


class Color(str, Enum):
    red = "red"
    green = "green"


# ---- dependencies ----------------------------------------------------------
def get_config():
    return {"env": "prod"}


def common_params(q: Optional[str] = None, limit: int = 10):
    return {"q": q, "limit": limit}


def get_db():
    db = {"conn": "open"}
    try:
        yield db
    finally:
        DEP_TEARDOWN.append("closed")


def dep_b():
    return "B"


def dep_a(b: str = Depends(dep_b)):
    return b + "A"


# ---- routes: verbs ---------------------------------------------------------
@app.get("/ping")
async def ping():
    return {"pong": True}


@app.post("/echo", status_code=201)
async def echo(item: Item):
    return item


@app.put("/put/{pid}")
async def put_route(pid: int):
    return {"method": "PUT", "pid": pid}


@app.delete("/del/{pid}")
async def del_route(pid: int):
    return {"method": "DELETE", "pid": pid}


@app.patch("/patch/{pid}")
async def patch_route(pid: int):
    return {"method": "PATCH", "pid": pid}


# ---- path params -----------------------------------------------------------
@app.get("/int/{x}")
def p_int(x: int):
    return {"v": x, "t": type(x).__name__}


@app.get("/float/{x}")
def p_float(x: float):
    return {"v": x, "t": type(x).__name__}


@app.get("/str/{x}")
def p_str(x: str):
    return {"v": x, "t": type(x).__name__}


@app.get("/bool/{x}")
def p_bool(x: bool):
    return {"v": x, "t": type(x).__name__}


@app.get("/uuid/{x}")
def p_uuid(x: uuidmod.UUID):
    return {"v": str(x), "t": type(x).__name__}


@app.get("/color/{c}")
def p_color(c: Color):
    return {"c": c.value}


@app.get("/ranged/{n}")
def p_ranged(n: int = Path(ge=1, le=10)):
    return {"n": n}


@app.get("/slug/{s}")
def p_slug(s: str = Path(pattern="^[a-z]+$")):
    return {"s": s}


# ---- query params ----------------------------------------------------------
@app.get("/search")
def q_search(q: str = "default", limit: int = 10, opt: Optional[str] = None):
    return {"q": q, "limit": limit, "opt": opt}


@app.get("/multi")
def q_multi(tags: List[str] = Query(default=[])):
    return {"tags": tags}


@app.get("/validated")
def q_validated(code: str = Query(min_length=3, max_length=6)):
    return {"code": code}


@app.get("/aliased")
def q_aliased(item_q: str = Query(alias="item-q")):
    return {"item_q": item_q}


# ---- response model variants ----------------------------------------------
@app.get("/pub", response_model=PubUser)
def r_pub():
    return FullUser(name="neo", secret="trinity", age=37)


@app.get("/excl", response_model=FullUser, response_model_exclude={"secret"})
def r_excl():
    return FullUser(name="neo", secret="trinity", age=37)


@app.get("/incl", response_model=FullUser, response_model_include={"name"})
def r_incl():
    return FullUser(name="neo", secret="trinity", age=37)


@app.get("/profile", response_model=Profile, response_model_exclude_unset=True)
def r_profile():
    return Profile(name="neo")


# ---- custom responses + cookies + headers ---------------------------------
@app.get("/plain", response_class=PlainTextResponse)
def r_plain():
    return "hello-plain"


@app.get("/jsonresp")
def r_jsonresp():
    return JSONResponse(status_code=202, content={"accepted": True},
                        headers={"X-Resp": "jr"})


@app.get("/cookie-set")
def r_cookie_set(response: Response):
    response.set_cookie(key="mycookie", value="cval")
    response.headers["X-Extra"] = "ex"
    return {"set": True}


# ---- header / cookie params ------------------------------------------------
@app.get("/readheader")
def r_readheader(x_token: str = Header(default="none")):
    return {"x_token": x_token}


@app.get("/readcookie")
def r_readcookie(session: str = Cookie(default="anon")):
    return {"session": session}


# ---- dependencies ----------------------------------------------------------
@app.get("/cfg")
def r_cfg(cfg: dict = Depends(get_config)):
    return cfg


@app.get("/common")
def r_common(commons: dict = Depends(common_params)):
    return commons


@app.get("/db")
def r_db(db: dict = Depends(get_db)):
    return {"conn": db["conn"]}


@app.get("/subdep")
def r_subdep(val: str = Depends(dep_a)):
    return {"val": val}


# ---- HTTPException ----------------------------------------------------------
@app.get("/notfound")
def r_notfound():
    raise HTTPException(status_code=404, detail="no such thing")


@app.get("/authheader")
def r_authheader():
    raise HTTPException(status_code=401, detail="nope",
                        headers={"WWW-Authenticate": "Bearer"})


@app.get("/teapot")
def r_teapot():
    raise TeapotError("earl-grey")


# ---- lifespan state --------------------------------------------------------
@app.get("/state")
def r_state(request: Request):
    return {
        "ready": request.app.state.ready,
        "lifespan_val": request.state.lifespan_val,
    }


# ---- async + sync ----------------------------------------------------------
@app.get("/asyncep")
async def r_async():
    return {"kind": "async"}


@app.get("/syncep")
def r_sync():
    return {"kind": "sync"}


# ---- APIRouter -------------------------------------------------------------
router = APIRouter(prefix="/v1", tags=["v1items"])


@router.get("/widgets")
def widgets():
    return {"widgets": [1, 2, 3]}


@router.get("/widgets/{wid}")
def widget(wid: int):
    return {"wid": wid}


app.include_router(router)


# ============================================================================
# LAYER A: in-process ASGI assertions
# ============================================================================
client = ASGIClient(app)

# lifespan startup ran
chk(LIFESPAN_EVENTS == ["startup"], "lifespan startup fired")
chk(app.state.ready is True, "lifespan set app.state.ready")

# ---- verbs ----
r = client.get("/ping")
chk(r.status_code == 200, "GET /ping status 200")
chk(r.json() == {"pong": True}, "GET /ping body")
chk(r.headers.get("content-type") == "application/json", "GET /ping content-type")
chk(r.headers.get("x-custom") == "carpet", "middleware header on /ping")

r = client.post("/echo", json={"name": "  sword  ", "price": 9.5})
chk(r.status_code == 201, "POST /echo custom status 201")
chk(r.json() == {"name": "sword", "price": 9.5, "tags": [], "address": None},
    "POST /echo body (field_validator strip + defaults)")

r = client.put("/put/7")
chk(r.status_code == 200 and r.json() == {"method": "PUT", "pid": 7}, "PUT verb")
r = client.delete("/del/8")
chk(r.status_code == 200 and r.json() == {"method": "DELETE", "pid": 8}, "DELETE verb")
r = client.patch("/patch/9")
chk(r.status_code == 200 and r.json() == {"method": "PATCH", "pid": 9}, "PATCH verb")

# wrong method -> 405
r = client.post("/ping")
chk(r.status_code == 405, "wrong method 405")

# unknown route -> 404
r = client.get("/does-not-exist")
chk(r.status_code == 404, "unknown route 404")

# ---- path params ----
r = client.get("/int/42")
chk(r.status_code == 200 and r.json() == {"v": 42, "t": "int"}, "path int coercion")
r = client.get("/int/notint")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "int_parsing",
    "path int parse 422")

r = client.get("/float/3.14")
chk(r.status_code == 200 and r.json() == {"v": 3.14, "t": "float"}, "path float coercion")

r = client.get("/str/hello")
chk(r.status_code == 200 and r.json() == {"v": "hello", "t": "str"}, "path str")

r = client.get("/bool/true")
chk(r.status_code == 200 and r.json() == {"v": True, "t": "bool"}, "path bool true")
r = client.get("/bool/false")
chk(r.status_code == 200 and r.json() == {"v": False, "t": "bool"}, "path bool false")
r = client.get("/bool/1")
chk(r.json() == {"v": True, "t": "bool"}, "path bool 1->True")
r = client.get("/bool/notbool")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "bool_parsing",
    "path bool parse 422")

U = "12345678-1234-5678-1234-567812345678"
r = client.get("/uuid/" + U)
chk(r.status_code == 200 and r.json() == {"v": U, "t": "UUID"}, "path uuid coercion")
r = client.get("/uuid/not-a-uuid")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "uuid_parsing",
    "path uuid parse 422")

r = client.get("/color/red")
chk(r.status_code == 200 and r.json() == {"c": "red"}, "path enum valid")
r = client.get("/color/blue")
j = r.json()
chk(r.status_code == 422 and j["detail"][0]["type"] == "enum"
    and j["detail"][0]["ctx"] == {"expected": "'red' or 'green'"}, "path enum invalid 422")

r = client.get("/ranged/5")
chk(r.status_code == 200 and r.json() == {"n": 5}, "Path ge/le valid")
r = client.get("/ranged/0")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "greater_than_equal",
    "Path ge fail")
r = client.get("/ranged/11")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "less_than_equal",
    "Path le fail")

r = client.get("/slug/abc")
chk(r.status_code == 200 and r.json() == {"s": "abc"}, "Path pattern match")
r = client.get("/slug/ABC9")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "string_pattern_mismatch",
    "Path pattern mismatch")

# ---- query params ----
r = client.get("/search")
chk(r.json() == {"q": "default", "limit": 10, "opt": None}, "query defaults")
r = client.get("/search", params={"q": "x", "limit": 3, "opt": "y"})
chk(r.json() == {"q": "x", "limit": 3, "opt": "y"}, "query provided")
r = client.get("/search", params={"limit": "notint"})
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "int_parsing",
    "query int parse 422")

r = client.get("/multi", params={"tags": ["a", "b", "c"]})
chk(r.json() == {"tags": ["a", "b", "c"]}, "query list multi")
r = client.get("/multi")
chk(r.json() == {"tags": []}, "query list default empty")

r = client.get("/validated", params={"code": "abcd"})
chk(r.json() == {"code": "abcd"}, "query length valid")
r = client.get("/validated", params={"code": "ab"})
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "string_too_short",
    "query min_length fail")
r = client.get("/validated", params={"code": "abcdefg"})
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "string_too_long",
    "query max_length fail")
r = client.get("/validated")
chk(r.status_code == 422 and r.json()["detail"][0]["type"] == "missing",
    "query required missing 422")

r = client.get("/aliased", params={"item-q": "viaAlias"})
chk(r.json() == {"item_q": "viaAlias"}, "query alias")

# ---- request body validation ----
r = client.post("/echo", json={"name": "ax", "price": 2.0,
                               "address": {"city": "neo", "zipcode": "12345"}})
chk(r.status_code == 201
    and r.json()["address"] == {"city": "neo", "zipcode": "12345"},
    "nested model body")

# missing required field
r = client.post("/echo", json={"price": 5.0})
j = r.json()
chk(r.status_code == 422 and j["detail"][0]["type"] == "missing"
    and j["detail"][0]["loc"] == ["body", "name"], "body missing field 422 loc")
chk(r.headers.get("content-type") == "application/json", "422 content-type json")

# field constraint violations -> exact structure
r = client.post("/echo", json={"name": "x", "price": -3})
j = r.json()["detail"]
chk(r.status_code == 422, "body constraint 422 status")
chk(j[0] == {"type": "string_too_short", "loc": ["body", "name"],
             "msg": "String should have at least 2 characters",
             "input": "x", "ctx": {"min_length": 2}}, "exact name error obj")
chk(j[1] == {"type": "greater_than", "loc": ["body", "price"],
             "msg": "Input should be greater than 0",
             "input": -3, "ctx": {"gt": 0.0}}, "exact price error obj")

# nested field constraint
r = client.post("/echo", json={"name": "ok", "price": 1.0,
                               "address": {"city": "x", "zipcode": "1"}})
j = r.json()["detail"]
chk(r.status_code == 422 and j[0]["loc"] == ["body", "address", "zipcode"]
    and j[0]["type"] == "string_too_short", "nested constraint loc")

# wrong-typed body (not an object) -> model_attributes_type
r = client.post("/echo", json=[1, 2, 3])
chk(r.status_code == 422, "body wrong type 422")

# ---- response model filtering ----
r = client.get("/pub")
chk(r.status_code == 200 and r.json() == {"name": "neo", "age": 37},
    "response_model strips extra field")
r = client.get("/excl")
chk(r.json() == {"name": "neo", "age": 37}, "response_model_exclude")
r = client.get("/incl")
chk(r.json() == {"name": "neo"}, "response_model_include")
r = client.get("/profile")
chk(r.json() == {"name": "neo"}, "response_model_exclude_unset")

# ---- custom responses / headers / cookies ----
r = client.get("/plain")
chk(r.status_code == 200 and r.text == "hello-plain"
    and r.headers.get("content-type") == "text/plain; charset=utf-8",
    "PlainTextResponse")
r = client.get("/jsonresp")
chk(r.status_code == 202 and r.json() == {"accepted": True}
    and r.headers.get("x-resp") == "jr", "JSONResponse custom status+header")
r = client.get("/cookie-set")
sc = r.set_cookies()
chk(len(sc) == 1 and sc[0].startswith("mycookie=cval"), "set_cookie header")
chk(r.headers.get("x-extra") == "ex", "response.headers extra")

# ---- header / cookie params ----
r = client.get("/readheader", headers={"x-token": "abc123"})
chk(r.json() == {"x_token": "abc123"}, "Header param read (underscore->dash)")
r = client.get("/readheader")
chk(r.json() == {"x_token": "none"}, "Header param default")
r = client.get("/readcookie", cookies={"session": "xyz"})
chk(r.json() == {"session": "xyz"}, "Cookie param read")
r = client.get("/readcookie")
chk(r.json() == {"session": "anon"}, "Cookie param default")

# ---- dependencies ----
r = client.get("/cfg")
chk(r.json() == {"env": "prod"}, "Depends simple")
r = client.get("/common", params={"q": "hi", "limit": 5})
chk(r.json() == {"q": "hi", "limit": 5}, "Depends with sub-params")
r = client.get("/subdep")
chk(r.json() == {"val": "BA"}, "sub-dependency chain")

DEP_TEARDOWN.clear()
r = client.get("/db")
chk(r.json() == {"conn": "open"}, "yield dep value")
chk(DEP_TEARDOWN == ["closed"], "yield dep teardown ran")

# dependency_overrides
app.dependency_overrides[get_config] = lambda: {"env": "test"}
r = client.get("/cfg")
chk(r.json() == {"env": "test"}, "dependency_overrides active")
app.dependency_overrides.clear()
r = client.get("/cfg")
chk(r.json() == {"env": "prod"}, "dependency_overrides cleared")

# ---- HTTPException ----
r = client.get("/notfound")
chk(r.status_code == 404 and r.json() == {"detail": "no such thing"},
    "HTTPException status+detail")
r = client.get("/authheader")
chk(r.status_code == 401 and r.headers.get("www-authenticate") == "Bearer",
    "HTTPException custom headers")
r = client.get("/teapot")
chk(r.status_code == 418 and r.json() == {"teapot": "earl-grey"},
    "custom exception handler")

# ---- lifespan state ----
r = client.get("/state")
chk(r.json() == {"ready": True, "lifespan_val": 42},
    "lifespan app.state + yielded request.state")

# ---- async + sync ----
chk(client.get("/asyncep").json() == {"kind": "async"}, "async endpoint")
chk(client.get("/syncep").json() == {"kind": "sync"}, "sync endpoint (threadpool)")

# ---- APIRouter ----
r = client.get("/v1/widgets")
chk(r.status_code == 200 and r.json() == {"widgets": [1, 2, 3]}, "router prefix route")
r = client.get("/v1/widgets/77")
chk(r.json() == {"wid": 77}, "router prefix + path param")

# ---- OpenAPI ----
schema = app.openapi()
chk(schema["openapi"] == "3.1.0", "openapi version 3.1.0")
chk(schema["info"]["title"] == "CarpetAPI" and schema["info"]["version"] == "3.2.1",
    "openapi info")
chk("/ping" in schema["paths"] and "/v1/widgets" in schema["paths"], "openapi paths")
chk("get" in schema["paths"]["/ping"], "openapi path verb")
chk("components" in schema and "schemas" in schema["components"], "openapi components")
chk("Item" in schema["components"]["schemas"], "openapi component Item")
chk(schema["paths"]["/v1/widgets"]["get"]["tags"] == ["v1items"], "router tags in openapi")

r = client.get("/openapi.json")
chk(r.status_code == 200 and r.json()["openapi"] == "3.1.0",
    "/openapi.json via client")
chk(r.headers.get("content-type") == "application/json", "/openapi.json content-type")

r = client.get("/docs")
chk(r.status_code == 200
    and r.headers.get("content-type") == "text/html; charset=utf-8",
    "/docs swagger HTML")
r = client.get("/redoc")
chk(r.status_code == 200, "/redoc HTML")

# caching: openapi() returns same cached object
chk(app.openapi() is schema, "app.openapi() cached")

# ---- jsonable_encoder ----
enc = jsonable_encoder(Item(name="zz", price=1.5))
chk(enc == {"name": "zz", "price": 1.5, "tags": [], "address": None},
    "jsonable_encoder model")

# ---- Form requires python-multipart (env-aware) ----
try:
    import multipart  # noqa
    HAVE_MP = True
except Exception:
    try:
        import python_multipart  # noqa
        HAVE_MP = True
    except Exception:
        HAVE_MP = False

from fastapi import Form
if HAVE_MP:
    formapp = FastAPI()

    @formapp.post("/login")
    def _login(username: str = Form(), password: str = Form()):
        return {"u": username, "p": password}

    fc = ASGIClient(formapp)
    rr = fc.post("/login", content="username=neo&password=pw",
                 headers={"content-type": "application/x-www-form-urlencoded"})
    chk(rr.status_code == 200 and rr.json() == {"u": "neo", "p": "pw"},
        "Form params parsed")
    fc.close()
else:
    raised = None
    import contextlib
    try:
        tmpapp = FastAPI()
        # FastAPI logs the install hint to stderr before raising; silence it.
        with open(os.devnull, "w") as _dn, contextlib.redirect_stderr(_dn):
            @tmpapp.post("/login")
            def _login(username: str = Form()):
                return {"u": username}
    except RuntimeError as e:
        raised = str(e)
    chk(raised is not None and "python-multipart" in raised,
        "Form without multipart raises documented RuntimeError")


# ============================================================================
# Pydantic 2 direct assertions
# ============================================================================
# ValidationError.errors() exact structure
try:
    Item(name="x", price=-1)
    verr = None
except ValidationError as e:
    verr = e
chk(verr is not None, "ValidationError raised")
chk(verr.error_count() == 2, "ValidationError error_count == 2")
chk(verr.title == "Item", "ValidationError title")
errs = verr.errors()
chk(errs[0]["type"] == "string_too_short" and errs[0]["loc"] == ("name",),
    "pydantic err[0] type/loc")
chk(errs[1]["type"] == "greater_than" and errs[1]["ctx"] == {"gt": 0.0},
    "pydantic err[1] type/ctx")
chk(errs[0]["url"] == "https://errors.pydantic.dev/2.12/v/string_too_short",
    "pydantic err url 2.12")
errs_no_url = verr.errors(include_url=False)
chk("url" not in errs_no_url[0], "errors(include_url=False) drops url")

# model_validate / model_dump / model_dump_json
rect = Rect.model_validate({"w": 3, "h": 4})
chk(rect.area == 12.0, "computed_field value")
chk(rect.model_dump() == {"w": 3.0, "h": 4.0, "area": 12.0}, "model_dump w/ computed")
chk(rect.model_dump_json() == '{"w":3.0,"h":4.0,"area":12.0}', "model_dump_json")
rect2 = Rect.model_validate_json('{"w":2,"h":5}')
chk(rect2.area == 10.0, "model_validate_json + computed")

# model_dump include/exclude
fu = FullUser(name="a", secret="s", age=1)
chk(fu.model_dump(exclude={"secret"}) == {"name": "a", "age": 1}, "model_dump exclude")
chk(fu.model_dump(include={"name"}) == {"name": "a"}, "model_dump include")

# field_validator transform applied
it = Item(name="   trimmed   ", price=1.0)
chk(it.name == "trimmed", "field_validator strip applied")

# nested model dump
it2 = Item(name="ok", price=2.0, address=Address(city="c", zipcode="123"))
chk(it2.model_dump()["address"] == {"city": "c", "zipcode": "123"},
    "nested model_dump")

# model_copy
it3 = it2.model_copy(update={"price": 9.0})
chk(it3.price == 9.0 and it2.price == 2.0, "model_copy update")

# model_construct (skip validation)
itc = Item.model_construct(name="raw", price=-99)
chk(itc.name == "raw" and itc.price == -99, "model_construct bypasses validation")

# model_fields metadata
chk(set(["name", "price", "tags", "address"]).issubset(set(Item.model_fields.keys())),
    "model_fields keys")
chk(Item.model_fields["name"].is_required() is True, "model_fields name required")
chk(Item.model_fields["tags"].is_required() is False, "model_fields tags optional")

# model_json_schema: validation mode excludes computed fields
js = Rect.model_json_schema()
chk(js["title"] == "Rect", "model_json_schema title")
chk(js["required"] == ["w", "h"], "model_json_schema required (computed excluded)")
chk("area" not in js["properties"], "validation-mode schema omits computed area")
# serialization mode includes the computed field
js_ser = Rect.model_json_schema(mode="serialization")
chk("area" in js_ser["properties"], "serialization-mode schema includes computed area")
chk(js_ser["required"] == ["w", "h", "area"], "serialization-mode required incl computed")

# ValidationError from bad type
try:
    Address(city="c", zipcode=123456)  # int not str -> but coercible? str strict
    aok = True
except ValidationError:
    aok = False
chk(aok is False, "Address zipcode int rejected (strict str)")

# fields_set tracking
prof = Profile(name="solo")
chk(prof.model_fields_set == {"name"}, "model_fields_set tracks set fields")
chk(prof.model_dump(exclude_unset=True) == {"name": "solo"}, "exclude_unset direct")


# ============================================================================
# LAYER B: REAL uvicorn server leg (bind/accept/serve on target net stack)
# ============================================================================
def _free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


uvi_app = FastAPI()
uvi_app.add_middleware(HeaderMiddleware)


@uvi_app.get("/ping")
async def uvi_ping():
    return {"pong": True, "n": 7}


@uvi_app.get("/items/{iid}")
async def uvi_item(iid: int):
    return {"iid": iid, "double": iid * 2}


PORT = _free_port()
config = uvicorn.Config(uvi_app, host="127.0.0.1", port=PORT,
                        log_level="error", loop="asyncio")
server = uvicorn.Server(config)
server_thread = threading.Thread(target=server.run, daemon=True)
server_thread.start()

# poll until started (bounded; sleep only for polling, never asserted)
started = False
for _ in range(500):
    if server.started:
        started = True
        break
    time.sleep(0.02)
chk(started is True, "uvicorn server.started")

try:
    with urllib.request.urlopen("http://127.0.0.1:%d/ping" % PORT, timeout=5) as resp:
        body = resp.read()
        rstatus = resp.status
        rhdr = resp.headers.get("X-Custom")
        rctype = resp.headers.get("content-type")
    chk(rstatus == 200, "uvicorn real GET /ping 200")
    chk(json.loads(body) == {"pong": True, "n": 7}, "uvicorn real GET /ping json")
    chk(rhdr == "carpet", "uvicorn real GET middleware header")
    chk(rctype == "application/json", "uvicorn real GET content-type")

    with urllib.request.urlopen("http://127.0.0.1:%d/items/21" % PORT, timeout=5) as resp:
        body2 = resp.read()
        rstatus2 = resp.status
    chk(rstatus2 == 200, "uvicorn real GET /items 200")
    chk(json.loads(body2) == {"iid": 21, "double": 42},
        "uvicorn real GET path-param json")

    # 404 over the wire
    got404 = False
    try:
        urllib.request.urlopen("http://127.0.0.1:%d/nope" % PORT, timeout=5)
    except urllib.error.HTTPError as he:
        got404 = (he.code == 404)
    chk(got404 is True, "uvicorn real GET 404")
finally:
    server.should_exit = True
    server_thread.join(timeout=10)

chk(server_thread.is_alive() is False, "uvicorn server thread exited")


# ============================================================================
# shutdown in-process client (runs lifespan shutdown)
# ============================================================================
client.close()
chk(LIFESPAN_EVENTS == ["startup", "shutdown"], "lifespan shutdown fired")


# ============================================================================
# result
# ============================================================================
print("FASTAPI_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("FASTAPI_DONE")
sys.exit(0 if fail == 0 else 1)
