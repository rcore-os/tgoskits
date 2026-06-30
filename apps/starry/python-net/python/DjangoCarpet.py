#!/usr/bin/env python3
# INDUSTRIAL carpet for Django 4.2.30
# Target runtime: musl-native CPython 3.14.3 on StarryOS.
# Drives everything through django.test.Client (no real socket), in-memory sqlite.
import os
import sys
import uuid as uuidmod

# ----------------------------------------------------------------------------
# Self-check harness
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
# Django configuration (no manage.py, no project)
# ----------------------------------------------------------------------------
import django
from django.conf import settings

settings.configure(
    DEBUG=True,
    SECRET_KEY="carpet-fixed-secret-key-deterministic-0123456789",
    USE_TZ=False,
    ALLOWED_HOSTS=["testserver", "localhost", "127.0.0.1"],
    ROOT_URLCONF="__main__",
    DATABASES={
        "default": {
            "ENGINE": "django.db.backends.sqlite3",
            "NAME": ":memory:",
        }
    },
    INSTALLED_APPS=[
        "django.contrib.contenttypes",
        "django.contrib.auth",
        "django.contrib.sessions",
        "django.contrib.messages",
        "__main__",
    ],
    MIDDLEWARE=[
        "django.contrib.sessions.middleware.SessionMiddleware",
        "django.middleware.common.CommonMiddleware",
        "django.contrib.auth.middleware.AuthenticationMiddleware",
        "django.contrib.messages.middleware.MessageMiddleware",
        "__main__.CustomHeaderMiddleware",
    ],
    TEMPLATES=[
        {
            "BACKEND": "django.template.backends.django.DjangoTemplates",
            "DIRS": [],
            "APP_DIRS": False,
            "OPTIONS": {
                "context_processors": [
                    "django.template.context_processors.request",
                    "django.contrib.auth.context_processors.auth",
                    "django.contrib.messages.context_processors.messages",
                ],
                "loaders": [
                    (
                        "django.template.loaders.locmem.Loader",
                        {
                            "greet.html": "Hello {{ name|upper }}! count={{ items|length }}",
                            "cond.html": "{% if flag %}YES{% else %}NO{% endif %}",
                            "loop.html": "{% for i in nums %}{{ i }}{% endfor %}",
                        },
                    )
                ],
            },
        }
    ],
    CACHES={
        "default": {
            "BACKEND": "django.core.cache.backends.locmem.LocMemCache",
            "LOCATION": "carpet-locmem",
        }
    },
    MESSAGE_STORAGE="django.contrib.messages.storage.session.SessionStorage",
)

# ----------------------------------------------------------------------------
# Middleware defined before setup so it is importable by path
# ----------------------------------------------------------------------------


class CustomHeaderMiddleware:
    def __init__(self, get_response):
        self.get_response = get_response

    def __call__(self, request):
        response = self.get_response(request)
        response["X-Custom-Header"] = "carpet-mw"
        return response


django.setup()

# ----------------------------------------------------------------------------
# Imports that require configured settings
# ----------------------------------------------------------------------------
from django.db import models, connection
from django.db.models import Sum, Count, Avg, Max, Min, F, Q
from django.http import (
    HttpResponse,
    JsonResponse,
    HttpResponseRedirect,
    HttpResponsePermanentRedirect,
    HttpResponseBadRequest,
    HttpResponseNotFound,
    Http404,
    QueryDict,
)
from django.shortcuts import redirect, render
from django.urls import path, re_path, include, reverse, resolve
from django.views import View
from django.views.generic import TemplateView
from django.template import Template, Context, engines
from django.template.loader import render_to_string
from django import forms
from django.core import signing
from django.core.signing import Signer, BadSignature
from django.core.cache import cache
from django.core.exceptions import ValidationError
from django.core.validators import MinValueValidator, MaxValueValidator
from django.test import Client
from django.contrib.messages import constants as msg_constants


# ----------------------------------------------------------------------------
# Models
# ----------------------------------------------------------------------------
class Author(models.Model):
    name = models.CharField(max_length=100)
    age = models.IntegerField(default=0)

    class Meta:
        app_label = "__main__"

    def __str__(self):
        return self.name


class Book(models.Model):
    title = models.CharField(max_length=200)
    price = models.IntegerField(default=0)
    author = models.ForeignKey(
        Author, on_delete=models.CASCADE, related_name="books"
    )

    class Meta:
        app_label = "__main__"

    def __str__(self):
        return self.title


# Create tables directly via schema editor (no migration files needed)
with connection.schema_editor() as se:
    se.create_model(Author)
    se.create_model(Book)


# ----------------------------------------------------------------------------
# Views
# ----------------------------------------------------------------------------
def view_int(request, n):
    return JsonResponse({"n": n, "type": type(n).__name__})


def view_str(request, s):
    return JsonResponse({"s": s, "type": type(s).__name__})


def view_slug(request, sl):
    return JsonResponse({"slug": sl})


def view_uuid(request, u):
    return JsonResponse({"uuid": str(u), "type": type(u).__name__})


def view_path(request, p):
    return JsonResponse({"p": p})


def view_re(request, year):
    return JsonResponse({"year": year, "type": type(year).__name__})


def view_plain(request):
    return HttpResponse("plain-ok")


def view_created(request):
    return HttpResponse("created", status=201)


def view_badreq(request):
    return HttpResponseBadRequest("bad")


def view_redirect(request):
    return HttpResponseRedirect("/plain/")


def view_permredirect(request):
    return HttpResponsePermanentRedirect("/plain/")


def view_shortcut_redirect(request):
    return redirect("intview", n=7)


def view_404(request):
    raise Http404("nope")


def view_cookie_set(request):
    resp = HttpResponse("cookie-set")
    resp.set_cookie("session_token", "abc123")
    resp.set_cookie("flag", "on", max_age=3600)
    return resp


def view_cookie_read(request):
    val = request.COOKIES.get("incoming", "none")
    return HttpResponse("cookie:" + val)


def view_echo_get(request):
    return JsonResponse(
        {
            "x": request.GET.get("x"),
            "xs": request.GET.getlist("x"),
            "y": request.GET.get("y", "default"),
        }
    )


def view_methods(request):
    return JsonResponse({"method": request.method})


def view_post_form(request):
    return JsonResponse(
        {
            "name": request.POST.get("name"),
            "age": request.POST.get("age"),
            "method": request.method,
        }
    )


def view_json_body(request):
    import json as _json

    data = _json.loads(request.body.decode("utf-8"))
    return JsonResponse({"received": data, "method": request.method})


def view_json_list(request):
    return JsonResponse([1, 2, 3], safe=False)


def view_headers(request):
    resp = HttpResponse("hdr")
    resp["X-App-Version"] = "4.2.30"
    return resp


def view_render_greet(request):
    return render(request, "greet.html", {"name": "carpet", "items": [1, 2, 3, 4]})


class ClassView(View):
    def get(self, request):
        return HttpResponse("cbv-get")

    def post(self, request):
        return HttpResponse("cbv-post", status=201)


class MyTemplateView(TemplateView):
    template_name = "cond.html"

    def get_context_data(self, **kwargs):
        ctx = super().get_context_data(**kwargs)
        ctx["flag"] = True
        return ctx


# included sub-patterns
included_patterns = [
    path("sub/", view_plain, name="incsub"),
]

urlpatterns = [
    path("int/<int:n>/", view_int, name="intview"),
    path("str/<str:s>/", view_str, name="strview"),
    path("slug/<slug:sl>/", view_slug, name="slugview"),
    path("uuid/<uuid:u>/", view_uuid, name="uuidview"),
    path("rest/<path:p>/", view_path, name="pathview"),
    re_path(r"^re/(?P<year>[0-9]{4})/$", view_re, name="review"),
    path("plain/", view_plain, name="plainview"),
    path("created/", view_created, name="createdview"),
    path("badreq/", view_badreq, name="badreqview"),
    path("redirect/", view_redirect, name="redirectview"),
    path("permredirect/", view_permredirect, name="permredirectview"),
    path("shortredirect/", view_shortcut_redirect, name="shortredirectview"),
    path("notfound/", view_404, name="notfoundview"),
    path("cookieset/", view_cookie_set, name="cookiesetview"),
    path("cookieread/", view_cookie_read, name="cookiereadview"),
    path("echo/", view_echo_get, name="echoview"),
    path("methods/", view_methods, name="methodsview"),
    path("postform/", view_post_form, name="postformview"),
    path("jsonbody/", view_json_body, name="jsonbodyview"),
    path("jsonlist/", view_json_list, name="jsonlistview"),
    path("headers/", view_headers, name="headersview"),
    path("greet/", view_render_greet, name="greetview"),
    path("cbv/", ClassView.as_view(), name="cbview"),
    path("tv/", MyTemplateView.as_view(), name="tview"),
    path("inc/", include(included_patterns)),
]


# ----------------------------------------------------------------------------
# Forms
# ----------------------------------------------------------------------------
class PersonForm(forms.Form):
    name = forms.CharField(max_length=10)
    age = forms.IntegerField(min_value=0, max_value=150)
    email = forms.EmailField()
    score = forms.IntegerField(
        required=False, validators=[MinValueValidator(1), MaxValueValidator(100)]
    )


# ============================================================================
# ASSERTIONS
# ============================================================================
client = Client()

# ---- URL routing: converters --------------------------------------------
r = client.get("/int/42/")
chk(r.status_code == 200, "int_status")
chk(r.json() == {"n": 42, "type": "int"}, "int_converter")

r = client.get("/str/hello/")
chk(r.json() == {"s": "hello", "type": "str"}, "str_converter")

r = client.get("/slug/my-slug-1/")
chk(r.json() == {"slug": "my-slug-1"}, "slug_converter")

_u = "12345678-1234-5678-1234-567812345678"
r = client.get("/uuid/%s/" % _u)
chk(r.json() == {"uuid": _u, "type": "UUID"}, "uuid_converter")

r = client.get("/rest/a/b/c/")
chk(r.json() == {"p": "a/b/c"}, "path_converter")

r = client.get("/re/2026/")
chk(r.json() == {"year": "2026", "type": "str"}, "re_path_named_group")

# str converter must NOT match a slash
r = client.get("/str/a/b/")
chk(r.status_code == 404, "str_no_slash_match")

# ---- reverse() -----------------------------------------------------------
chk(reverse("intview", args=[5]) == "/int/5/", "reverse_int_args")
chk(reverse("intview", kwargs={"n": 9}) == "/int/9/", "reverse_int_kwargs")
chk(reverse("strview", args=["xy"]) == "/str/xy/", "reverse_str")
chk(reverse("pathview", args=["a/b"]) == "/rest/a/b/", "reverse_path")
chk(reverse("review", kwargs={"year": "1999"}) == "/re/1999/", "reverse_re")
chk(reverse("plainview") == "/plain/", "reverse_plain")
chk(reverse("incsub") == "/inc/sub/", "reverse_include")

# ---- resolve() -----------------------------------------------------------
m = resolve("/int/77/")
chk(m.url_name == "intview", "resolve_url_name")
chk(m.kwargs == {"n": 77}, "resolve_kwargs")

# ---- include() routing ---------------------------------------------------
r = client.get("/inc/sub/")
chk(r.status_code == 200 and r.content == b"plain-ok", "include_routing")

# ---- views: function -----------------------------------------------------
r = client.get("/plain/")
chk(r.status_code == 200 and r.content == b"plain-ok", "func_view")

# ---- views: class-based View ---------------------------------------------
r = client.get("/cbv/")
chk(r.status_code == 200 and r.content == b"cbv-get", "cbv_get")
r = client.post("/cbv/")
chk(r.status_code == 201 and r.content == b"cbv-post", "cbv_post")
# unsupported method -> 405
r = client.delete("/cbv/")
chk(r.status_code == 405, "cbv_405")

# ---- views: TemplateView -------------------------------------------------
r = client.get("/tv/")
chk(r.status_code == 200 and r.content == b"YES", "template_view")

# ---- request/response: status codes --------------------------------------
chk(client.get("/created/").status_code == 201, "status_201")
chk(client.get("/badreq/").status_code == 400, "status_400")
chk(client.get("/notfound/").status_code == 404, "status_404_http404")
chk(client.get("/does-not-exist/").status_code == 404, "status_404_nomatch")

# ---- redirects -----------------------------------------------------------
r = client.get("/redirect/")
chk(r.status_code == 302, "redirect_302_status")
chk(r["Location"] == "/plain/", "redirect_302_location")
r = client.get("/permredirect/")
chk(r.status_code == 301, "redirect_301_status")
# redirect() shortcut with reverse of named url
r = client.get("/shortredirect/")
chk(r.status_code == 302 and r["Location"] == "/int/7/", "redirect_shortcut")
# follow the redirect chain
r = client.get("/redirect/", follow=True)
chk(r.status_code == 200 and r.content == b"plain-ok", "redirect_follow_status")
chk(r.redirect_chain == [("/plain/", 302)], "redirect_chain")

# ---- headers & content-type ----------------------------------------------
r = client.get("/headers/")
chk(r["X-App-Version"] == "4.2.30", "custom_header")
chk(r["Content-Type"] == "text/html; charset=utf-8", "default_content_type")
r = client.get("/int/1/")
chk(r["Content-Type"] == "application/json", "json_content_type")

# ---- cookies -------------------------------------------------------------
r = client.get("/cookieset/")
chk(r.cookies["session_token"].value == "abc123", "set_cookie_value")
chk(r.cookies["flag"]["max-age"] == 3600, "set_cookie_maxage")
r = client.get("/cookieread/", HTTP_COOKIE="incoming=fromclient")
chk(r.content == b"cookie:fromclient", "read_cookie")

# ---- HTTP methods via Client ---------------------------------------------
chk(client.get("/methods/").json()["method"] == "GET", "method_get")
chk(client.post("/methods/").json()["method"] == "POST", "method_post")
chk(client.put("/methods/").json()["method"] == "PUT", "method_put")
chk(client.delete("/methods/").json()["method"] == "DELETE", "method_delete")
chk(client.patch("/methods/").json()["method"] == "PATCH", "method_patch")

# ---- POST form data ------------------------------------------------------
r = client.post("/postform/", {"name": "Alice", "age": "30"})
chk(r.json() == {"name": "Alice", "age": "30", "method": "POST"}, "post_form_data")

# ---- POST/PUT/PATCH JSON body --------------------------------------------
r = client.post(
    "/jsonbody/", data='{"k": "v", "n": 5}', content_type="application/json"
)
chk(
    r.json() == {"received": {"k": "v", "n": 5}, "method": "POST"},
    "post_json_body",
)
r = client.put(
    "/jsonbody/", data='{"u": 1}', content_type="application/json"
)
chk(r.json() == {"received": {"u": 1}, "method": "PUT"}, "put_json_body")
r = client.patch(
    "/jsonbody/", data='{"p": true}', content_type="application/json"
)
chk(r.json() == {"received": {"p": True}, "method": "PATCH"}, "patch_json_body")

# ---- request.GET / QueryDict ---------------------------------------------
r = client.get("/echo/?x=10&x=20")
chk(r.json() == {"x": "20", "xs": ["10", "20"], "y": "default"}, "request_get_multi")
r = client.get("/echo/?x=1&y=hi")
chk(r.json() == {"x": "1", "xs": ["1"], "y": "hi"}, "request_get_default")

# QueryDict direct
q = QueryDict("a=1&a=2&b=3")
chk(q.get("a") == "2", "querydict_get_last")
chk(q.getlist("a") == ["1", "2"], "querydict_getlist")
chk(q.get("b") == "3", "querydict_get_single")
chk(q.urlencode() == "a=1&a=2&b=3", "querydict_urlencode")
chk(q.dict() == {"a": "2", "b": "3"}, "querydict_dict")
qm = QueryDict(mutable=True)
qm["k"] = "v"
qm.appendlist("k", "v2")
chk(qm.getlist("k") == ["v", "v2"], "querydict_mutable")

# ---- JsonResponse safe=False for lists -----------------------------------
r = client.get("/jsonlist/")
chk(r.json() == [1, 2, 3], "json_list_safe_false")
chk(r.content == b"[1, 2, 3]", "json_list_content")

# ---- JsonResponse object content -----------------------------------------
jr = JsonResponse({"k": "v", "n": 1})
chk(jr.content == b'{"k": "v", "n": 1}', "jsonresponse_content")
chk(jr["Content-Type"] == "application/json", "jsonresponse_ct")
chk(jr.status_code == 200, "jsonresponse_status")

# ---- HttpResponse basics -------------------------------------------------
hr = HttpResponse("body", status=201, content_type="text/plain")
chk(hr.status_code == 201, "httpresponse_status")
chk(hr.content == b"body", "httpresponse_content")
chk(hr["Content-Type"] == "text/plain", "httpresponse_ct")
chk(HttpResponseNotFound("x").status_code == 404, "httpresponsenotfound")

# ----------------------------------------------------------------------------
# Templates
# ----------------------------------------------------------------------------
t = Template("{{ name|upper }}|{{ items|length }}|{{ missing|default:'NA' }}")
chk(t.render(Context({"name": "abc", "items": [1, 2, 3]})) == "ABC|3|NA", "tpl_filters")

t2 = Template("{% if x %}Y{% else %}N{% endif %}")
chk(t2.render(Context({"x": 1})) == "Y", "tpl_if_true")
chk(t2.render(Context({"x": 0})) == "N", "tpl_if_false")

t3 = Template("{% for i in nums %}{{ i }}{% endfor %}")
chk(t3.render(Context({"nums": [4, 5, 6]})) == "456", "tpl_for")

t4 = Template("{{ a|add:b }}")
chk(t4.render(Context({"a": 3, "b": 4})) == "7", "tpl_add_filter")

t5 = Template("{{ s|lower }}-{{ s|title }}")
chk(t5.render(Context({"s": "HELLO world"})) == "hello world-Hello World", "tpl_lower_title")

t6 = Template("{% for i in nums %}{{ forloop.counter }}{% if not forloop.last %},{% endif %}{% endfor %}")
chk(t6.render(Context({"nums": ["a", "b", "c"]})) == "1,2,3", "tpl_forloop_counter")

# render() shortcut via Client + locmem loader
r = client.get("/greet/")
chk(r.content == b"Hello CARPET! count=4", "render_shortcut")

# render_to_string
chk(render_to_string("loop.html", {"nums": [7, 8, 9]}) == "789", "render_to_string")

# template autoescaping
te = Template("{{ html }}")
chk(te.render(Context({"html": "<b>x</b>"})) == "&lt;b&gt;x&lt;/b&gt;", "tpl_autoescape")

# ----------------------------------------------------------------------------
# ORM
# ----------------------------------------------------------------------------
# create / save
a1 = Author.objects.create(name="Alice", age=30)
a2 = Author.objects.create(name="Bob", age=25)
a3 = Author(name="Carol", age=40)
a3.save()
chk(Author.objects.count() == 3, "orm_count_authors")
chk(a1.pk == 1, "orm_autopk")

b1 = Book.objects.create(title="Django", price=40, author=a1)
b2 = Book.objects.create(title="Flask", price=30, author=a1)
b3 = Book.objects.create(title="Rust", price=50, author=a2)
chk(Book.objects.count() == 3, "orm_count_books")

# get
chk(Author.objects.get(name="Alice").pk == a1.pk, "orm_get")
# get raising DoesNotExist
try:
    Author.objects.get(name="Zzz")
    chk(False, "orm_get_doesnotexist")
except Author.DoesNotExist:
    chk(True, "orm_get_doesnotexist")

# filter / exclude
chk(Book.objects.filter(price__gt=35).count() == 2, "orm_filter_gt")
chk(Book.objects.filter(author=a1).count() == 2, "orm_filter_fk")
chk(Book.objects.exclude(author=a1).count() == 1, "orm_exclude")
chk(Book.objects.filter(title__startswith="D").count() == 1, "orm_startswith")
chk(Book.objects.filter(title__contains="us").count() == 1, "orm_contains")
chk(Book.objects.filter(price__in=[30, 50]).count() == 2, "orm_in")

# order_by
chk(list(Book.objects.order_by("price").values_list("title", flat=True)) == ["Flask", "Django", "Rust"], "orm_order_by_asc")
chk(Book.objects.order_by("-price").first().title == "Rust", "orm_order_by_desc")

# values / values_list
chk(list(Author.objects.order_by("name").values("name")) == [{"name": "Alice"}, {"name": "Bob"}, {"name": "Carol"}], "orm_values")
chk(list(Book.objects.order_by("price").values_list("price", flat=True)) == [30, 40, 50], "orm_values_list_flat")

# aggregate Sum / Count / Avg / Max / Min
chk(Book.objects.aggregate(Sum("price")) == {"price__sum": 120}, "orm_aggregate_sum")
chk(Book.objects.aggregate(Count("id")) == {"id__count": 3}, "orm_aggregate_count")
chk(Book.objects.aggregate(Avg("price")) == {"price__avg": 40.0}, "orm_aggregate_avg")
chk(Book.objects.aggregate(Max("price"), Min("price")) == {"price__max": 50, "price__min": 30}, "orm_aggregate_maxmin")

# annotate (reverse count)
ann = Author.objects.annotate(nbooks=Count("books")).order_by("name")
chk([(a.name, a.nbooks) for a in ann] == [("Alice", 2), ("Bob", 1), ("Carol", 0)], "orm_annotate_count")

# Q objects
chk(Book.objects.filter(Q(price__gt=45) | Q(title="Flask")).count() == 2, "orm_q_or")
chk(Book.objects.filter(Q(price__gte=30) & ~Q(title="Rust")).count() == 2, "orm_q_and_not")

# F expressions
Book.objects.filter(title="Django").update(price=F("price") + 5)
chk(Book.objects.get(title="Django").price == 45, "orm_f_update")
# F comparison
chk(Book.objects.filter(price__gt=F("price") - 1).count() == 3, "orm_f_compare")

# update (bulk) returns number of rows
n = Book.objects.filter(author=a1).update(price=100)
chk(n == 2, "orm_update_returns_count")
chk(list(Book.objects.filter(author=a1).values_list("price", flat=True)) == [100, 100], "orm_update_applied")

# get_or_create
obj, created = Author.objects.get_or_create(name="Alice", defaults={"age": 99})
chk(created is False and obj.pk == a1.pk, "orm_get_or_create_existing")
obj2, created2 = Author.objects.get_or_create(name="Dave", defaults={"age": 50})
chk(created2 is True and obj2.age == 50, "orm_get_or_create_new")
chk(Author.objects.count() == 4, "orm_count_after_goc")

# bulk_create
Book.objects.bulk_create([
    Book(title="Go", price=20, author=a2),
    Book(title="Zig", price=15, author=a2),
])
chk(Book.objects.count() == 5, "orm_bulk_create")
chk(Book.objects.filter(author=a2).count() == 3, "orm_bulk_create_fk")

# FK forward + reverse
bk = Book.objects.get(title="Rust")
chk(bk.author.name == "Bob", "orm_fk_forward")
chk(a1.books.count() == 2, "orm_fk_reverse_count")
chk(set(a2.books.values_list("title", flat=True)) == {"Rust", "Go", "Zig"}, "orm_fk_reverse_set")

# exists()
chk(Book.objects.filter(title="Go").exists() is True, "orm_exists_true")
chk(Book.objects.filter(title="NoSuch").exists() is False, "orm_exists_false")

# first / last
chk(Author.objects.order_by("pk").first().name == "Alice", "orm_first")
chk(Author.objects.order_by("pk").last().name == "Dave", "orm_last")

# delete returns count tuple
del_count, _detail = Book.objects.filter(title="Zig").delete()
chk(del_count == 1, "orm_delete_count")
chk(Book.objects.count() == 4, "orm_count_after_delete")

# cascade delete: deleting author removes their books
pre = Book.objects.count()
a2.delete()
chk(Book.objects.filter(title="Go").exists() is False, "orm_cascade_delete")
chk(Book.objects.count() == pre - 2, "orm_cascade_count")  # Rust + Go remained for a2

# ----------------------------------------------------------------------------
# Forms
# ----------------------------------------------------------------------------
f = PersonForm(data={"name": "Bob", "age": "20", "email": "bob@example.com"})
chk(f.is_valid() is True, "form_valid")
chk(f.cleaned_data == {"name": "Bob", "age": 20, "email": "bob@example.com", "score": None}, "form_cleaned_data")

# invalid: missing email, age out of range, name too long
f2 = PersonForm(data={"name": "x" * 20, "age": "500", "email": "not-an-email"})
chk(f2.is_valid() is False, "form_invalid")
chk("email" in f2.errors, "form_error_email")
chk("age" in f2.errors, "form_error_age")
chk("name" in f2.errors, "form_error_name")
chk(f2.errors["email"] == ["Enter a valid email address."], "form_error_email_msg")
chk(f2.errors["age"] == ["Ensure this value is less than or equal to 150."], "form_error_age_msg")

# required field missing
f3 = PersonForm(data={"age": "10", "email": "a@b.com"})
chk(f3.is_valid() is False, "form_required_missing")
chk(f3.errors["name"] == ["This field is required."], "form_required_msg")

# validators (MinValueValidator/MaxValueValidator on score)
f4 = PersonForm(data={"name": "n", "age": "5", "email": "a@b.com", "score": "200"})
chk(f4.is_valid() is False, "form_validator_max")
chk("score" in f4.errors, "form_validator_score_error")

# field-level: CharField max_length error
cf = forms.CharField(max_length=3)
try:
    cf.clean("toolong")
    chk(False, "form_charfield_maxlen")
except ValidationError as e:
    chk(e.messages == ["Ensure this value has at most 3 characters (it has 7)."], "form_charfield_maxlen")

# IntegerField conversion
ifield = forms.IntegerField()
chk(ifield.clean("42") == 42, "form_integerfield_clean")
try:
    ifield.clean("abc")
    chk(False, "form_integerfield_invalid")
except ValidationError as e:
    chk(e.messages == ["Enter a whole number."], "form_integerfield_invalid")

# EmailField
ef = forms.EmailField()
chk(ef.clean("a@b.com") == "a@b.com", "form_emailfield_valid")

# ----------------------------------------------------------------------------
# Middleware (custom header added to every response)
# ----------------------------------------------------------------------------
r = client.get("/plain/")
chk(r["X-Custom-Header"] == "carpet-mw", "middleware_header")
r = client.get("/int/1/")
chk(r["X-Custom-Header"] == "carpet-mw", "middleware_header_json")

# ----------------------------------------------------------------------------
# Signing
# ----------------------------------------------------------------------------
signed = signing.dumps({"user": "alice", "id": 7})
chk(signing.loads(signed) == {"user": "alice", "id": 7}, "signing_roundtrip")
# tampering raises BadSignature
try:
    signing.loads(signed + "x")
    chk(False, "signing_tamper")
except signing.BadSignature:
    chk(True, "signing_tamper")

s = Signer()
signed2 = s.sign("payload")
chk(signed2.startswith("payload:"), "signer_format")
chk(s.unsign(signed2) == "payload", "signer_unsign")
try:
    s.unsign("payload:bogus")
    chk(False, "signer_bad")
except BadSignature:
    chk(True, "signer_bad")

# sign_object / unsign_object
so = s.sign_object({"a": 1})
chk(s.unsign_object(so) == {"a": 1}, "signer_object_roundtrip")

# ----------------------------------------------------------------------------
# Cache (locmem)
# ----------------------------------------------------------------------------
cache.clear()
chk(cache.get("missing") is None, "cache_get_missing")
chk(cache.get("missing", "fallback") == "fallback", "cache_get_default")
cache.set("k1", "v1")
chk(cache.get("k1") == "v1", "cache_set_get")
chk(cache.add("k1", "other") is False, "cache_add_existing")
chk(cache.add("k2", "v2") is True, "cache_add_new")
chk(cache.get("k2") == "v2", "cache_add_get")
cache.set("num", 10)
chk(cache.incr("num") == 11, "cache_incr")
chk(cache.decr("num", 5) == 6, "cache_decr")
chk(cache.get_many(["k1", "k2"]) == {"k1": "v1", "k2": "v2"}, "cache_get_many")
cache.delete("k1")
chk(cache.get("k1") is None, "cache_delete")
cache.set_many({"m1": 1, "m2": 2})
chk(cache.get("m1") == 1 and cache.get("m2") == 2, "cache_set_many")

# ----------------------------------------------------------------------------
# Messages framework constants (exact level values)
# ----------------------------------------------------------------------------
chk(msg_constants.DEBUG == 10, "msg_debug")
chk(msg_constants.INFO == 20, "msg_info")
chk(msg_constants.SUCCESS == 25, "msg_success")
chk(msg_constants.WARNING == 30, "msg_warning")
chk(msg_constants.ERROR == 40, "msg_error")

# ----------------------------------------------------------------------------
# Final result
# ----------------------------------------------------------------------------
print("DJANGO_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("DJANGO_DONE")
sys.exit(0 if fail == 0 else 1)
