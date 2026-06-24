// Carpet-grade deterministic exercise of github.com/gin-gonic/gin v1.12.0.
//
// Determinism rules:
//   - gin.SetMode(gin.TestMode); NO real socket. Everything driven through
//     net/http/httptest (httptest.NewRecorder + httptest.NewRequest) +
//     (*Engine).ServeHTTP, or gin.CreateTestContext / CreateTestContextOnly.
//   - No timestamps / addresses / map-iteration order / randomness in output.
//     gin.H bodies are kept single-key or use structs / sorted JSON so byte
//     output is stable.
//   - Every assertion prints "ok: <label> = <value>"; a global counter is
//     printed at the very end as GIN_COUNT=<n>.
package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"html/template"
	"io"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing/fstest"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/gin-gonic/gin/binding"
	"google.golang.org/protobuf/types/known/wrapperspb"
)

// gin_do drives one request through an engine and returns the recorder.
func gin_do(r *gin.Engine, method, target string, body io.Reader, headers map[string]string) *httptest.ResponseRecorder {
	w := httptest.NewRecorder()
	req := httptest.NewRequest(method, target, body)
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	r.ServeHTTP(w, req)
	return w
}

// gin_ct returns the response Content-Type header.
func gin_ct(w *httptest.ResponseRecorder) string { return w.Header().Get("Content-Type") }

func runFrameworkGin() {
	gin.SetMode(gin.TestMode)
	// Determinism: quiet, no color, suppress debug stdout.
	gin.DisableConsoleColor()
	// Globally silence gin's default writers. Recovery stack traces and
	// Logger lines carry timestamps/addresses (non-deterministic) and must
	// never reach our stdout/stderr. Tests that need to observe log output
	// route it into their own explicit *bytes.Buffer instead.
	gin.DefaultWriter = io.Discard
	gin.DefaultErrorWriter = io.Discard

	engineConstruction()
	routingMethods()
	routingParamsWildcardsNoRouteRedirect()
	routingGroups()
	middlewareChain()
	bindingShould()
	bindingBindAndTags()
	renderJSONFamily()
	renderXMLYAMLStringDataHTML()
	renderTOMLBSONProtoBufSSE()
	renderFileAndStatic()
	renderNegotiate()
	renderRedirectStatusHeader()
	contextQuery()
	contextFormPostParams()
	contextHeadersCookiesGetSet()
	typedGetters()
	errorSurface()
	clientIPProxy()
	rawAndMultipart()
	handlerIntrospection()
	wrappersAndModeHelpers()
	loggerVariants()
}

// ---------------------------------------------------------------------------
// Engine construction & ServeHTTP harness
// ---------------------------------------------------------------------------

func engineConstruction() {
	// gin.New: empty middleware chain.
	r := gin.New()
	r.GET("/ok", func(c *gin.Context) { c.String(200, "ok") })
	w := gin_do(r, "GET", "/ok", nil, nil)
	fwOK("New/ServeHTTP code", w.Code)
	fwOK("New/ServeHTTP body", w.Body.String())

	// gin.Default: Logger + Recovery attached; a panicking handler -> 500.
	// (DefaultWriter/DefaultErrorWriter are globally io.Discard - see main.)
	d := gin.Default()
	d.GET("/panic", func(c *gin.Context) { panic("boom") })
	w = gin_do(d, "GET", "/panic", nil, nil)
	fwOK("Default recovers panic code", w.Code)

	// ServeHTTP implements http.Handler.
	var _ http.Handler = r
	fwOK("Engine implements http.Handler", true)

	// gin.CreateTestContext: unit-test a handler without routing.
	w2 := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w2)
	c.Request = httptest.NewRequest("GET", "/unit", nil)
	(func(c *gin.Context) { c.String(202, "unit") })(c)
	fwOK("CreateTestContext code", w2.Code)
	fwOK("CreateTestContext body", w2.Body.String())

	// gin.CreateTestContextOnly: fresh context bound to an existing engine.
	eng := gin.New()
	w3 := httptest.NewRecorder()
	c3 := gin.CreateTestContextOnly(w3, eng)
	c3.Request = httptest.NewRequest("GET", "/only", nil)
	c3.String(203, "only")
	fwOK("CreateTestContextOnly code", w3.Code)
	fwOK("CreateTestContextOnly body", w3.Body.String())

	// SetMode / Mode / mode constants.
	fwOK("Mode after SetMode(TestMode)", gin.Mode())
	fwOK("TestMode const", gin.TestMode)
	fwOK("DebugMode const", gin.DebugMode)
	fwOK("ReleaseMode const", gin.ReleaseMode)
}

// ---------------------------------------------------------------------------
// Routing - HTTP methods
// ---------------------------------------------------------------------------

func routingMethods() {
	r := gin.New()
	r.GET("/ping", func(c *gin.Context) { c.String(200, "pong") })
	r.POST("/items", func(c *gin.Context) { c.String(201, "created") })
	r.PUT("/items/:id", func(c *gin.Context) { c.String(200, "put:"+c.Param("id")) })
	r.DELETE("/items/:id", func(c *gin.Context) { c.Status(204) })
	r.PATCH("/items/:id", func(c *gin.Context) { c.String(200, "patch:"+c.Param("id")) })
	r.HEAD("/ping", func(c *gin.Context) { c.String(200, "pong") })
	r.OPTIONS("/ping", func(c *gin.Context) { c.Status(204) })

	w := gin_do(r, "GET", "/ping", nil, nil)
	fwOK("GET code", w.Code)
	fwOK("GET body", w.Body.String())

	w = gin_do(r, "POST", "/items", strings.NewReader("x"), nil)
	fwOK("POST code", w.Code)
	fwOK("POST body", w.Body.String())

	w = gin_do(r, "PUT", "/items/1", nil, nil)
	fwOK("PUT code", w.Code)
	fwOK("PUT body", w.Body.String())

	w = gin_do(r, "DELETE", "/items/9", nil, nil)
	fwOK("DELETE code", w.Code)
	fwOK("DELETE body empty", w.Body.Len() == 0)

	w = gin_do(r, "PATCH", "/items/7", nil, nil)
	fwOK("PATCH code", w.Code)
	fwOK("PATCH body", w.Body.String())

	w = gin_do(r, "HEAD", "/ping", nil, nil)
	fwOK("HEAD code", w.Code)
	fwOK("HEAD body (recorder retains)", w.Body.String())

	w = gin_do(r, "OPTIONS", "/ping", nil, nil)
	fwOK("OPTIONS code", w.Code)

	// Handle: arbitrary method string, including a custom verb.
	h := gin.New()
	h.Handle("GET", "/h", func(c *gin.Context) { c.String(200, "h") })
	h.Handle("CUSTOM", "/c", func(c *gin.Context) { c.String(200, "custom") })
	w = gin_do(h, "GET", "/h", nil, nil)
	fwOK("Handle GET code", w.Code)
	w = gin_do(h, "CUSTOM", "/c", nil, nil)
	fwOK("Handle custom-verb code", w.Code)
	fwOK("Handle custom-verb body", w.Body.String())

	// Any: all 9 anyMethods hit the handler.
	a := gin.New()
	a.Any("/any", func(c *gin.Context) { c.String(200, c.Request.Method) })
	for _, m := range []string{"GET", "POST", "PUT", "PATCH", "HEAD", "OPTIONS", "DELETE", "CONNECT", "TRACE"} {
		w = gin_do(a, m, "/any", nil, nil)
		fwOK("Any "+m+" code", w.Code)
		if m != "HEAD" { // HEAD body identical; assert echo for the rest
			fwOK("Any "+m+" echo", w.Body.String())
		}
	}

	// Match: only listed methods; others 404 (no NoMethod, default).
	m := gin.New()
	m.Match([]string{"GET", "POST"}, "/m", func(c *gin.Context) { c.String(200, "matched") })
	w = gin_do(m, "GET", "/m", nil, nil)
	fwOK("Match GET code", w.Code)
	w = gin_do(m, "POST", "/m", nil, nil)
	fwOK("Match POST code", w.Code)
	w = gin_do(m, "DELETE", "/m", nil, nil)
	fwOK("Match DELETE (unmatched) code", w.Code)
}

// ---------------------------------------------------------------------------
// Routing - path params, wildcards, NoRoute/NoMethod, redirects, Routes()
// ---------------------------------------------------------------------------

func routingParamsWildcardsNoRouteRedirect() {
	r := gin.New()
	r.GET("/users/:id", func(c *gin.Context) {
		c.String(200, c.Param("id")+"|"+c.Param("missing")+"|"+c.FullPath())
	})
	r.GET("/files/*filepath", func(c *gin.Context) { c.String(200, c.Param("filepath")) })

	w := gin_do(r, "GET", "/users/42", nil, nil)
	fwOK("path-param id|missing|fullpath", w.Body.String())

	w = gin_do(r, "GET", "/files/a/b/c.txt", nil, nil)
	fwOK("wildcard filepath", w.Body.String())

	// wildcard root redirect: /files -> /files/ (301 for GET).
	w = gin_do(r, "GET", "/files", nil, nil)
	fwOK("wildcard no-trailing redirect code", w.Code)
	fwOK("wildcard redirect Location", w.Header().Get("Location"))

	// NoRoute custom handler.
	nr := gin.New()
	nr.NoRoute(func(c *gin.Context) { c.String(404, "nope") })
	w = gin_do(nr, "GET", "/does-not-exist", nil, nil)
	fwOK("NoRoute code", w.Code)
	fwOK("NoRoute body", w.Body.String())

	// default 404 (no NoRoute).
	def := gin.New()
	def.GET("/x", func(c *gin.Context) { c.Status(200) })
	w = gin_do(def, "GET", "/nope", nil, nil)
	fwOK("default 404 code", w.Code)

	// NoMethod with HandleMethodNotAllowed=true.
	nm := gin.New()
	nm.HandleMethodNotAllowed = true
	nm.GET("/x", func(c *gin.Context) { c.Status(200) })
	nm.NoMethod(func(c *gin.Context) { c.String(405, "bad method") })
	w = gin_do(nm, "POST", "/x", nil, nil)
	fwOK("NoMethod code", w.Code)
	fwOK("NoMethod body", w.Body.String())

	// HandleMethodNotAllowed=false -> 404 for wrong method.
	nm2 := gin.New()
	nm2.GET("/x", func(c *gin.Context) { c.Status(200) })
	w = gin_do(nm2, "POST", "/x", nil, nil)
	fwOK("MethodNotAllowed disabled -> code", w.Code)

	// RedirectTrailingSlash (default true): GET /foo/ -> 301 Location /foo.
	rts := gin.New()
	rts.GET("/foo", func(c *gin.Context) { c.Status(200) })
	w = gin_do(rts, "GET", "/foo/", nil, nil)
	fwOK("RedirectTrailingSlash GET code", w.Code)
	fwOK("RedirectTrailingSlash GET Location", w.Header().Get("Location"))
	// non-GET trailing slash redirect -> 307.
	rts.POST("/bar", func(c *gin.Context) { c.Status(200) })
	w = gin_do(rts, "POST", "/bar/", nil, nil)
	fwOK("RedirectTrailingSlash POST code", w.Code)

	// RedirectFixedPath: cleans path case/extra-slash, redirects.
	rfp := gin.New()
	rfp.RedirectFixedPath = true
	rfp.GET("/path", func(c *gin.Context) { c.String(200, "p") })
	w = gin_do(rfp, "GET", "/PATH", nil, nil)
	fwOK("RedirectFixedPath code", w.Code)
	fwOK("RedirectFixedPath Location", w.Header().Get("Location"))

	// RemoveExtraSlash: //double//slash matched.
	res := gin.New()
	res.RemoveExtraSlash = true
	res.GET("/seg", func(c *gin.Context) { c.String(200, "seg") })
	w = gin_do(res, "GET", "//seg", nil, nil)
	fwOK("RemoveExtraSlash code", w.Code)

	// Routes() registration record (deterministic, no socket).
	reg := gin.New()
	reg.GET("/a", func(c *gin.Context) {})
	reg.POST("/b/:id", func(c *gin.Context) {})
	ri := reg.Routes()
	// sort to be deterministic regardless of internal ordering
	sort.Slice(ri, func(i, j int) bool {
		if ri[i].Method != ri[j].Method {
			return ri[i].Method < ri[j].Method
		}
		return ri[i].Path < ri[j].Path
	})
	fwOK("Routes() len", len(ri))
	fwOK("Routes()[0] Method", ri[0].Method)
	fwOK("Routes()[0] Path", ri[0].Path)
	fwOK("Routes()[0] Handler non-empty", ri[0].Handler != "")
	fwOK("Routes()[0] HandlerFunc non-nil", ri[0].HandlerFunc != nil)
}

// ---------------------------------------------------------------------------
// Routing - groups
// ---------------------------------------------------------------------------

func routingGroups() {
	r := gin.New()
	v1 := r.Group("/v1")
	v1.GET("/ping", func(c *gin.Context) { c.String(200, "v1pong") })
	w := gin_do(r, "GET", "/v1/ping", nil, nil)
	fwOK("Group /v1/ping code", w.Code)
	fwOK("Group /v1/ping body", w.Body.String())
	w = gin_do(r, "GET", "/ping", nil, nil)
	fwOK("Group prefix required (/ping 404)", w.Code)

	// Nested groups + group-scoped middleware.
	var order []string
	r2 := gin.New()
	admin := r2.Group("/admin", func(c *gin.Context) { order = append(order, "mw"); c.Next() })
	sub := admin.Group("/users")
	sub.GET("/:id", func(c *gin.Context) { order = append(order, "h"); c.String(200, c.Param("id")+"|"+c.FullPath()) })
	order = nil
	w = gin_do(r2, "GET", "/admin/users/9", nil, nil)
	fwOK("Nested group body (id|fullpath)", w.Body.String())
	fwOK("Nested group mw-before-h order", strings.Join(order, ","))

	// BasePath.
	g := r.Group("/api/v2")
	fwOK("Group BasePath", g.BasePath())
	fwOK("Engine root BasePath", r.BasePath())

	// Group-level Use applies only inside the group.
	r3 := gin.New()
	gg := r3.Group("/g")
	gg.Use(func(c *gin.Context) { c.Set("k", 1); c.Next() })
	gg.GET("/x", func(c *gin.Context) {
		v := c.GetInt("k")
		c.JSON(200, gin.H{"k": v})
	})
	r3.GET("/out", func(c *gin.Context) {
		_, exists := c.Get("k")
		c.JSON(200, gin.H{"k": exists})
	})
	w = gin_do(r3, "GET", "/g/x", nil, nil)
	fwOK("Group Use inside body", w.Body.String())
	w = gin_do(r3, "GET", "/out", nil, nil)
	fwOK("Group Use outside body", w.Body.String())
}

// ---------------------------------------------------------------------------
// Middleware - chain control
// ---------------------------------------------------------------------------

func middlewareChain() {
	// Engine.Use global, registration order.
	var order []string
	r := gin.New()
	r.Use(func(c *gin.Context) { order = append(order, "mw"); c.Next() })
	r.GET("/x", func(c *gin.Context) { order = append(order, "h"); c.Status(200) })
	order = nil
	w := gin_do(r, "GET", "/x", nil, nil)
	fwOK("Engine.Use order", strings.Join(order, ","))
	fwOK("Engine.Use code", w.Code)

	// Next: before/handler/after.
	var seq []string
	r2 := gin.New()
	r2.Use(func(c *gin.Context) { seq = append(seq, "before"); c.Next(); seq = append(seq, "after") })
	r2.GET("/n", func(c *gin.Context) { seq = append(seq, "handler"); c.Status(200) })
	seq = nil
	gin_do(r2, "GET", "/n", nil, nil)
	fwOK("Next sequence", strings.Join(seq, ","))

	// Abort: downstream not invoked.
	reached := false
	r3 := gin.New()
	r3.Use(func(c *gin.Context) {
		c.Status(401)
		fwOK("IsAborted before Abort", c.IsAborted())
		c.Abort()
		fwOK("IsAborted after Abort", c.IsAborted())
	})
	r3.GET("/a", func(c *gin.Context) { reached = true; c.Status(200) })
	w = gin_do(r3, "GET", "/a", nil, nil)
	fwOK("Abort downstream-not-reached", !reached)
	fwOK("Abort code", w.Code)

	// AbortWithStatus.
	reached = false
	r4 := gin.New()
	r4.Use(func(c *gin.Context) { c.AbortWithStatus(403) })
	r4.GET("/a", func(c *gin.Context) { reached = true })
	w = gin_do(r4, "GET", "/a", nil, nil)
	fwOK("AbortWithStatus code", w.Code)
	fwOK("AbortWithStatus downstream-not-reached", !reached)

	// AbortWithStatusJSON.
	r5 := gin.New()
	r5.Use(func(c *gin.Context) { c.AbortWithStatusJSON(401, gin.H{"error": "unauthorized"}) })
	r5.GET("/a", func(c *gin.Context) {})
	w = gin_do(r5, "GET", "/a", nil, nil)
	fwOK("AbortWithStatusJSON code", w.Code)
	fwOK("AbortWithStatusJSON ct", gin_ct(w))
	fwOK("AbortWithStatusJSON body", w.Body.String())

	// AbortWithStatusPureJSON: HTML unescaped in body, chain aborted.
	reached = false
	r5b := gin.New()
	r5b.Use(func(c *gin.Context) { c.AbortWithStatusPureJSON(401, gin.H{"html": "<b>"}) })
	r5b.GET("/a", func(c *gin.Context) { reached = true })
	w = gin_do(r5b, "GET", "/a", nil, nil)
	fwOK("AbortWithStatusPureJSON code", w.Code)
	fwOK("AbortWithStatusPureJSON unescaped body", strings.TrimRight(w.Body.String(), "\n"))
	fwOK("AbortWithStatusPureJSON aborted", !reached)

	// AbortWithError -> 500, error pushed.
	r6 := gin.New()
	var errsLen int
	r6.Use(func(c *gin.Context) {
		c.AbortWithError(500, errors.New("boom"))
		errsLen = len(c.Errors)
	})
	r6.GET("/a", func(c *gin.Context) {})
	w = gin_do(r6, "GET", "/a", nil, nil)
	fwOK("AbortWithError code", w.Code)
	fwOK("AbortWithError errors-len", errsLen)

	// Custom middleware: Set/MustGet across chain.
	r7 := gin.New()
	r7.Use(func(c *gin.Context) { c.Set("user", "alice"); c.Next() })
	r7.GET("/u", func(c *gin.Context) { c.String(200, c.MustGet("user").(string)) })
	w = gin_do(r7, "GET", "/u", nil, nil)
	fwOK("Custom mw value passing", w.Body.String())

	// Recovery: recovers panic -> 500. (DefaultErrorWriter is io.Discard.)
	r8 := gin.New()
	r8.Use(gin.Recovery())
	r8.GET("/p", func(c *gin.Context) { panic("x") })
	w = gin_do(r8, "GET", "/p", nil, nil)
	fwOK("Recovery code", w.Code)

	// CustomRecovery: handler-defined JSON 500.
	r9 := gin.New()
	r9.Use(gin.CustomRecovery(func(c *gin.Context, err any) {
		c.AbortWithStatusJSON(500, gin.H{"err": "recovered"})
	}))
	r9.GET("/p", func(c *gin.Context) { panic("x") })
	w = gin_do(r9, "GET", "/p", nil, nil)
	fwOK("CustomRecovery code", w.Code)
	fwOK("CustomRecovery body", w.Body.String())

	// LoggerWithWriter into in-memory buffer.
	buf := &bytes.Buffer{}
	r10 := gin.New()
	r10.Use(gin.LoggerWithWriter(buf))
	r10.GET("/log", func(c *gin.Context) { c.Status(200) })
	gin_do(r10, "GET", "/log", nil, nil)
	logged := buf.String()
	fwOK("LoggerWithWriter has 200", strings.Contains(logged, "200"))
	fwOK("LoggerWithWriter has path", strings.Contains(logged, "/log"))
	fwOK("LoggerWithWriter has GET", strings.Contains(logged, "GET"))

	// BasicAuth valid + invalid.
	r11 := gin.New()
	r11.Use(gin.BasicAuth(gin.Accounts{"u": "p"}))
	r11.GET("/secure", func(c *gin.Context) { c.String(200, c.MustGet(gin.AuthUserKey).(string)) })
	// valid
	wv := httptest.NewRecorder()
	rqv := httptest.NewRequest("GET", "/secure", nil)
	rqv.SetBasicAuth("u", "p")
	r11.ServeHTTP(wv, rqv)
	fwOK("BasicAuth valid code", wv.Code)
	fwOK("BasicAuth valid user", wv.Body.String())
	// invalid
	wi := httptest.NewRecorder()
	rqi := httptest.NewRequest("GET", "/secure", nil)
	rqi.SetBasicAuth("u", "wrong")
	r11.ServeHTTP(wi, rqi)
	fwOK("BasicAuth invalid code", wi.Code)
	fwOK("BasicAuth WWW-Authenticate present", wi.Header().Get("WWW-Authenticate") != "")
}

// ---------------------------------------------------------------------------
// Binding & validation - ShouldBind* (no abort on error)
// ---------------------------------------------------------------------------

type gin_jsonIn struct {
	Name string `json:"name" binding:"required"`
}

func bindingShould() {
	// ShouldBindJSON valid + malformed + missing-required.
	r := gin.New()
	r.POST("/j", func(c *gin.Context) {
		var in gin_jsonIn
		if err := c.ShouldBindJSON(&in); err != nil {
			c.JSON(400, gin.H{"err": "bad"})
			return
		}
		c.JSON(200, gin.H{"name": in.Name})
	})
	w := gin_do(r, "POST", "/j", strings.NewReader(`{"name":"a"}`), map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBindJSON valid code", w.Code)
	fwOK("ShouldBindJSON valid body", w.Body.String())
	w = gin_do(r, "POST", "/j", strings.NewReader(`{`), map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBindJSON malformed code", w.Code)
	w = gin_do(r, "POST", "/j", strings.NewReader(`{}`), map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBindJSON missing-required code", w.Code)

	// ShouldBindQuery.
	rq := gin.New()
	rq.GET("/q", func(c *gin.Context) {
		var q struct {
			Page int `form:"page"`
		}
		err := c.ShouldBindQuery(&q)
		c.JSON(200, gin.H{"page": q.Page, "err": err == nil})
	})
	w = gin_do(rq, "GET", "/q?page=3", nil, nil)
	fwOK("ShouldBindQuery body", gin_sortedJSON(w.Body.Bytes()))

	// ShouldBindUri.
	ru := gin.New()
	ru.GET("/u/:id", func(c *gin.Context) {
		var u struct {
			ID string `uri:"id" binding:"required"`
		}
		err := c.ShouldBindUri(&u)
		c.JSON(200, gin.H{"id": u.ID, "err": err == nil})
	})
	w = gin_do(ru, "GET", "/u/abc", nil, nil)
	fwOK("ShouldBindUri body", gin_sortedJSON(w.Body.Bytes()))

	// ShouldBindHeader present + missing-required.
	rh := gin.New()
	rh.GET("/h", func(c *gin.Context) {
		var h struct {
			Token string `header:"X-Token" binding:"required"`
		}
		err := c.ShouldBindHeader(&h)
		if err != nil {
			c.JSON(400, gin.H{"err": "bad"})
			return
		}
		c.JSON(200, gin.H{"token": h.Token})
	})
	w = gin_do(rh, "GET", "/h", nil, map[string]string{"X-Token": "t"})
	fwOK("ShouldBindHeader present body", w.Body.String())
	w = gin_do(rh, "GET", "/h", nil, nil)
	fwOK("ShouldBindHeader missing code", w.Code)

	// ShouldBind content-type aware: form + json.
	rb := gin.New()
	rb.POST("/b", func(c *gin.Context) {
		var v struct {
			Name string `form:"name" json:"name"`
		}
		err := c.ShouldBind(&v)
		c.JSON(200, gin.H{"name": v.Name, "err": err == nil})
	})
	w = gin_do(rb, "POST", "/b", strings.NewReader("name=formval"),
		map[string]string{"Content-Type": "application/x-www-form-urlencoded"})
	fwOK("ShouldBind form body", w.Body.String())
	w = gin_do(rb, "POST", "/b", strings.NewReader(`{"name":"jsonval"}`),
		map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBind json body", w.Body.String())
	// ShouldBind on GET -> Form(query).
	rb.GET("/b", func(c *gin.Context) {
		var v struct {
			Name string `form:"name"`
		}
		_ = c.ShouldBind(&v)
		c.String(200, v.Name)
	})
	w = gin_do(rb, "GET", "/b?name=queryval", nil, nil)
	fwOK("ShouldBind GET->query body", w.Body.String())

	// ShouldBindWith (no response touch) vs MustBindWith (400 on fail).
	rw := gin.New()
	rw.POST("/sbw", func(c *gin.Context) {
		var v gin_jsonIn
		err := c.ShouldBindWith(&v, binding.JSON)
		c.JSON(200, gin.H{"name": v.Name, "err": err == nil})
	})
	w = gin_do(rw, "POST", "/sbw", strings.NewReader(`{"name":"z"}`), map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBindWith body", w.Body.String())

	rmb := gin.New()
	var mbErrs int
	rmb.POST("/mbw", func(c *gin.Context) {
		var v gin_jsonIn
		_ = c.MustBindWith(&v, binding.JSON)
		mbErrs = len(c.Errors)
	})
	w = gin_do(rmb, "POST", "/mbw", strings.NewReader(`{}`), map[string]string{"Content-Type": "application/json"})
	fwOK("MustBindWith fail code", w.Code)
	fwOK("MustBindWith errors pushed", mbErrs)

	// ShouldBindBodyWithJSON twice (cached body re-readable).
	rc := gin.New()
	rc.POST("/cache", func(c *gin.Context) {
		var a, b gin_jsonIn
		e1 := c.ShouldBindBodyWithJSON(&a)
		e2 := c.ShouldBindBodyWithJSON(&b)
		c.JSON(200, gin.H{"a": a.Name, "b": b.Name, "ok": e1 == nil && e2 == nil})
	})
	w = gin_do(rc, "POST", "/cache", strings.NewReader(`{"name":"twice"}`), map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBindBodyWithJSON twice body", gin_sortedJSON(w.Body.Bytes()))

	// ShouldBindBodyWith with explicit binding (generic re-readable variant).
	rc2 := gin.New()
	rc2.POST("/cache2", func(c *gin.Context) {
		var a, b gin_jsonIn
		e1 := c.ShouldBindBodyWith(&a, binding.JSON)
		e2 := c.ShouldBindBodyWith(&b, binding.JSON)
		c.JSON(200, gin.H{"ok": e1 == nil && e2 == nil && a.Name == b.Name})
	})
	w = gin_do(rc2, "POST", "/cache2", strings.NewReader(`{"name":"both"}`), map[string]string{"Content-Type": "application/json"})
	fwOK("ShouldBindBodyWith twice body", w.Body.String())

	// ShouldBindPlain / BindPlain: text/plain into a *string.
	rp := gin.New()
	rp.POST("/plain", func(c *gin.Context) {
		var s string
		err := c.ShouldBindPlain(&s)
		c.JSON(200, gin.H{"s": s, "err": err == nil})
	})
	w = gin_do(rp, "POST", "/plain", strings.NewReader("plain-text"), map[string]string{"Content-Type": "text/plain"})
	fwOK("ShouldBindPlain body", gin_sortedJSON(w.Body.Bytes()))
}

// ---------------------------------------------------------------------------
// Binding & validation - Bind* (auto 400) and validator tags
// ---------------------------------------------------------------------------

func bindingBindAndTags() {
	// BindJSON auto-400 on malformed.
	r := gin.New()
	r.POST("/bind", func(c *gin.Context) {
		var v gin_jsonIn
		if err := c.BindJSON(&v); err != nil {
			return // gin already wrote 400
		}
		c.JSON(200, gin.H{"name": v.Name})
	})
	w := gin_do(r, "POST", "/bind", strings.NewReader(`{`), map[string]string{"Content-Type": "application/json"})
	fwOK("BindJSON malformed auto-400", w.Code)
	w = gin_do(r, "POST", "/bind", strings.NewReader(`{"name":"good"}`), map[string]string{"Content-Type": "application/json"})
	fwOK("BindJSON valid code", w.Code)

	// BindQuery / BindUri / BindHeader (success paths).
	rq := gin.New()
	rq.GET("/bq", func(c *gin.Context) {
		var v struct {
			P int `form:"p"`
		}
		if c.BindQuery(&v) == nil {
			c.String(200, fmt.Sprintf("%d", v.P))
		}
	})
	w = gin_do(rq, "GET", "/bq?p=7", nil, nil)
	fwOK("BindQuery body", w.Body.String())

	ru := gin.New()
	ru.GET("/bu/:id", func(c *gin.Context) {
		var v struct {
			ID string `uri:"id"`
		}
		if c.BindUri(&v) == nil {
			c.String(200, v.ID)
		}
	})
	w = gin_do(ru, "GET", "/bu/zz", nil, nil)
	fwOK("BindUri body", w.Body.String())

	rhh := gin.New()
	rhh.GET("/bh", func(c *gin.Context) {
		var v struct {
			T string `header:"X-T"`
		}
		if c.BindHeader(&v) == nil {
			c.String(200, v.T)
		}
	})
	w = gin_do(rhh, "GET", "/bh", nil, map[string]string{"X-T": "hdrval"})
	fwOK("BindHeader body", w.Body.String())

	// required tag.
	type Req struct {
		Name string `json:"name" binding:"required"`
	}
	fwOK("tag:required omitted -> err", gin_bindJSONErr(&Req{}, `{}`) != nil)
	fwOK("tag:required present -> nil", gin_bindJSONErr(&Req{}, `{"name":"x"}`) == nil)

	// min/max numeric.
	type Age struct {
		Age int `json:"age" binding:"min=18,max=65"`
	}
	fwOK("tag:min numeric fail", strings.Contains(gin_errString(gin_bindJSONErr(&Age{}, `{"age":10}`)), "min"))
	fwOK("tag:max numeric fail", strings.Contains(gin_errString(gin_bindJSONErr(&Age{}, `{"age":70}`)), "max"))
	fwOK("tag:min/max numeric ok", gin_bindJSONErr(&Age{}, `{"age":30}`) == nil)

	// min/max string length.
	type Nm struct {
		Name string `json:"name" binding:"min=3,max=10"`
	}
	fwOK("tag:min string fail", strings.Contains(gin_errString(gin_bindJSONErr(&Nm{}, `{"name":"ab"}`)), "min"))
	fwOK("tag:min/max string ok", gin_bindJSONErr(&Nm{}, `{"name":"abcd"}`) == nil)

	// email / oneof / gte / lte / len / eqfield.
	type Em struct {
		Email string `json:"email" binding:"required,email"`
	}
	fwOK("tag:email invalid", strings.Contains(gin_errString(gin_bindJSONErr(&Em{}, `{"email":"nope"}`)), "email"))
	fwOK("tag:email valid", gin_bindJSONErr(&Em{}, `{"email":"a@b.com"}`) == nil)

	type On struct {
		Status string `json:"status" binding:"oneof=a b c"`
	}
	fwOK("tag:oneof invalid", strings.Contains(gin_errString(gin_bindJSONErr(&On{}, `{"status":"z"}`)), "oneof"))
	fwOK("tag:oneof valid", gin_bindJSONErr(&On{}, `{"status":"b"}`) == nil)

	type Gl struct {
		N int `json:"n" binding:"gte=1,lte=5"`
	}
	fwOK("tag:gte fail", strings.Contains(gin_errString(gin_bindJSONErr(&Gl{}, `{"n":0}`)), "gte"))
	fwOK("tag:lte fail", strings.Contains(gin_errString(gin_bindJSONErr(&Gl{}, `{"n":9}`)), "lte"))
	fwOK("tag:gte/lte ok", gin_bindJSONErr(&Gl{}, `{"n":3}`) == nil)

	type Ln struct {
		Code string `json:"code" binding:"len=4"`
	}
	fwOK("tag:len fail", strings.Contains(gin_errString(gin_bindJSONErr(&Ln{}, `{"code":"abc"}`)), "len"))
	fwOK("tag:len ok", gin_bindJSONErr(&Ln{}, `{"code":"abcd"}`) == nil)

	type Eq struct {
		Pwd     string `json:"pwd" binding:"required"`
		Confirm string `json:"confirm" binding:"eqfield=Pwd"`
	}
	fwOK("tag:eqfield fail", strings.Contains(gin_errString(gin_bindJSONErr(&Eq{}, `{"pwd":"a","confirm":"b"}`)), "eqfield"))
	fwOK("tag:eqfield ok", gin_bindJSONErr(&Eq{}, `{"pwd":"a","confirm":"a"}`) == nil)

	// DisableBindValidation: required no longer enforced (global; reset after).
	origValidator := binding.Validator
	gin.DisableBindValidation()
	type Dv struct {
		Name string `json:"name" binding:"required"`
	}
	fwOK("DisableBindValidation skips required", gin_bindJSONErr(&Dv{}, `{}`) == nil)
	binding.Validator = origValidator // restore default validator

	// EnableJsonDecoderDisallowUnknownFields: extra field -> err. (global; reset)
	gin.EnableJsonDecoderDisallowUnknownFields()
	type Uk struct {
		Name string `json:"name"`
	}
	fwOK("DisallowUnknownFields extra -> err", gin_bindJSONErr(&Uk{}, `{"name":"x","extra":1}`) != nil)
	binding.EnableDecoderDisallowUnknownFields = false
	fwOK("DisallowUnknownFields disabled extra -> nil", gin_bindJSONErr(&Uk{}, `{"name":"x","extra":1}`) == nil)
}

// ---------------------------------------------------------------------------
// Rendering - JSON family
// ---------------------------------------------------------------------------

func renderJSONFamily() {
	r := gin.New()
	r.GET("/json", func(c *gin.Context) { c.JSON(200, gin.H{"message": "pong"}) })
	r.GET("/json-esc", func(c *gin.Context) { c.JSON(200, gin.H{"html": "<b>&"}) })
	r.GET("/indent", func(c *gin.Context) { c.IndentedJSON(200, gin.H{"a": 1}) })
	r.GET("/pure", func(c *gin.Context) { c.PureJSON(200, gin.H{"html": "<b>"}) })
	r.GET("/secure", func(c *gin.Context) { c.SecureJSON(200, []string{"a", "b"}) })
	r.GET("/ascii", func(c *gin.Context) { c.AsciiJSON(200, gin.H{"lang": "中文"}) })
	r.GET("/jsonp", func(c *gin.Context) { c.JSONP(200, gin.H{"a": 1}) })

	w := gin_do(r, "GET", "/json", nil, nil)
	fwOK("JSON code", w.Code)
	fwOK("JSON ct", gin_ct(w))
	fwOK("JSON body", w.Body.String())

	w = gin_do(r, "GET", "/json-esc", nil, nil)
	fwOK("JSON escapes HTML", w.Body.String())

	w = gin_do(r, "GET", "/indent", nil, nil)
	fwOK("IndentedJSON ct", gin_ct(w))
	fwOK("IndentedJSON body", strings.ReplaceAll(w.Body.String(), "\n", "\\n"))

	w = gin_do(r, "GET", "/pure", nil, nil)
	fwOK("PureJSON ct", gin_ct(w))
	fwOK("PureJSON unescaped body", strings.TrimRight(w.Body.String(), "\n"))

	w = gin_do(r, "GET", "/secure", nil, nil)
	fwOK("SecureJSON ct", gin_ct(w))
	fwOK("SecureJSON body", w.Body.String())

	// SecureJsonPrefix override.
	rp := gin.New()
	rp.SecureJsonPrefix(")]}',\n")
	rp.GET("/s", func(c *gin.Context) { c.SecureJSON(200, []string{"a"}) })
	w = gin_do(rp, "GET", "/s", nil, nil)
	fwOK("SecureJSON custom prefix body", strings.ReplaceAll(w.Body.String(), "\n", "\\n"))

	w = gin_do(r, "GET", "/ascii", nil, nil)
	fwOK("AsciiJSON ct", gin_ct(w))
	fwOK("AsciiJSON escaped body", w.Body.String())

	w = gin_do(r, "GET", "/jsonp?callback=cb", nil, nil)
	fwOK("JSONP ct", gin_ct(w))
	fwOK("JSONP body", w.Body.String())
	w = gin_do(r, "GET", "/jsonp", nil, nil)
	fwOK("JSONP no-callback body", w.Body.String())
}

// ---------------------------------------------------------------------------
// Rendering - XML / YAML / String / Data / DataFromReader / HTML
// ---------------------------------------------------------------------------

type gin_xmlMsg struct {
	XMLName struct{} `xml:"msg"`
	Text    string   `xml:",chardata"`
}

func renderXMLYAMLStringDataHTML() {
	r := gin.New()
	r.GET("/xml", func(c *gin.Context) { c.XML(200, gin_xmlMsg{Text: "hi"}) })
	r.GET("/yaml", func(c *gin.Context) { c.YAML(200, gin.H{"a": 1}) })
	r.GET("/strf", func(c *gin.Context) { c.String(200, "hello %s", "world") })
	r.GET("/strraw", func(c *gin.Context) { c.String(201, "raw") })
	r.GET("/data", func(c *gin.Context) { c.Data(200, "application/octet-stream", []byte{1, 2, 3}) })
	r.GET("/reader", func(c *gin.Context) {
		c.DataFromReader(200, 5, "text/plain", strings.NewReader("hello"), nil)
	})

	w := gin_do(r, "GET", "/xml", nil, nil)
	fwOK("XML code", w.Code)
	fwOK("XML ct", gin_ct(w))
	fwOK("XML body", w.Body.String())

	w = gin_do(r, "GET", "/yaml", nil, nil)
	fwOK("YAML code", w.Code)
	fwOK("YAML ct", gin_ct(w))
	fwOK("YAML body", strings.TrimRight(w.Body.String(), "\n"))

	w = gin_do(r, "GET", "/strf", nil, nil)
	fwOK("String fmt ct", gin_ct(w))
	fwOK("String fmt body", w.Body.String())
	w = gin_do(r, "GET", "/strraw", nil, nil)
	fwOK("String raw code", w.Code)
	fwOK("String raw body", w.Body.String())

	w = gin_do(r, "GET", "/data", nil, nil)
	fwOK("Data code", w.Code)
	fwOK("Data ct", gin_ct(w))
	fwOK("Data bytes", fmt.Sprintf("%v", w.Body.Bytes()))

	w = gin_do(r, "GET", "/reader", nil, nil)
	fwOK("DataFromReader code", w.Code)
	fwOK("DataFromReader Content-Length", w.Header().Get("Content-Length"))
	fwOK("DataFromReader ct", gin_ct(w))
	fwOK("DataFromReader body", w.Body.String())

	// HTML via in-memory template (no files).
	rh := gin.New()
	tmpl := gin_htmlTemplate()
	rh.SetHTMLTemplate(tmpl)
	rh.GET("/h", func(c *gin.Context) { c.HTML(200, "t", gin.H{"name": "X"}) })
	w = gin_do(rh, "GET", "/h", nil, nil)
	fwOK("HTML code", w.Code)
	fwOK("HTML ct", gin_ct(w))
	fwOK("HTML body", w.Body.String())
}

// ---------------------------------------------------------------------------
// Rendering - TOML / BSON / ProtoBuf / SSE
// ---------------------------------------------------------------------------

func renderTOMLBSONProtoBufSSE() {
	r := gin.New()
	r.GET("/toml", func(c *gin.Context) { c.TOML(200, gin.H{"a": 1}) })
	r.GET("/bson", func(c *gin.Context) { c.BSON(200, gin.H{"a": int32(1)}) })
	r.GET("/proto", func(c *gin.Context) { c.ProtoBuf(200, wrapperspb.String("hi")) })
	r.GET("/sse", func(c *gin.Context) { c.SSEvent("message", "hello") })

	w := gin_do(r, "GET", "/toml", nil, nil)
	fwOK("TOML code", w.Code)
	fwOK("TOML ct", gin_ct(w))
	fwOK("TOML body", strings.TrimRight(w.Body.String(), "\n"))

	w = gin_do(r, "GET", "/bson", nil, nil)
	fwOK("BSON code", w.Code)
	fwOK("BSON ct", gin_ct(w))
	fwOK("BSON body-nonempty", w.Body.Len() > 0)

	w = gin_do(r, "GET", "/proto", nil, nil)
	fwOK("ProtoBuf code", w.Code)
	fwOK("ProtoBuf ct", gin_ct(w))
	// proto wire bytes are deterministic for a fixed message.
	fwOK("ProtoBuf wire bytes", fmt.Sprintf("%v", w.Body.Bytes()))

	w = gin_do(r, "GET", "/sse", nil, nil)
	fwOK("SSEvent code", w.Code)
	fwOK("SSEvent ct", gin_ct(w))
	fwOK("SSEvent body", strings.ReplaceAll(w.Body.String(), "\n", "\\n"))
}

// ---------------------------------------------------------------------------
// Rendering - File / FileAttachment / FileFromFS / Static*
// ---------------------------------------------------------------------------

func renderFileAndStatic() {
	// Create a deterministic temp file on disk for c.File / FileAttachment.
	dir, err := os.MkdirTemp("", "fwgin-static")
	if err != nil {
		fmt.Println("SKIP file/static: MkdirTemp:", err)
		return
	}
	defer os.RemoveAll(dir)
	fpath := filepath.Join(dir, "hello.txt")
	if err := os.WriteFile(fpath, []byte("file-body"), 0o644); err != nil {
		fmt.Println("SKIP file/static: WriteFile:", err)
		return
	}

	// c.File
	r := gin.New()
	r.GET("/file", func(c *gin.Context) { c.File(fpath) })
	w := gin_do(r, "GET", "/file", nil, nil)
	fwOK("File code", w.Code)
	fwOK("File body", w.Body.String())

	// c.FileAttachment -> Content-Disposition attachment.
	r.GET("/attach", func(c *gin.Context) { c.FileAttachment(fpath, "download.txt") })
	w = gin_do(r, "GET", "/attach", nil, nil)
	fwOK("FileAttachment code", w.Code)
	fwOK("FileAttachment Content-Disposition", w.Header().Get("Content-Disposition"))
	fwOK("FileAttachment body", w.Body.String())

	// c.FileFromFS with an in-memory fs.
	mfs := fstest.MapFS{"a/b.txt": &fstest.MapFile{Data: []byte("fromfs")}}
	r.GET("/fromfs", func(c *gin.Context) { c.FileFromFS("a/b.txt", http.FS(mfs)) })
	w = gin_do(r, "GET", "/fromfs", nil, nil)
	fwOK("FileFromFS code", w.Code)
	fwOK("FileFromFS body", w.Body.String())

	// Static directory serving.
	rs := gin.New()
	rs.Static("/assets", dir)
	w = gin_do(rs, "GET", "/assets/hello.txt", nil, nil)
	fwOK("Static dir code", w.Code)
	fwOK("Static dir body", w.Body.String())

	// StaticFile: single file at a fixed route.
	rsf := gin.New()
	rsf.StaticFile("/single", fpath)
	w = gin_do(rsf, "GET", "/single", nil, nil)
	fwOK("StaticFile code", w.Code)
	fwOK("StaticFile body", w.Body.String())

	// StaticFS: serve from http.FileSystem.
	rfs := gin.New()
	rfs.StaticFS("/fs", http.FS(fstest.MapFS{"x.txt": &fstest.MapFile{Data: []byte("staticfs")}}))
	w = gin_do(rfs, "GET", "/fs/x.txt", nil, nil)
	fwOK("StaticFS code", w.Code)
	fwOK("StaticFS body", w.Body.String())

	// StaticFileFS: single file from a FileSystem.
	rffs := gin.New()
	rffs.StaticFileFS("/one", "y.txt", http.FS(fstest.MapFS{"y.txt": &fstest.MapFile{Data: []byte("onefs")}}))
	w = gin_do(rffs, "GET", "/one", nil, nil)
	fwOK("StaticFileFS code", w.Code)
	fwOK("StaticFileFS body", w.Body.String())
}

// ---------------------------------------------------------------------------
// Rendering - Negotiate / NegotiateFormat / SetAccepted
// ---------------------------------------------------------------------------

func renderNegotiate() {
	r := gin.New()
	r.GET("/neg", func(c *gin.Context) {
		c.Negotiate(200, gin.Negotiate{
			Offered:  []string{gin.MIMEJSON, gin.MIMEXML},
			JSONData: gin.H{"a": 1},
			XMLData:  gin_xmlMsg{Text: "x"},
		})
	})
	// Accept JSON.
	w := gin_do(r, "GET", "/neg", nil, map[string]string{"Accept": "application/json"})
	fwOK("Negotiate JSON code", w.Code)
	fwOK("Negotiate JSON ct", gin_ct(w))
	fwOK("Negotiate JSON body", w.Body.String())
	// Accept XML.
	w = gin_do(r, "GET", "/neg", nil, map[string]string{"Accept": "application/xml"})
	fwOK("Negotiate XML ct", gin_ct(w))
	fwOK("Negotiate XML body", w.Body.String())

	// NegotiateFormat: choose best offered for an Accept header.
	rf := gin.New()
	rf.GET("/nf", func(c *gin.Context) {
		c.String(200, c.NegotiateFormat(gin.MIMEJSON, gin.MIMEXML))
	})
	w = gin_do(rf, "GET", "/nf", nil, map[string]string{"Accept": "application/xml"})
	fwOK("NegotiateFormat picks xml", w.Body.String())
	w = gin_do(rf, "GET", "/nf", nil, map[string]string{"Accept": "application/json"})
	fwOK("NegotiateFormat picks json", w.Body.String())

	// SetAccepted overrides the request's accepted formats.
	rsa := gin.New()
	rsa.GET("/sa", func(c *gin.Context) {
		c.SetAccepted(gin.MIMEXML)
		c.String(200, c.NegotiateFormat(gin.MIMEJSON, gin.MIMEXML))
	})
	w = gin_do(rsa, "GET", "/sa", nil, map[string]string{"Accept": "application/json"})
	fwOK("SetAccepted forces xml", w.Body.String())
}

// ---------------------------------------------------------------------------
// Rendering - Redirect / Status / Header / status codes
// ---------------------------------------------------------------------------

func renderRedirectStatusHeader() {
	r := gin.New()
	r.GET("/redir", func(c *gin.Context) { c.Redirect(http.StatusMovedPermanently, "/new") })
	r.GET("/status", func(c *gin.Context) { c.Status(204) })
	r.GET("/hdr", func(c *gin.Context) { c.Header("X-Custom", "v"); c.Status(200) })
	r.GET("/hdrdel", func(c *gin.Context) { c.Header("X-Del", "v"); c.Header("X-Del", ""); c.Status(200) })

	w := gin_do(r, "GET", "/redir", nil, nil)
	fwOK("Redirect code", w.Code)
	fwOK("Redirect Location", w.Header().Get("Location"))

	w = gin_do(r, "GET", "/status", nil, nil)
	fwOK("Status code", w.Code)
	fwOK("Status empty body", w.Body.Len() == 0)

	w = gin_do(r, "GET", "/hdr", nil, nil)
	fwOK("Header set value", w.Header().Get("X-Custom"))
	w = gin_do(r, "GET", "/hdrdel", nil, nil)
	fwOK("Header delete (empty)", "["+w.Header().Get("X-Del")+"]")

	// net/http status constants on a render.
	rc := gin.New()
	rc.GET("/created", func(c *gin.Context) { c.JSON(http.StatusCreated, gin.H{"a": 1}) })
	rc.GET("/bad", func(c *gin.Context) { c.JSON(http.StatusBadRequest, gin.H{"a": 1}) })
	rc.GET("/ise", func(c *gin.Context) { c.JSON(http.StatusInternalServerError, gin.H{"a": 1}) })
	fwOK("Status 201", gin_do(rc, "GET", "/created", nil, nil).Code)
	fwOK("Status 400", gin_do(rc, "GET", "/bad", nil, nil).Code)
	fwOK("Status 500", gin_do(rc, "GET", "/ise", nil, nil).Code)

	// Redirect out-of-range code panics.
	rpanic := gin.New()
	rpanic.GET("/badredir", func(c *gin.Context) {
		defer func() {
			if rec := recover(); rec != nil {
				c.String(200, "panicked")
			}
		}()
		c.Redirect(200, "/x")
	})
	w = gin_do(rpanic, "GET", "/badredir", nil, nil)
	fwOK("Redirect invalid-code panics", w.Body.String())
}

// ---------------------------------------------------------------------------
// Context helpers - query
// ---------------------------------------------------------------------------

func contextQuery() {
	r := gin.New()
	r.GET("/q", func(c *gin.Context) {
		v, ok1 := c.GetQuery("a")
		_, ok2 := c.GetQuery("missing")
		c.JSON(200, gin.H{
			"query":     c.Query("q"),
			"querymiss": c.Query("nope") == "",
			"default":   c.DefaultQuery("a", "z"),
			"defmiss":   c.DefaultQuery("b", "z"),
			"getq":      v,
			"getqok":    ok1,
			"missok":    ok2,
		})
	})
	w := gin_do(r, "GET", "/q?q=hello&a=1", nil, nil)
	fwOK("Query family body", gin_sortedJSON(w.Body.Bytes()))

	// DefaultQuery: present-but-empty (?a=) returns "" not default.
	r.GET("/q2", func(c *gin.Context) { c.String(200, "["+c.DefaultQuery("a", "z")+"]") })
	w = gin_do(r, "GET", "/q2?a=", nil, nil)
	fwOK("DefaultQuery empty-present", w.Body.String())

	// QueryArray / GetQueryArray.
	r.GET("/qa", func(c *gin.Context) {
		arr := c.QueryArray("ids")
		_, ok := c.GetQueryArray("missing")
		c.JSON(200, gin.H{"arr": arr, "missok": ok})
	})
	w = gin_do(r, "GET", "/qa?ids=1&ids=2", nil, nil)
	fwOK("QueryArray body", gin_sortedJSON(w.Body.Bytes()))

	// QueryMap / GetQueryMap.
	r.GET("/qm", func(c *gin.Context) {
		c.JSON(200, c.QueryMap("m"))
	})
	w = gin_do(r, "GET", "/qm?m[a]=1&m[b]=2", nil, nil)
	fwOK("QueryMap body", gin_sortedJSON(w.Body.Bytes()))
}

// ---------------------------------------------------------------------------
// Context helpers - form/post & params
// ---------------------------------------------------------------------------

func contextFormPostParams() {
	r := gin.New()
	r.POST("/f", func(c *gin.Context) {
		v, ok := c.GetPostForm("name")
		c.JSON(200, gin.H{
			"postform": c.PostForm("name"),
			"miss":     c.PostForm("nope") == "",
			"default":  c.DefaultPostForm("name", "z"),
			"defmiss":  c.DefaultPostForm("nope", "z"),
			"getpf":    v,
			"getpfok":  ok,
		})
	})
	w := gin_do(r, "POST", "/f", strings.NewReader("name=alice"),
		map[string]string{"Content-Type": "application/x-www-form-urlencoded"})
	fwOK("PostForm family body", gin_sortedJSON(w.Body.Bytes()))

	// PostFormArray / GetPostFormArray / PostFormMap / GetPostFormMap.
	r.POST("/fa", func(c *gin.Context) {
		gv, gok := c.GetPostFormArray("ids")
		mv, mok := c.GetPostFormMap("m")
		c.JSON(200, gin.H{
			"arr":    c.PostFormArray("ids"),
			"map":    c.PostFormMap("m"),
			"garr":   gv,
			"garrok": gok,
			"gmap":   mv,
			"gmapok": mok,
		})
	})
	w = gin_do(r, "POST", "/fa", strings.NewReader("ids=1&ids=2&m[a]=x"),
		map[string]string{"Content-Type": "application/x-www-form-urlencoded"})
	fwOK("PostForm array/map body", gin_sortedJSON(w.Body.Bytes()))

	// Param (defined + undefined).
	rp := gin.New()
	rp.GET("/u/:id", func(c *gin.Context) { c.String(200, c.Param("id")+"|"+c.Param("nope")) })
	w = gin_do(rp, "GET", "/u/55", nil, nil)
	fwOK("Param defined|undefined", w.Body.String())

	// FormFile + MultipartForm via in-memory multipart writer.
	body := &bytes.Buffer{}
	mw := multipart.NewWriter(body)
	fw, _ := mw.CreateFormFile("file", "upload.txt")
	fw.Write([]byte("multipart-content"))
	mw.WriteField("field", "fv")
	mw.Close()
	rf := gin.New()
	rf.POST("/upload", func(c *gin.Context) {
		fh, err := c.FormFile("file")
		if err != nil {
			c.String(500, "err")
			return
		}
		form, _ := c.MultipartForm()
		c.JSON(200, gin.H{
			"filename": fh.Filename,
			"size":     fh.Size,
			"field":    form.Value["field"][0],
		})
	})
	w = gin_do(rf, "POST", "/upload", body, map[string]string{"Content-Type": mw.FormDataContentType()})
	fwOK("FormFile/MultipartForm body", gin_sortedJSON(w.Body.Bytes()))
}

// ---------------------------------------------------------------------------
// Context helpers - headers, cookies, Set/Get
// ---------------------------------------------------------------------------

func contextHeadersCookiesGetSet() {
	r := gin.New()
	r.GET("/hdr", func(c *gin.Context) {
		c.JSON(200, gin.H{
			"token": c.GetHeader("X-Token"),
			"miss":  c.GetHeader("X-None") == "",
			"ctype": c.ContentType(),
		})
	})
	w := gin_do(r, "GET", "/hdr", nil, map[string]string{
		"X-Token":      "abc",
		"Content-Type": "application/json; charset=utf-8",
	})
	fwOK("GetHeader/ContentType body", gin_sortedJSON(w.Body.Bytes()))

	// Cookie read (present + missing).
	rc := gin.New()
	rc.GET("/c", func(c *gin.Context) {
		v, err := c.Cookie("sid")
		_, errMiss := c.Cookie("none")
		c.JSON(200, gin.H{
			"sid":     v,
			"err":     err == nil,
			"missErr": errors.Is(errMiss, http.ErrNoCookie),
		})
	})
	wc := httptest.NewRecorder()
	rqc := httptest.NewRequest("GET", "/c", nil)
	rqc.AddCookie(&http.Cookie{Name: "sid", Value: "xyz"})
	rc.ServeHTTP(wc, rqc)
	fwOK("Cookie read body", gin_sortedJSON(wc.Body.Bytes()))

	// SetCookie -> Set-Cookie header parsed.
	rsc := gin.New()
	rsc.GET("/sc", func(c *gin.Context) {
		c.SetCookie("sid", "xyz", 3600, "/", "", false, true)
		c.Status(200)
	})
	w = gin_do(rsc, "GET", "/sc", nil, nil)
	cookies := w.Result().Cookies()
	if len(cookies) == 1 {
		ck := cookies[0]
		fwOK("SetCookie name", ck.Name)
		fwOK("SetCookie value", ck.Value)
		fwOK("SetCookie path", ck.Path)
		fwOK("SetCookie maxage", ck.MaxAge)
		fwOK("SetCookie httponly", ck.HttpOnly)
	} else {
		fwOK("SetCookie count unexpected", len(cookies))
	}

	// SetSameSite + SetCookie -> SameSite attribute.
	rss := gin.New()
	rss.GET("/ss", func(c *gin.Context) {
		c.SetSameSite(http.SameSiteStrictMode)
		c.SetCookie("s", "v", 60, "/", "", false, false)
		c.Status(200)
	})
	w = gin_do(rss, "GET", "/ss", nil, nil)
	fwOK("SetSameSite Strict", w.Result().Cookies()[0].SameSite == http.SameSiteStrictMode)

	// SetCookieData: full http.Cookie control.
	rcd := gin.New()
	rcd.GET("/cd", func(c *gin.Context) {
		c.SetCookieData(&http.Cookie{Name: "cd", Value: "dat", Path: "/p", MaxAge: 120, HttpOnly: true})
		c.Status(200)
	})
	w = gin_do(rcd, "GET", "/cd", nil, nil)
	cd := w.Result().Cookies()[0]
	fwOK("SetCookieData name/path", cd.Name+"|"+cd.Path)

	// Set / Get / MustGet (present, missing, panic-recovered).
	w2 := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w2)
	c.Set("k", "v")
	val, exists := c.Get("k")
	_, exists2 := c.Get("missing")
	fwOK("Get present val", val)
	fwOK("Get present ok", exists)
	fwOK("Get missing ok", exists2)
	c.Set("n", 1)
	fwOK("MustGet present", c.MustGet("n"))
	func() {
		defer func() {
			if rec := recover(); rec != nil {
				fwOK("MustGet missing panics", true)
			}
		}()
		_ = c.MustGet("absent")
		fwOK("MustGet missing panics", false)
	}()
}

// ---------------------------------------------------------------------------
// Typed getters (full enumerated set)
// ---------------------------------------------------------------------------

func typedGetters() {
	w := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w)
	fixedTime := time.Unix(1700000000, 0).UTC()
	c.Set("s", "hello")
	c.Set("i", int(5))
	c.Set("i8", int8(8))
	c.Set("i16", int16(16))
	c.Set("i32", int32(32))
	c.Set("i64", int64(64))
	c.Set("u", uint(5))
	c.Set("u8", uint8(8))
	c.Set("u16", uint16(16))
	c.Set("u32", uint32(32))
	c.Set("u64", uint64(64))
	c.Set("f32", float32(1.5))
	c.Set("f64", float64(2.5))
	c.Set("b", true)
	c.Set("t", fixedTime)
	c.Set("d", 3*time.Second)
	c.Set("sms", map[string]string{"a": "b"})
	c.Set("sm", map[string]any{"a": 1})
	c.Set("smss", map[string][]string{"a": {"x", "y"}})
	c.Set("ss", []string{"x", "y"})
	c.Set("is", []int{1, 2})
	c.Set("i8s", []int8{1, 2})
	c.Set("i16s", []int16{1, 2})
	c.Set("i32s", []int32{1, 2})
	c.Set("i64s", []int64{1, 2})
	c.Set("us", []uint{1, 2})
	c.Set("u8s", []uint8{1, 2})
	c.Set("u16s", []uint16{1, 2})
	c.Set("u32s", []uint32{1, 2})
	c.Set("u64s", []uint64{1, 2})
	c.Set("f32s", []float32{1.5, 2.5})
	c.Set("f64s", []float64{1.5, 2.5})

	fwOK("GetString", c.GetString("s"))
	fwOK("GetInt", c.GetInt("i"))
	fwOK("GetInt8", c.GetInt8("i8"))
	fwOK("GetInt16", c.GetInt16("i16"))
	fwOK("GetInt32", c.GetInt32("i32"))
	fwOK("GetInt64", c.GetInt64("i64"))
	fwOK("GetUint", c.GetUint("u"))
	fwOK("GetUint8", c.GetUint8("u8"))
	fwOK("GetUint16", c.GetUint16("u16"))
	fwOK("GetUint32", c.GetUint32("u32"))
	fwOK("GetUint64", c.GetUint64("u64"))
	fwOK("GetFloat32", c.GetFloat32("f32"))
	fwOK("GetFloat64", c.GetFloat64("f64"))
	fwOK("GetBool", c.GetBool("b"))
	fwOK("GetTime", c.GetTime("t").Format(time.RFC3339))
	fwOK("GetDuration", c.GetDuration("d"))
	fwOK("GetStringMapString", fmt.Sprintf("%v", c.GetStringMapString("sms")))
	fwOK("GetStringMap", fmt.Sprintf("%v", c.GetStringMap("sm")))
	fwOK("GetStringMapStringSlice", fmt.Sprintf("%v", c.GetStringMapStringSlice("smss")))
	fwOK("GetStringSlice", fmt.Sprintf("%v", c.GetStringSlice("ss")))
	fwOK("GetIntSlice", fmt.Sprintf("%v", c.GetIntSlice("is")))
	fwOK("GetInt8Slice", fmt.Sprintf("%v", c.GetInt8Slice("i8s")))
	fwOK("GetInt16Slice", fmt.Sprintf("%v", c.GetInt16Slice("i16s")))
	fwOK("GetInt32Slice", fmt.Sprintf("%v", c.GetInt32Slice("i32s")))
	fwOK("GetInt64Slice", fmt.Sprintf("%v", c.GetInt64Slice("i64s")))
	fwOK("GetUintSlice", fmt.Sprintf("%v", c.GetUintSlice("us")))
	fwOK("GetUint8Slice", fmt.Sprintf("%v", c.GetUint8Slice("u8s")))
	fwOK("GetUint16Slice", fmt.Sprintf("%v", c.GetUint16Slice("u16s")))
	fwOK("GetUint32Slice", fmt.Sprintf("%v", c.GetUint32Slice("u32s")))
	fwOK("GetUint64Slice", fmt.Sprintf("%v", c.GetUint64Slice("u64s")))
	fwOK("GetFloat32Slice", fmt.Sprintf("%v", c.GetFloat32Slice("f32s")))
	fwOK("GetFloat64Slice", fmt.Sprintf("%v", c.GetFloat64Slice("f64s")))

	// Missing key / type mismatch -> zero value, no panic.
	fwOK("GetString missing -> zero", "["+c.GetString("none")+"]")
	fwOK("GetInt missing -> zero", c.GetInt("none"))
	fwOK("GetBool missing -> zero", c.GetBool("none"))
	fwOK("GetString type-mismatch -> zero", "["+c.GetString("i")+"]")
}

// ---------------------------------------------------------------------------
// Error surface
// ---------------------------------------------------------------------------

func errorSurface() {
	// c.Error pushes without aborting; downstream still runs.
	reached := false
	r := gin.New()
	r.GET("/e", func(c *gin.Context) {
		e := c.Error(errors.New("first"))
		e.SetType(gin.ErrorTypePublic)
		e.SetMeta(gin.H{"k": "v"})
		c.Next()
	}, func(c *gin.Context) {
		reached = true
		c.JSON(200, gin.H{"errs": len(c.Errors)})
	})
	w := gin_do(r, "GET", "/e", nil, nil)
	fwOK("c.Error does not abort (downstream ran)", reached)
	fwOK("c.Error pushed count", w.Body.String())

	// Error type filters + JSON() + ByType.
	w2 := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w2)
	c.Request = httptest.NewRequest("GET", "/x", nil)
	c.Error(errors.New("pub")).SetType(gin.ErrorTypePublic)
	c.Error(errors.New("priv")).SetType(gin.ErrorTypePrivate)
	c.Error(&gin.Error{Err: errors.New("bind"), Type: gin.ErrorTypeBind})
	fwOK("Errors total", len(c.Errors))
	fwOK("Errors ByType public count", len(c.Errors.ByType(gin.ErrorTypePublic)))
	fwOK("Errors ByType private count", len(c.Errors.ByType(gin.ErrorTypePrivate)))
	fwOK("Errors Last msg", c.Errors.Last().Error())

	// Error.JSON of a typed error with meta.
	je := &gin.Error{Err: errors.New("boom"), Type: gin.ErrorTypePublic}
	je.SetMeta(gin.H{"k": "v"})
	jb, _ := json.Marshal(je.JSON())
	fwOK("Error.JSON", gin_sortedJSON(jb))

	// ErrorLogger / ErrorLoggerT middleware: writes accumulated errors as JSON.
	// gin.ErrorLogger() captures DefaultErrorWriter at construction, so set the
	// observation buffer first, then restore the global io.Discard.
	buf := &bytes.Buffer{}
	gin.DefaultErrorWriter = buf
	rel := gin.New()
	rel.Use(gin.ErrorLogger())
	gin.DefaultErrorWriter = io.Discard
	rel.GET("/el", func(c *gin.Context) {
		c.Error(errors.New("logged-error"))
		c.Status(200)
	})
	w = gin_do(rel, "GET", "/el", nil, nil)
	fwOK("ErrorLogger response code", w.Code)
	fwOK("ErrorLogger wrote error JSON", strings.Contains(w.Body.String(), "logged-error") || strings.Contains(buf.String(), "logged-error"))

	// ErrorLoggerT for a specific type.
	relt := gin.New()
	relt.Use(gin.ErrorLoggerT(gin.ErrorTypePublic))
	relt.GET("/elt", func(c *gin.Context) {
		c.Error(errors.New("public-err")).SetType(gin.ErrorTypePublic)
		c.Status(200)
	})
	w = gin_do(relt, "GET", "/elt", nil, nil)
	fwOK("ErrorLoggerT wrote typed error", strings.Contains(w.Body.String(), "public-err"))
}

// ---------------------------------------------------------------------------
// Client IP / proxy
// ---------------------------------------------------------------------------

func clientIPProxy() {
	// Trusted proxy + X-Forwarded-For: ClientIP returns the original client.
	r := gin.New()
	r.ForwardedByClientIP = true
	if err := r.SetTrustedProxies([]string{"192.0.2.1"}); err != nil {
		fwOK("SetTrustedProxies err", err.Error())
	} else {
		fwOK("SetTrustedProxies ok", true)
	}
	r.GET("/ip", func(c *gin.Context) {
		c.JSON(200, gin.H{"client": c.ClientIP(), "remote": c.RemoteIP()})
	})
	w := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/ip", nil)
	req.RemoteAddr = "192.0.2.1:1234" // trusted proxy
	req.Header.Set("X-Forwarded-For", "203.0.113.9")
	r.ServeHTTP(w, req)
	fwOK("ClientIP via XFF body", gin_sortedJSON(w.Body.Bytes()))

	// Untrusted proxy: ClientIP falls back to RemoteAddr.
	w2 := httptest.NewRecorder()
	req2 := httptest.NewRequest("GET", "/ip", nil)
	req2.RemoteAddr = "198.51.100.5:9999" // not trusted
	req2.Header.Set("X-Forwarded-For", "203.0.113.9")
	r.ServeHTTP(w2, req2)
	fwOK("ClientIP untrusted falls back", gin_sortedJSON(w2.Body.Bytes()))

	// TrustedPlatform: trust a platform header (e.g. CF-Connecting-IP).
	rp := gin.New()
	rp.TrustedPlatform = "CF-Connecting-IP"
	rp.GET("/ip", func(c *gin.Context) { c.String(200, c.ClientIP()) })
	w3 := httptest.NewRecorder()
	req3 := httptest.NewRequest("GET", "/ip", nil)
	req3.RemoteAddr = "10.0.0.1:1"
	req3.Header.Set("CF-Connecting-IP", "203.0.113.50")
	rp.ServeHTTP(w3, req3)
	fwOK("TrustedPlatform ClientIP", w3.Body.String())

	// RemoteIPHeaders custom + ForwardedByClientIP.
	rh := gin.New()
	rh.ForwardedByClientIP = true
	rh.RemoteIPHeaders = []string{"X-Real-IP"}
	_ = rh.SetTrustedProxies([]string{"192.0.2.1"})
	rh.GET("/ip", func(c *gin.Context) { c.String(200, c.ClientIP()) })
	w4 := httptest.NewRecorder()
	req4 := httptest.NewRequest("GET", "/ip", nil)
	req4.RemoteAddr = "192.0.2.1:1"
	req4.Header.Set("X-Real-IP", "203.0.113.77")
	rh.ServeHTTP(w4, req4)
	fwOK("RemoteIPHeaders X-Real-IP ClientIP", w4.Body.String())
}

// ---------------------------------------------------------------------------
// Body / raw / SaveUploadedFile
// ---------------------------------------------------------------------------

func rawAndMultipart() {
	// GetRawData reads the full request body.
	r := gin.New()
	r.POST("/raw", func(c *gin.Context) {
		b, err := c.GetRawData()
		c.JSON(200, gin.H{"data": string(b), "err": err == nil})
	})
	w := gin_do(r, "POST", "/raw", strings.NewReader("raw-bytes"), nil)
	fwOK("GetRawData body", gin_sortedJSON(w.Body.Bytes()))

	// SaveUploadedFile writes the uploaded part's content to disk.
	dir, err := os.MkdirTemp("", "fwgin-upload")
	if err != nil {
		fmt.Println("SKIP SaveUploadedFile: MkdirTemp:", err)
		return
	}
	defer os.RemoveAll(dir)
	body := &bytes.Buffer{}
	mw := multipart.NewWriter(body)
	fw, _ := mw.CreateFormFile("file", "u.txt")
	fw.Write([]byte("saved-content"))
	mw.Close()

	dst := filepath.Join(dir, "out.txt")
	rs := gin.New()
	rs.POST("/save", func(c *gin.Context) {
		fh, e := c.FormFile("file")
		if e != nil {
			c.String(500, "nofile")
			return
		}
		if e := c.SaveUploadedFile(fh, dst); e != nil {
			c.String(500, "saveerr")
			return
		}
		c.String(200, "saved")
	})
	w = gin_do(rs, "POST", "/save", body, map[string]string{"Content-Type": mw.FormDataContentType()})
	fwOK("SaveUploadedFile code", w.Code)
	saved, _ := os.ReadFile(dst)
	fwOK("SaveUploadedFile content", string(saved))
}

// ---------------------------------------------------------------------------
// Handler introspection
// ---------------------------------------------------------------------------

func gin_named(c *gin.Context) {
	c.String(200, c.HandlerName()+"|"+fmt.Sprintf("%d", len(c.HandlerNames())))
}

func gin_wrapMW(c *gin.Context) { c.Next() }

func handlerIntrospection() {
	r := gin.New()
	r.Use(gin_wrapMW)
	r.GET("/n", gin_named)
	w := gin_do(r, "GET", "/n", nil, nil)
	// HandlerName ends with the function name; assert suffix only.
	body := w.Body.String()
	fwOK("HandlerName ends with .named", strings.HasSuffix(strings.Split(body, "|")[0], ".gin_named"))
	fwOK("HandlerNames count >=2", strings.Split(body, "|")[1])

	// c.Handler returns the final handler func (non-nil).
	rh := gin.New()
	var handlerNonNil bool
	rh.GET("/h", func(c *gin.Context) {
		handlerNonNil = c.Handler() != nil
		c.Status(200)
	})
	gin_do(rh, "GET", "/h", nil, nil)
	fwOK("c.Handler non-nil", handlerNonNil)

	// Routes() Handler / HandlerFunc fields.
	ri := r.Routes()
	fwOK("Routes Handler ends with .named", strings.HasSuffix(ri[0].Handler, ".gin_named"))
	fwOK("Routes HandlerFunc non-nil", ri[0].HandlerFunc != nil)
}

// ---------------------------------------------------------------------------
// WrapF / WrapH adapters + mode helpers
// ---------------------------------------------------------------------------

func wrappersAndModeHelpers() {
	// WrapF: adapt an http.HandlerFunc.
	r := gin.New()
	r.GET("/wf", gin.WrapF(func(w http.ResponseWriter, req *http.Request) {
		w.WriteHeader(200)
		w.Write([]byte("wrapf"))
	}))
	w := gin_do(r, "GET", "/wf", nil, nil)
	fwOK("WrapF code", w.Code)
	fwOK("WrapF body", w.Body.String())

	// WrapH: adapt an http.Handler.
	r.GET("/wh", gin.WrapH(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		w.WriteHeader(202)
		w.Write([]byte("wraph"))
	})))
	w = gin_do(r, "GET", "/wh", nil, nil)
	fwOK("WrapH code", w.Code)
	fwOK("WrapH body", w.Body.String())

	// Mode helpers (no output side-effects that pollute determinism).
	gin.DisableConsoleColor()
	fwOK("IsDebugging in TestMode", gin.IsDebugging())
	gin.ForceConsoleColor()
	gin.DisableConsoleColor() // restore quiet
	fwOK("ForceConsoleColor toggled ok", true)

	// EnableJsonDecoderUseNumber: numbers decode as json.Number.
	gin.EnableJsonDecoderUseNumber()
	rn := gin.New()
	rn.POST("/num", func(c *gin.Context) {
		var v map[string]any
		_ = c.ShouldBindJSON(&v)
		_, isNum := v["n"].(json.Number)
		c.JSON(200, gin.H{"isNumber": isNum})
	})
	w = gin_do(rn, "POST", "/num", strings.NewReader(`{"n":12345678901234}`),
		map[string]string{"Content-Type": "application/json"})
	fwOK("EnableJsonDecoderUseNumber body", w.Body.String())
	binding.EnableDecoderUseNumber = false // restore global for determinism across runs
}

// ---------------------------------------------------------------------------
// Logger variants: LoggerWithConfig / LoggerWithFormatter
// ---------------------------------------------------------------------------

func loggerVariants() {
	// LoggerWithConfig: custom output, SkipPaths.
	buf := &bytes.Buffer{}
	r := gin.New()
	r.Use(gin.LoggerWithConfig(gin.LoggerConfig{
		Output:    buf,
		SkipPaths: []string{"/skip"},
	}))
	r.GET("/keep", func(c *gin.Context) { c.Status(200) })
	r.GET("/skip", func(c *gin.Context) { c.Status(200) })
	gin_do(r, "GET", "/keep", nil, nil)
	gin_do(r, "GET", "/skip", nil, nil)
	logged := buf.String()
	fwOK("LoggerWithConfig logs /keep", strings.Contains(logged, "/keep"))
	fwOK("LoggerWithConfig skips /skip", !strings.Contains(logged, "/skip"))

	// LoggerWithFormatter: deterministic custom format (no timestamps).
	// gin.LoggerWithFormatter captures DefaultWriter at construction, so set
	// the observation buffer first, then restore the global io.Discard.
	buf2 := &bytes.Buffer{}
	gin.DefaultWriter = buf2
	r2 := gin.New()
	r2.Use(gin.LoggerWithFormatter(func(p gin.LogFormatterParams) string {
		return fmt.Sprintf("FMT %s %s %d\n", p.Method, p.Path, p.StatusCode)
	}))
	gin.DefaultWriter = io.Discard
	r2.GET("/fmt", func(c *gin.Context) { c.Status(200) })
	gin_do(r2, "GET", "/fmt", nil, nil)
	fwOK("LoggerWithFormatter custom line", strings.TrimRight(buf2.String(), "\n"))

	// LoggerConfig with explicit Formatter into a buffer.
	buf2b := &bytes.Buffer{}
	r2b := gin.New()
	r2b.Use(gin.LoggerWithConfig(gin.LoggerConfig{
		Output: buf2b,
		Formatter: func(p gin.LogFormatterParams) string {
			return fmt.Sprintf("CFG %s %s %d\n", p.Method, p.Path, p.StatusCode)
		},
	}))
	r2b.GET("/cfgfmt", func(c *gin.Context) { c.Status(200) })
	gin_do(r2b, "GET", "/cfgfmt", nil, nil)
	fwOK("LoggerConfig Formatter line", strings.TrimRight(buf2b.String(), "\n"))

	// LoggerConfig Skip func.
	buf3 := &bytes.Buffer{}
	r3 := gin.New()
	r3.Use(gin.LoggerWithConfig(gin.LoggerConfig{
		Output: buf3,
		Skip:   func(c *gin.Context) bool { return c.Request.URL.Path == "/no" },
	}))
	r3.GET("/yes", func(c *gin.Context) { c.Status(200) })
	r3.GET("/no", func(c *gin.Context) { c.Status(200) })
	gin_do(r3, "GET", "/yes", nil, nil)
	gin_do(r3, "GET", "/no", nil, nil)
	l3 := buf3.String()
	fwOK("LoggerConfig Skip skips /no", strings.Contains(l3, "/yes") && !strings.Contains(l3, "/no"))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

// gin_sortedJSON re-marshals a JSON object with sorted keys for stable output.
// encoding/json sorts map keys, so unmarshal->marshal yields a deterministic
// ordering independent of gin's renderer (goccy/go-json).
func gin_sortedJSON(b []byte) string {
	var v any
	if err := json.Unmarshal(b, &v); err != nil {
		return "PARSE_ERR:" + string(b)
	}
	out, _ := json.Marshal(v)
	return string(out)
}

func gin_errString(e error) string {
	if e == nil {
		return ""
	}
	return e.Error()
}

// gin_bindJSONErr binds a JSON body into obj via a fresh engine and returns the
// validation/decode error (no response written: ShouldBindJSON).
func gin_bindJSONErr(obj any, body string) error {
	w := httptest.NewRecorder()
	c, _ := gin.CreateTestContext(w)
	c.Request = httptest.NewRequest("POST", "/", strings.NewReader(body))
	c.Request.Header.Set("Content-Type", "application/json")
	return c.ShouldBindJSON(obj)
}

// gin_htmlTemplate builds an in-memory html/template named "t" (no files on disk).
func gin_htmlTemplate() *template.Template {
	return template.Must(template.New("t").Parse("Hello {{.name}}"))
}
