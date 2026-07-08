// Package main is a deterministic, doc-grounded carpet for github.com/zeromicro/go-zero v1.10.2.
//
// Every assertion prints `ok: <label> = <value>` and increments a counter; the
// program prints GOZERO_COUNT=<n> at the end. All inputs are fixed; map keys are
// sorted; no timestamps/addresses/random values leak into output; rest is exercised
// via httptest recorders + in-process routers; zrpc via google.golang.org/grpc/test/bufconn
// with the real grpc health stub. Run twice -> byte-identical.
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zeromicro/go-zero/core/collection"
	"github.com/zeromicro/go-zero/core/fx"
	"github.com/zeromicro/go-zero/core/jsonx"
	"github.com/zeromicro/go-zero/core/logx"
	"github.com/zeromicro/go-zero/core/mapping"
	"github.com/zeromicro/go-zero/core/mathx"
	"github.com/zeromicro/go-zero/core/mr"
	"github.com/zeromicro/go-zero/core/stringx"
	"github.com/zeromicro/go-zero/core/syncx"
	"github.com/zeromicro/go-zero/core/timex"
	"github.com/zeromicro/go-zero/rest"
	"github.com/zeromicro/go-zero/rest/httpx"
	"github.com/zeromicro/go-zero/rest/router"
	"github.com/zeromicro/go-zero/zrpc"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	healthpb "google.golang.org/grpc/health/grpc_health_v1"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"
)

// section prints a banner so the output is human-scannable & still deterministic.
func gz_section(name string) { fmt.Printf("# == %s ==\n", name) }

func runFrameworkGoZero() {
	// Silence go-zero's internal logx/logc writers: WriteJson/WriteJsonCtx and the
	// zrpc client interceptors log JSON lines (with timestamps, trace ids,
	// durations) to stdout/stderr on the error paths exercised below. Those are
	// framework logging noise, not assertions, and are inherently nondeterministic.
	// Disabling logx makes the carpet output byte-identical across runs.
	logx.Disable()

	restResponses()
	restRequests()
	restServerRouting()
	zrpcSection()
	stringxSection()
	mathxSection()
	collectionSection()
	syncxSection()
	fxSection()
	mrSection()
	jsonxSection()
	mappingSection()

}

// ---------------------------------------------------------------------------
// rest/httpx response writers
// ---------------------------------------------------------------------------

func restResponses() {
	gz_section("rest/httpx response writers")

	// OkJson: 200 + json body, application/json; charset=utf-8, no trailing newline.
	{
		rec := httptest.NewRecorder()
		httpx.OkJson(rec, struct {
			Name string `json:"name"`
		}{"a"})
		fwOK("httpx.OkJson code", rec.Code)
		fwOK("httpx.OkJson body", rec.Body.String())
		fwOK("httpx.OkJson content-type", rec.Header().Get("Content-Type"))
		fwOK("httpx.OkJson no-trailing-newline", !strings.HasSuffix(rec.Body.String(), "\n"))
	}

	// WriteJson with explicit code.
	{
		rec := httptest.NewRecorder()
		httpx.WriteJson(rec, 201, map[string]int{"n": 7})
		fwOK("httpx.WriteJson code", rec.Code)
		fwOK("httpx.WriteJson body", rec.Body.String())
		fwOK("httpx.WriteJson content-type", rec.Header().Get("Content-Type"))
	}

	// Ok: 200, empty body.
	{
		rec := httptest.NewRecorder()
		httpx.Ok(rec)
		fwOK("httpx.Ok code", rec.Code)
		fwOK("httpx.Ok empty-body", rec.Body.Len() == 0)
	}

	// OkJsonCtx / WriteJsonCtx: same JSON + status as non-ctx variants.
	{
		ctx := context.Background()
		rec := httptest.NewRecorder()
		httpx.OkJsonCtx(ctx, rec, map[string]int{"n": 7})
		fwOK("httpx.OkJsonCtx code", rec.Code)
		fwOK("httpx.OkJsonCtx body", rec.Body.String())

		rec2 := httptest.NewRecorder()
		httpx.WriteJsonCtx(ctx, rec2, 202, map[string]string{"k": "v"})
		fwOK("httpx.WriteJsonCtx code", rec2.Code)
		fwOK("httpx.WriteJsonCtx body", rec2.Body.String())
	}

	// Error default handler: no custom handler => http.Error(...) with
	// StatusBadRequest (400) and the err text + trailing newline (doc-grounded:
	// doHandleError falls back to http.Error w/ http.StatusBadRequest).
	{
		rec := httptest.NewRecorder()
		httpx.Error(rec, errors.New("boom"))
		fwOK("httpx.Error default code", rec.Code)
		fwOK("httpx.Error default body-contains-boom", strings.Contains(rec.Body.String(), "boom"))
	}

	// Error with per-call fns override.
	{
		rec := httptest.NewRecorder()
		httpx.Error(rec, errors.New("ignored"), func(w http.ResponseWriter, err error) {
			w.WriteHeader(418)
			_, _ = io.WriteString(w, "teapot")
		})
		fwOK("httpx.Error fns-override code", rec.Code)
		fwOK("httpx.Error fns-override body", rec.Body.String())
	}

	// SetErrorHandler: custom (code, body) tuple => WriteJson path.
	{
		httpx.SetErrorHandler(func(e error) (int, any) {
			return 422, map[string]string{"msg": e.Error()}
		})
		rec := httptest.NewRecorder()
		httpx.Error(rec, errors.New("bad"))
		fwOK("httpx.SetErrorHandler code", rec.Code)
		fwOK("httpx.SetErrorHandler body", rec.Body.String())
		// reset global (determinism: no order dependence)
		httpx.SetErrorHandler(nil)
	}

	// SetErrorHandlerCtx (shares the same global slot as SetErrorHandler).
	// When the body is an `error`, doHandleError uses http.Error (text body).
	{
		httpx.SetErrorHandlerCtx(func(_ context.Context, e error) (int, any) {
			return 400, e // error body => http.Error path
		})
		rec := httptest.NewRecorder()
		httpx.ErrorCtx(context.Background(), rec, errors.New("x"))
		fwOK("httpx.SetErrorHandlerCtx code", rec.Code)
		fwOK("httpx.SetErrorHandlerCtx body-contains-x", strings.Contains(rec.Body.String(), "x"))
		httpx.SetErrorHandlerCtx(nil)
	}

	// SetOkHandler: wrap the OkJson payload.
	{
		httpx.SetOkHandler(func(_ context.Context, v any) any {
			return map[string]any{"data": v}
		})
		rec := httptest.NewRecorder()
		httpx.OkJsonCtx(context.Background(), rec, 1)
		fwOK("httpx.SetOkHandler body", rec.Body.String())
		httpx.SetOkHandler(nil)
	}

	// Error marshal-failure path (critic gap): a value json cannot marshal
	// makes doWriteJson http.Error with 500. Use a channel (unmarshalable).
	{
		httpx.SetErrorHandler(func(e error) (int, any) {
			return 422, map[string]any{"bad": make(chan int)} // marshal fails
		})
		rec := httptest.NewRecorder()
		httpx.Error(rec, errors.New("x"))
		fwOK("httpx.Error marshal-fail code", rec.Code) // 500 from doWriteJson
		httpx.SetErrorHandler(nil)
	}

	// Stream: fn called repeatedly until it returns false; body is concatenation.
	{
		rec := httptest.NewRecorder()
		chunks := []string{"a", "b", "c"}
		i := 0
		httpx.Stream(context.Background(), rec, func(w io.Writer) bool {
			_, _ = io.WriteString(w, chunks[i])
			i++
			return i < len(chunks)
		})
		fwOK("httpx.Stream body", rec.Body.String())
	}

	// Stream stops immediately when ctx already canceled.
	{
		rec := httptest.NewRecorder()
		cctx, cancel := context.WithCancel(context.Background())
		cancel()
		called := false
		httpx.Stream(cctx, rec, func(w io.Writer) bool { called = true; return false })
		fwOK("httpx.Stream ctx-canceled-no-write", !called && rec.Body.Len() == 0)
	}
}

// ---------------------------------------------------------------------------
// rest/httpx request parsing
// ---------------------------------------------------------------------------

func restRequests() {
	gz_section("rest/httpx request parsing")

	// Parse: form-tagged query params.
	{
		r := httptest.NewRequest(http.MethodGet, "/?name=foo&age=3", nil)
		v := &struct {
			Name string `form:"name"`
			Age  int    `form:"age"`
		}{}
		err := httpx.Parse(r, v)
		fwOK("httpx.Parse err", err)
		fwOK("httpx.Parse Name", v.Name)
		fwOK("httpx.Parse Age", v.Age)
	}

	// ParseJsonBody happy path.
	{
		body := strings.NewReader(`{"name":"a","age":2}`)
		r := httptest.NewRequest(http.MethodPost, "/", body)
		r.Header.Set("Content-Type", "application/json")
		v := &struct {
			Name string `json:"name"`
			Age  int    `json:"age"`
		}{}
		err := httpx.ParseJsonBody(r, v)
		fwOK("httpx.ParseJsonBody err", err)
		fwOK("httpx.ParseJsonBody Name", v.Name)
		fwOK("httpx.ParseJsonBody Age", v.Age)
	}

	// ParseJsonBody missing required field => non-nil error.
	{
		body := strings.NewReader(`{"name":"a"}`)
		r := httptest.NewRequest(http.MethodPost, "/", body)
		r.Header.Set("Content-Type", "application/json")
		v := &struct {
			Name string `json:"name"`
			Age  int    `json:"age"` // required, missing
		}{}
		err := httpx.ParseJsonBody(r, v)
		fwOK("httpx.ParseJsonBody missing-required err!=nil", err != nil)
	}

	// ParseForm: query-string form values.
	{
		r := httptest.NewRequest(http.MethodGet, "/?city=hz&zip=310000", nil)
		v := &struct {
			City string `form:"city"`
			Zip  int    `form:"zip"`
		}{}
		err := httpx.ParseForm(r, v)
		fwOK("httpx.ParseForm err", err)
		fwOK("httpx.ParseForm City", v.City)
		fwOK("httpx.ParseForm Zip", v.Zip)
	}

	// ParseHeaders.
	{
		r := httptest.NewRequest(http.MethodGet, "/", nil)
		r.Header.Set("X-Token", "t")
		v := &struct {
			Token string `header:"X-Token"`
		}{}
		err := httpx.ParseHeaders(r, v)
		fwOK("httpx.ParseHeaders err", err)
		fwOK("httpx.ParseHeaders Token", v.Token)
	}

	// ParseHeader: ";"-separated key=value list => map.
	{
		m := httpx.ParseHeader("key=value;foo=bar")
		fwOK("httpx.ParseHeader key", m["key"])
		fwOK("httpx.ParseHeader foo", m["foo"])
		fwOK("httpx.ParseHeader len", len(m))
	}

	// GetFormValues: query => map[string]any.
	{
		r := httptest.NewRequest(http.MethodGet, "/?a=1&b=x", nil)
		m, err := httpx.GetFormValues(r)
		fwOK("httpx.GetFormValues err", err)
		fwOK("httpx.GetFormValues a", m["a"])
		fwOK("httpx.GetFormValues b", m["b"])
	}

	// GetRemoteAddr: X-Forwarded-For first entry; falls back to RemoteAddr.
	{
		r := httptest.NewRequest(http.MethodGet, "/", nil)
		r.Header.Set("X-Forwarded-For", "1.2.3.4, 5.6.7.8")
		fwOK("httpx.GetRemoteAddr xff", httpx.GetRemoteAddr(r))

		r2 := httptest.NewRequest(http.MethodGet, "/", nil)
		r2.Header.Del("X-Forwarded-For")
		r2.RemoteAddr = "9.9.9.9:1234"
		fwOK("httpx.GetRemoteAddr fallback", httpx.GetRemoteAddr(r2))
	}

	// SetValidator: validator error surfaces through Parse; reset after.
	{
		httpx.SetValidator(gz_stubValidator{shouldFail: true})
		r := httptest.NewRequest(http.MethodGet, "/?name=foo", nil)
		v := &struct {
			Name string `form:"name"`
		}{}
		err := httpx.Parse(r, v)
		fwOK("httpx.SetValidator fail err!=nil", err != nil)

		httpx.SetValidator(gz_stubValidator{shouldFail: false})
		r2 := httptest.NewRequest(http.MethodGet, "/?name=foo", nil)
		v2 := &struct {
			Name string `form:"name"`
		}{}
		err2 := httpx.Parse(r2, v2)
		fwOK("httpx.SetValidator pass err", err2)
		// reset global validator to nil to avoid cross-section order dependence
		httpx.SetValidator(nil)
	}
}

type gz_stubValidator struct{ shouldFail bool }

func (s gz_stubValidator) Validate(_ *http.Request, _ any) error {
	if s.shouldFail {
		return errors.New("validation failed")
	}
	return nil
}

// ---------------------------------------------------------------------------
// rest server + routing (httptest, in-process; no port binding)
// ---------------------------------------------------------------------------

func restServerRouting() {
	gz_section("rest server + routing")

	// MustNewServer / NewServer with a minimal RestConf (Port 0). We do not call
	// Start() (binds a real port + blocks); we exercise routing via Routes() and a
	// standalone router below for deterministic ServeHTTP.
	conf := rest.RestConf{Host: "127.0.0.1", Port: 0}
	conf.Name = "carpet"
	conf.Log.Mode = "console"
	conf.Log.ServiceName = "carpet"
	srv, err := rest.NewServer(conf)
	fwOK("rest.NewServer err", err)
	fwOK("rest.NewServer non-nil", srv != nil)

	// AddRoute / AddRoutes / Routes()
	srv.AddRoute(rest.Route{
		Method: http.MethodGet, Path: "/ping",
		Handler: func(w http.ResponseWriter, _ *http.Request) { httpx.OkJson(w, "pong") },
	})
	srv.AddRoutes([]rest.Route{
		{Method: http.MethodPost, Path: "/echo", Handler: func(w http.ResponseWriter, _ *http.Request) {}},
	})
	routes := srv.Routes()
	fwOK("rest.Server.Routes len", len(routes))
	// sort for deterministic membership reporting
	got := make([]string, 0, len(routes))
	for _, rt := range routes {
		got = append(got, rt.Method+" "+rt.Path)
	}
	sort.Strings(got)
	fwOK("rest.Server.Routes set", strings.Join(got, ","))
	srv.Stop() // just closes logx, safe without Start

	// router.NewRouter(): Handle + ServeHTTP, path var via httpx.ParsePath.
	{
		r := router.NewRouter()
		herr := r.Handle(http.MethodGet, "/u/:id", http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
			var p struct {
				ID int `path:"id"`
			}
			if e := httpx.ParsePath(req, &p); e != nil {
				w.WriteHeader(500)
				return
			}
			httpx.OkJson(w, map[string]int{"id": p.ID})
		}))
		fwOK("router.Handle err", herr)
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, "/u/42", nil)
		r.ServeHTTP(rec, req)
		fwOK("router ServeHTTP code", rec.Code)
		fwOK("router ParsePath body", rec.Body.String())
	}

	// SetNotFoundHandler.
	{
		r := router.NewRouter()
		_ = r.Handle(http.MethodGet, "/known", http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
		r.SetNotFoundHandler(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(404)
			_, _ = io.WriteString(w, "custom-404")
		}))
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, "/nope", nil)
		r.ServeHTTP(rec, req)
		fwOK("router SetNotFoundHandler code", rec.Code)
		fwOK("router SetNotFoundHandler body", rec.Body.String())
	}

	// SetNotAllowedHandler: register GET only, request POST.
	{
		r := router.NewRouter()
		_ = r.Handle(http.MethodGet, "/x", http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
		r.SetNotAllowedHandler(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(405)
			_, _ = io.WriteString(w, "not-allowed")
		}))
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodPost, "/x", nil)
		r.ServeHTTP(rec, req)
		fwOK("router SetNotAllowedHandler code", rec.Code)
		fwOK("router SetNotAllowedHandler body", rec.Body.String())
	}

	// rest.WithMiddleware: wrap a Route's handler. The middleware sets a header,
	// then calls next which writes a body.
	{
		mw := func(next http.HandlerFunc) http.HandlerFunc {
			return func(w http.ResponseWriter, req *http.Request) {
				w.Header().Set("X-MW", "1")
				next(w, req)
			}
		}
		base := rest.Route{
			Method: http.MethodGet, Path: "/m",
			Handler: func(w http.ResponseWriter, _ *http.Request) { _, _ = io.WriteString(w, "handler") },
		}
		wrapped := rest.WithMiddleware(mw, base)
		fwOK("rest.WithMiddleware count", len(wrapped))
		r := router.NewRouter()
		_ = r.Handle(wrapped[0].Method, wrapped[0].Path, wrapped[0].Handler)
		rec := httptest.NewRecorder()
		r.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/m", nil))
		fwOK("rest.WithMiddleware header", rec.Header().Get("X-MW"))
		fwOK("rest.WithMiddleware body", rec.Body.String())
	}

	// rest.WithMiddlewares: outer-first application order (m0 wraps m1 wraps handler).
	{
		order := &[]string{}
		mkmw := func(tag string) rest.Middleware {
			return func(next http.HandlerFunc) http.HandlerFunc {
				return func(w http.ResponseWriter, req *http.Request) {
					*order = append(*order, tag)
					next(w, req)
				}
			}
		}
		base := rest.Route{
			Method: http.MethodGet, Path: "/mm",
			Handler: func(w http.ResponseWriter, _ *http.Request) { *order = append(*order, "h") },
		}
		wrapped := rest.WithMiddlewares([]rest.Middleware{mkmw("m0"), mkmw("m1")}, base)
		r := router.NewRouter()
		_ = r.Handle(wrapped[0].Method, wrapped[0].Path, wrapped[0].Handler)
		r.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/mm", nil))
		fwOK("rest.WithMiddlewares order", strings.Join(*order, ","))
	}

	// rest.ToMiddleware: adapt a std net/http middleware (func(http.Handler) http.Handler).
	{
		stdmw := func(next http.Handler) http.Handler {
			return http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				w.Header().Set("X-Adapted", "yes")
				next.ServeHTTP(w, req)
			})
		}
		mw := rest.ToMiddleware(stdmw)
		base := rest.Route{
			Method: http.MethodGet, Path: "/a",
			Handler: func(w http.ResponseWriter, _ *http.Request) { _, _ = io.WriteString(w, "ok") },
		}
		wrapped := rest.WithMiddleware(mw, base)
		r := router.NewRouter()
		_ = r.Handle(wrapped[0].Method, wrapped[0].Path, wrapped[0].Handler)
		rec := httptest.NewRecorder()
		r.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/a", nil))
		fwOK("rest.ToMiddleware header", rec.Header().Get("X-Adapted"))
		fwOK("rest.ToMiddleware body", rec.Body.String())
	}

	// (*Server).Use: global middleware. We verify it does not error and Routes()
	// remains intact (Server.serve/build are unexported; full ServeHTTP needs
	// Start). Use is exercised for API coverage; the dynamic effect is covered by
	// WithMiddleware above.
	{
		s2, e := rest.NewServer(conf)
		fwOK("rest.Server.Use new-server err", e)
		s2.Use(func(next http.HandlerFunc) http.HandlerFunc {
			return func(w http.ResponseWriter, req *http.Request) { next(w, req) }
		})
		s2.AddRoute(rest.Route{Method: http.MethodGet, Path: "/g", Handler: func(http.ResponseWriter, *http.Request) {}})
		fwOK("rest.Server.Use routes-len", len(s2.Routes()))
		s2.Stop()
	}
}

// ---------------------------------------------------------------------------
// zrpc server/client (bufconn + real grpc health stub; deterministic, no net)
// ---------------------------------------------------------------------------

// gz_healthImpl is a deterministic HealthServer: returns SERVING for "ok", and an
// optional sleep for "slow" to exercise WithCallTimeout deadline.
type gz_healthImpl struct {
	healthpb.UnimplementedHealthServer
	slow time.Duration
}

func (h *gz_healthImpl) Check(ctx context.Context, req *healthpb.HealthCheckRequest) (*healthpb.HealthCheckResponse, error) {
	if req.GetService() == "slow" && h.slow > 0 {
		select {
		case <-time.After(h.slow):
		case <-ctx.Done():
			return nil, status.FromContextError(ctx.Err()).Err()
		}
	}
	if req.GetService() == "missing" {
		return nil, status.Error(codes.NotFound, "unknown service")
	}
	return &healthpb.HealthCheckResponse{Status: healthpb.HealthCheckResponse_SERVING}, nil
}

func zrpcSection() {
	gz_section("zrpc server/client (bufconn)")

	// NewDirectClientConf: pure value check (no dial).
	{
		conf := zrpc.NewDirectClientConf([]string{"127.0.0.1:0"}, "app", "token")
		fwOK("zrpc.NewDirectClientConf Endpoints", strings.Join(conf.Endpoints, ","))
		fwOK("zrpc.NewDirectClientConf App", conf.App)
		fwOK("zrpc.NewDirectClientConf Token", conf.Token)
	}

	// Build a grpc server over bufconn with a stacked, order-recording unary
	// interceptor chain (mirrors zrpc.AddUnaryInterceptors semantics: registration
	// order = execution order before the handler).
	const bufSize = 1024 * 1024
	lis := bufconn.Listen(bufSize)

	var orderMu sync.Mutex
	var order []string
	mkInterceptor := func(tag string) grpc.UnaryServerInterceptor {
		return func(ctx context.Context, req any, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (any, error) {
			orderMu.Lock()
			order = append(order, tag)
			orderMu.Unlock()
			return handler(ctx, req)
		}
	}

	gs := grpc.NewServer(grpc.ChainUnaryInterceptor(mkInterceptor("i0"), mkInterceptor("i1")))
	healthpb.RegisterHealthServer(gs, &gz_healthImpl{slow: 200 * time.Millisecond})
	serveErr := make(chan error, 1)
	go func() { serveErr <- gs.Serve(lis) }()
	defer func() {
		gs.Stop()
		<-serveErr
	}()

	bufDialer := func(context.Context, string) (net.Conn, error) { return lis.Dial() }

	// zrpc.NewClientWithTarget + zrpc.WithDialOption(grpc.WithContextDialer + insecure):
	// connects entirely over the in-memory bufconn (no real network).
	client, err := zrpc.NewClientWithTarget(
		"passthrough:///bufnet",
		zrpc.WithDialOption(grpc.WithContextDialer(bufDialer)),
		zrpc.WithDialOption(grpc.WithTransportCredentials(insecure.NewCredentials())),
	)
	fwOK("zrpc.NewClientWithTarget err", err)
	fwOK("zrpc.NewClientWithTarget conn non-nil", client != nil && client.Conn() != nil)

	stub := healthpb.NewHealthClient(client.Conn())

	// Happy path unary RPC over bufconn.
	{
		resp, e := stub.Check(context.Background(), &healthpb.HealthCheckRequest{Service: "ok"})
		fwOK("zrpc bufconn Check err", e)
		fwOK("zrpc bufconn Check status", resp.GetStatus().String())
	}

	// Forced error -> codes.NotFound.
	{
		_, e := stub.Check(context.Background(), &healthpb.HealthCheckRequest{Service: "missing"})
		fwOK("zrpc bufconn forced-error code", status.Code(e).String())
	}

	// Interceptor stacking order: i0 then i1 (registration order) before handler.
	{
		orderMu.Lock()
		recorded := append([]string(nil), order...)
		orderMu.Unlock()
		// take the first two entries from the latest invocation tail
		var last2 []string
		if len(recorded) >= 2 {
			last2 = recorded[len(recorded)-2:]
		}
		fwOK("zrpc interceptor order", strings.Join(last2, ","))
	}

	// zrpc.WithCallTimeout: server sleeps 200ms; call with 50ms timeout =>
	// DeadlineExceeded. WithCallTimeout returns a grpc.CallOption.
	{
		_, e := stub.Check(context.Background(),
			&healthpb.HealthCheckRequest{Service: "slow"},
			zrpc.WithCallTimeout(50*time.Millisecond))
		fwOK("zrpc.WithCallTimeout code", status.Code(e).String())
	}
}

// ---------------------------------------------------------------------------
// core/stringx
// ---------------------------------------------------------------------------

func stringxSection() {
	gz_section("core/stringx")

	// Rand/Randn/RandId: assert length + non-empty (values are random; we Seed for
	// reproducibility of the RNG state but only assert structural invariants).
	stringx.Seed(1)
	fwOK("stringx.Rand len==8", len(stringx.Rand()) == 8)
	fwOK("stringx.Randn(16) len==16", len(stringx.Randn(16)) == 16)
	fwOK("stringx.RandId non-empty", len(stringx.RandId()) > 0)

	// Filter
	fwOK("stringx.Filter", stringx.Filter("ab1cd2", func(r rune) bool { return r >= '0' && r <= '9' }))

	// Substr (rune-aware) + error cases
	s1, e1 := stringx.Substr("abcdef", 1, 4)
	fwOK("stringx.Substr ascii", s1)
	fwOK("stringx.Substr ascii err", e1)
	s2, e2 := stringx.Substr("你好世界", 0, 2)
	fwOK("stringx.Substr utf8", s2)
	fwOK("stringx.Substr utf8 err", e2)
	_, e3 := stringx.Substr("abc", -1, 2)
	fwOK("stringx.Substr neg-start err==ErrInvalidStartPosition", errors.Is(e3, stringx.ErrInvalidStartPosition))

	// ToCamelCase
	fwOK("stringx.ToCamelCase", stringx.ToCamelCase("HelloWorld"))

	// Reverse / FirstN (+ellipsis) / Contains / HasEmpty / NotEmpty / TakeOne / TakeWithPriority
	fwOK("stringx.Reverse", stringx.Reverse("abc"))
	fwOK("stringx.FirstN", stringx.FirstN("hello", 3))
	fwOK("stringx.FirstN ellipsis", stringx.FirstN("hello", 3, "..."))
	fwOK("stringx.FirstN n>len", stringx.FirstN("hi", 5))
	fwOK("stringx.Contains true", stringx.Contains([]string{"a", "b"}, "a"))
	fwOK("stringx.Contains false", stringx.Contains([]string{"a"}, "z"))
	fwOK("stringx.HasEmpty", stringx.HasEmpty("", "x"))
	fwOK("stringx.NotEmpty", stringx.NotEmpty("a", "b"))
	fwOK("stringx.TakeOne valid", stringx.TakeOne("kept", "def"))
	fwOK("stringx.TakeOne empty", stringx.TakeOne("", "def"))
	fwOK("stringx.TakeWithPriority", stringx.TakeWithPriority(
		func() string { return "" },
		func() string { return "second" },
	))

	// Replacer: longest-match keyword replacement
	fwOK("stringx.NewReplacer", stringx.NewReplacer(map[string]string{"foo": "bar"}).Replace("a foo b"))

	// Trie: Filter (default mask '*') + FindKeywords + WithMask
	tr := stringx.NewTrie([]string{"bad"})
	sentence, keywords, found := tr.Filter("this is bad")
	fwOK("stringx.Trie.Filter sentence", sentence)
	fwOK("stringx.Trie.Filter keywords", strings.Join(keywords, ","))
	fwOK("stringx.Trie.Filter found", found)
	fwOK("stringx.Trie.FindKeywords", strings.Join(tr.FindKeywords("a bad day"), ","))
	trMask := stringx.NewTrie([]string{"bad"}, stringx.WithMask('#'))
	sm, _, _ := trMask.Filter("very bad")
	fwOK("stringx.Trie WithMask", sm)

	// Union / Remove / Join
	fwOK("stringx.Union", strings.Join(gz_sortedCopy(stringx.Union([]string{"a"}, []string{"a", "b"})), ","))
	fwOK("stringx.Remove", strings.Join(stringx.Remove([]string{"a", "b", "c"}, "a", "c"), ","))
	fwOK("stringx.Join", stringx.Join('-', "x", "y", "z"))
}

func gz_sortedCopy(in []string) []string {
	out := append([]string(nil), in...)
	sort.Strings(out)
	return out
}

// ---------------------------------------------------------------------------
// core/mathx
// ---------------------------------------------------------------------------

func mathxSection() {
	gz_section("core/mathx")

	// Between / AtLeast / AtMost (generic clamps)
	fwOK("mathx.Between hi", mathx.Between(5, 1, 3))
	fwOK("mathx.Between lo", mathx.Between(-2, 1, 3))
	fwOK("mathx.Between in", mathx.Between(2, 1, 3))
	fwOK("mathx.AtLeast", mathx.AtLeast(2, 5))
	fwOK("mathx.AtMost", mathx.AtMost(9, 4))
	fwOK("mathx.Between float", mathx.Between(2.5, 0.0, 2.0))

	// MaxInt / MinInt
	fwOK("mathx.MaxInt", mathx.MaxInt(3, 7))
	fwOK("mathx.MinInt", mathx.MinInt(3, 7))

	// Unstable: deviation 0 => exact base; deviation 0.05 => bounded.
	u0 := mathx.NewUnstable(0)
	fwOK("mathx.Unstable dev0 AroundInt", u0.AroundInt(1000))
	fwOK("mathx.Unstable dev0 AroundDuration", u0.AroundDuration(time.Second))
	u := mathx.NewUnstable(0.05)
	allIn := true
	for i := 0; i < 1000; i++ {
		v := u.AroundInt(1000)
		if v < 950 || v > 1050 {
			allIn = false
			break
		}
	}
	fwOK("mathx.Unstable dev0.05 bounded[950,1050]", allIn)

	// Proba: 1.0 => true; 0.0 => false (deterministic edges)
	p := mathx.NewProba()
	t1, f1 := true, false
	for i := 0; i < 1000; i++ {
		if !p.TrueOnProba(1.0) {
			t1 = false
		}
		if p.TrueOnProba(0.0) {
			f1 = true
		}
	}
	fwOK("mathx.Proba TrueOnProba(1.0) always-true", t1)
	fwOK("mathx.Proba TrueOnProba(0.0) always-false", !f1)

	// CalcEntropy: 2 equal buckets => 1.0; single bucket => 1 (doc behavior:
	// len<=1 returns 1, not 0).
	fwOK("mathx.CalcEntropy 2-equal", mathx.CalcEntropy(map[any]int{"a": 1, "b": 1}))
	fwOK("mathx.CalcEntropy single", mathx.CalcEntropy(map[any]int{"a": 5}))
}

// ---------------------------------------------------------------------------
// core/collection
// ---------------------------------------------------------------------------

func collectionSection() {
	gz_section("core/collection")

	// Set[T]: Add/Count/Contains/Remove/Clear/Keys
	s := collection.NewSet[int]()
	s.Add(1, 2, 2)
	fwOK("collection.Set Count", s.Count())
	fwOK("collection.Set Contains(2)", s.Contains(2))
	fwOK("collection.Set Contains(9)", s.Contains(9))
	fwOK("collection.Set Keys-sorted", fmt.Sprint(gz_sortedInts(s.Keys())))
	s.Remove(1)
	fwOK("collection.Set after Remove Count", s.Count())
	s.Clear()
	fwOK("collection.Set after Clear Count", s.Count())

	// Ring: last n overwrite oldest, Take oldest->newest
	r := collection.NewRing(3)
	for _, v := range []int{1, 2, 3, 4, 5} {
		r.Add(v)
	}
	fwOK("collection.Ring Take", fmt.Sprint(r.Take()))

	// Cache: Set/Get/Del/Take/SetWithExpire; WithLimit/WithName options.
	c, cerr := collection.NewCache(time.Minute, collection.WithLimit(100), collection.WithName("carpet"))
	fwOK("collection.NewCache err", cerr)
	c.Set("k", 1)
	v, okGet := c.Get("k")
	fwOK("collection.Cache Get value", v)
	fwOK("collection.Cache Get ok", okGet)
	c.Del("k")
	_, okGet2 := c.Get("k")
	fwOK("collection.Cache Get after Del", okGet2)
	// Take: single-flight fetch, cached after first call.
	var fetchCount int32
	tv, terr := c.Take("t", func() (any, error) {
		atomic.AddInt32(&fetchCount, 1)
		return 99, nil
	})
	fwOK("collection.Cache Take value", tv)
	fwOK("collection.Cache Take err", terr)
	_, _ = c.Take("t", func() (any, error) { atomic.AddInt32(&fetchCount, 1); return -1, nil })
	fwOK("collection.Cache Take fetch-once", atomic.LoadInt32(&fetchCount) == 1)
	c.SetWithExpire("e", 7, time.Hour)
	ev, _ := c.Get("e")
	fwOK("collection.Cache SetWithExpire Get", ev)

	// SafeMap: Set/Get/Size/Del/Range
	m := collection.NewSafeMap()
	m.Set("a", 1)
	m.Set("b", 2)
	mv, mok := m.Get("a")
	fwOK("collection.SafeMap Get value", mv)
	fwOK("collection.SafeMap Get ok", mok)
	fwOK("collection.SafeMap Size", m.Size())
	cnt := 0
	m.Range(func(_, _ any) bool { cnt++; return true })
	fwOK("collection.SafeMap Range count", cnt)
	m.Del("a")
	fwOK("collection.SafeMap Size after Del", m.Size())

	// Queue: FIFO
	q := collection.NewQueue(4)
	q.Put(1)
	q.Put(2)
	fwOK("collection.Queue Empty before take", q.Empty())
	a, aok := q.Take()
	fwOK("collection.Queue Take first", a)
	fwOK("collection.Queue Take ok", aok)
	_, _ = q.Take()
	fwOK("collection.Queue Empty after drain", q.Empty())
	_, eok := q.Take()
	fwOK("collection.Queue Take on empty ok", eok)

	// TimingWheel with a FakeTicker (deterministic): SetTimer then Tick enough to
	// fire; synchronize via a buffered channel from the execute callback.
	{
		ticker := timex.NewFakeTicker()
		fired := make(chan [2]any, 1)
		tw, twerr := collection.NewTimingWheelWithTicker(
			gz_testStep, 10,
			func(k, val any) { fired <- [2]any{k, val} },
			ticker,
		)
		fwOK("collection.NewTimingWheelWithTicker err", twerr)
		_ = tw.SetTimer("job", 7, gz_testStep*2)
		// advance ticks until callback fires (bounded loop for determinism)
		var got [2]any
		gotFired := false
		for i := 0; i < 10 && !gotFired; i++ {
			ticker.Tick()
			select {
			case got = <-fired:
				gotFired = true
			case <-time.After(50 * time.Millisecond):
			}
		}
		tw.Stop()
		fwOK("collection.TimingWheel fired", gotFired)
		fwOK("collection.TimingWheel key", got[0])
		fwOK("collection.TimingWheel value", got[1])
	}

	// RollingWindow[float64, *Bucket[float64]]: Add then Reduce over buckets.
	{
		rw := collection.NewRollingWindow[float64, *collection.Bucket[float64]](
			func() *collection.Bucket[float64] { return new(collection.Bucket[float64]) },
			3, time.Hour, // long interval => no bucket rolls during test
		)
		rw.Add(1)
		rw.Add(2)
		rw.Add(3)
		var sum float64
		rw.Reduce(func(b *collection.Bucket[float64]) { sum += b.Sum })
		fwOK("collection.RollingWindow Reduce sum", sum)

		// IgnoreCurrentBucket option: current in-progress bucket is excluded; with
		// a long interval everything lands in the current bucket, so Reduce sees 0.
		rwIgnore := collection.NewRollingWindow[float64, *collection.Bucket[float64]](
			func() *collection.Bucket[float64] { return new(collection.Bucket[float64]) },
			3, time.Hour,
			collection.IgnoreCurrentBucket[float64, *collection.Bucket[float64]](),
		)
		rwIgnore.Add(10)
		var sum2 float64
		rwIgnore.Reduce(func(b *collection.Bucket[float64]) { sum2 += b.Sum })
		fwOK("collection.RollingWindow IgnoreCurrentBucket sum", sum2)
	}
}

const gz_testStep = time.Millisecond * 50

func gz_sortedInts(in []int) []int {
	out := append([]int(nil), in...)
	sort.Ints(out)
	return out
}

// ---------------------------------------------------------------------------
// core/syncx
// ---------------------------------------------------------------------------

func syncxSection() {
	gz_section("core/syncx")

	// SingleFlight.Do: N concurrent callers with the same key share one execution.
	{
		sf := syncx.NewSingleFlight()
		var calls int32
		release := make(chan struct{})
		var wg sync.WaitGroup
		results := make([]any, 8)
		for i := 0; i < 8; i++ {
			wg.Add(1)
			go func(idx int) {
				defer wg.Done()
				v, _ := sf.Do("k", func() (any, error) {
					atomic.AddInt32(&calls, 1)
					<-release
					return 123, nil
				})
				results[idx] = v
			}(i)
		}
		// give goroutines time to coalesce on the same key
		for atomic.LoadInt32(&calls) == 0 {
			time.Sleep(time.Millisecond)
		}
		time.Sleep(5 * time.Millisecond)
		close(release)
		wg.Wait()
		fwOK("syncx.SingleFlight call-once", atomic.LoadInt32(&calls) == 1)
		same := true
		for _, r := range results {
			if r != 123 {
				same = false
			}
		}
		fwOK("syncx.SingleFlight all-same-result", same)
	}

	// SingleFlight.DoEx: fresh==true for the executing caller.
	{
		sf := syncx.NewSingleFlight()
		v, fresh, e := sf.DoEx("dk", func() (any, error) { return 5, nil })
		fwOK("syncx.SingleFlight DoEx value", v)
		fwOK("syncx.SingleFlight DoEx fresh", fresh)
		fwOK("syncx.SingleFlight DoEx err", e)
	}

	// Limit: Borrow/TryBorrow/Return + ErrLimitReturn on over-return.
	{
		l := syncx.NewLimit(1)
		l.Borrow()
		fwOK("syncx.Limit TryBorrow when-full", l.TryBorrow())
		retErr := l.Return()
		fwOK("syncx.Limit Return err", retErr)
		fwOK("syncx.Limit TryBorrow after-return", l.TryBorrow())
		_ = l.Return()        // back to 0
		overErr := l.Return() // over-return
		fwOK("syncx.Limit over-return err==ErrLimitReturn", errors.Is(overErr, syncx.ErrLimitReturn))
	}

	// AtomicBool
	{
		b := syncx.ForAtomicBool(false)
		b.Set(true)
		fwOK("syncx.AtomicBool True after Set", b.True())
		fwOK("syncx.AtomicBool CAS(true,false)", b.CompareAndSwap(true, false))
		fwOK("syncx.AtomicBool True after CAS", b.True())
		fwOK("syncx.AtomicBool CAS wrong-old", b.CompareAndSwap(true, true))
	}

	// AtomicFloat64
	{
		f := syncx.ForAtomicFloat64(1.0)
		f.Add(0.5)
		fwOK("syncx.AtomicFloat64 Load after Add", f.Load())
		fwOK("syncx.AtomicFloat64 CAS(1.5,2.0)", f.CompareAndSwap(1.5, 2.0))
		fwOK("syncx.AtomicFloat64 Load after CAS", f.Load())
	}

	// AtomicDuration
	{
		d := syncx.ForAtomicDuration(time.Second)
		d.Set(2 * time.Second)
		fwOK("syncx.AtomicDuration Load", d.Load())
		fwOK("syncx.AtomicDuration CAS", d.CompareAndSwap(2*time.Second, 3*time.Second))
		fwOK("syncx.AtomicDuration Load after CAS", d.Load())
	}

	// Cond.WaitWithTimeout: signaled-before-timeout => ok==true.
	{
		c := syncx.NewCond()
		go func() {
			time.Sleep(10 * time.Millisecond)
			c.Signal()
		}()
		_, signaled := c.WaitWithTimeout(time.Second)
		fwOK("syncx.Cond signaled-ok", signaled)

		c2 := syncx.NewCond()
		_, timedOut := c2.WaitWithTimeout(5 * time.Millisecond) // no signal
		fwOK("syncx.Cond timeout-ok==false", timedOut)
	}

	// DoneChan: idempotent Close; Done() closed channel receive doesn't block.
	{
		dc := syncx.NewDoneChan()
		dc.Close()
		dc.Close() // must not panic
		select {
		case <-dc.Done():
			fwOK("syncx.DoneChan receive-immediate", true)
		case <-time.After(time.Second):
			fwOK("syncx.DoneChan receive-immediate", false)
		}
	}

	// Pool: limited resource pool, reuse on Put.
	{
		var created int32
		pool := syncx.NewPool(2,
			func() any { atomic.AddInt32(&created, 1); return new(int) },
			func(any) {},
		)
		x := pool.Get()
		pool.Put(x)
		_ = pool.Get() // should reuse
		fwOK("syncx.Pool reuse create<=2", atomic.LoadInt32(&created) <= 2)
	}

	// Once: underlying fn runs exactly once.
	{
		var n int32
		f := syncx.Once(func() { atomic.AddInt32(&n, 1) })
		f()
		f()
		f()
		fwOK("syncx.Once run-once", atomic.LoadInt32(&n) == 1)
	}

	// Guard: fn runs under the given lock.
	{
		var mu sync.Mutex
		ran := false
		syncx.Guard(&mu, func() { ran = true })
		fwOK("syncx.Guard ran", ran)
	}
}

// ---------------------------------------------------------------------------
// core/fx functional pipeline
// ---------------------------------------------------------------------------

func fxSection() {
	gz_section("core/fx")

	// From / Just / Count
	fwOK("fx.Just Count", fx.Just(1, 2, 3).Count())
	fwOK("fx.From Count", fx.From(func(source chan<- any) {
		for i := 0; i < 4; i++ {
			source <- i
		}
	}).Count())

	// Map + Reduce (sum)
	mapped, _ := fx.Just(1, 2, 3).
		Map(func(i any) any { return i.(int) * 2 }).
		Reduce(func(pipe <-chan any) (any, error) {
			s := 0
			for v := range pipe {
				s += v.(int)
			}
			return s, nil
		})
	fwOK("fx.Map+Reduce sum", mapped)

	// Filter
	fwOK("fx.Filter evens Count", fx.Just(1, 2, 3, 4).
		Filter(func(i any) bool { return i.(int)%2 == 0 }).Count())

	// Reduce direct
	red, rerr := fx.Just(1, 2, 3).Reduce(func(pipe <-chan any) (any, error) {
		s := 0
		for v := range pipe {
			s += v.(int)
		}
		return s, nil
	})
	fwOK("fx.Reduce value", red)
	fwOK("fx.Reduce err", rerr)

	// Distinct
	fwOK("fx.Distinct Count", fx.Just(1, 1, 2, 3, 3).
		Distinct(func(i any) any { return i }).Count())

	// Sort + collect via ForEach (deterministic order)
	{
		var sorted []int
		fx.Just(3, 1, 2).
			Sort(func(a, b any) bool { return a.(int) < b.(int) }).
			ForEach(func(i any) { sorted = append(sorted, i.(int)) })
		fwOK("fx.Sort order", fmt.Sprint(sorted))
	}

	// Reverse
	{
		var rev []int
		fx.Just(1, 2, 3).Reverse().ForEach(func(i any) { rev = append(rev, i.(int)) })
		fwOK("fx.Reverse order", fmt.Sprint(rev))
	}

	// Head / Tail / Skip
	fwOK("fx.Head(2) Count", fx.Just(1, 2, 3, 4).Head(2).Count())
	fwOK("fx.Tail(1) Count", fx.Just(1, 2, 3, 4).Tail(1).Count())
	fwOK("fx.Skip(1) Count", fx.Just(1, 2, 3, 4).Skip(1).Count())
	{
		var head []int
		fx.Just(1, 2, 3, 4).
			Sort(func(a, b any) bool { return a.(int) < b.(int) }).
			Head(2).ForEach(func(i any) { head = append(head, i.(int)) })
		fwOK("fx.Head(2) values", fmt.Sprint(head))
	}

	// AllMatch / AnyMatch / NoneMatch / First / Last
	even := func(i any) bool { return i.(int)%2 == 0 }
	fwOK("fx.AllMatch", fx.Just(2, 4).AllMatch(even))
	fwOK("fx.AnyMatch", fx.Just(1, 2).AnyMatch(even))
	fwOK("fx.NoneMatch", fx.Just(1, 3).NoneMatch(even))
	fwOK("fx.First", fx.Just(2, 4).First())
	fwOK("fx.Last", fx.Just(2, 4).Last())

	// Group: keys -> groups; assert group count (each group is a []any). Group
	// order is not guaranteed, so only Count is asserted.
	fwOK("fx.Group Count", fx.Just(1, 2, 3, 4).
		Group(func(i any) any { return i.(int) % 2 }).Count())

	// Parallel: all fns run.
	{
		var n int32
		fx.Parallel(
			func() { atomic.AddInt32(&n, 1) },
			func() { atomic.AddInt32(&n, 1) },
			func() { atomic.AddInt32(&n, 1) },
		)
		fwOK("fx.Parallel ran-all", atomic.LoadInt32(&n))
	}

	// ParallelErr: returns the first error.
	{
		perr := fx.ParallelErr(
			func() error { return nil },
			func() error { return errors.New("pe") },
		)
		fwOK("fx.ParallelErr err!=nil", perr != nil)
	}

	// DoWithRetry: succeed on 3rd attempt; default times=3 (loop runs fn up to 3x).
	{
		var attempts int32
		err := fx.DoWithRetry(func() error {
			n := atomic.AddInt32(&attempts, 1)
			if n < 3 {
				return errors.New("fail")
			}
			return nil
		})
		fwOK("fx.DoWithRetry success err", err)
		fwOK("fx.DoWithRetry attempts", atomic.LoadInt32(&attempts))
	}

	// DoWithRetry always-fail with WithRetry(2): fn runs exactly 2 times, returns error.
	{
		var attempts int32
		err := fx.DoWithRetry(func() error {
			atomic.AddInt32(&attempts, 1)
			return errors.New("always")
		}, fx.WithRetry(2))
		fwOK("fx.DoWithRetry always-fail err!=nil", err != nil)
		fwOK("fx.DoWithRetry always-fail attempts", atomic.LoadInt32(&attempts))
	}

	// DoWithRetry WithIgnoreErrors: ignored error => returns nil.
	{
		ignored := errors.New("ignored-sentinel")
		err := fx.DoWithRetry(func() error { return ignored },
			fx.WithIgnoreErrors([]error{ignored}))
		fwOK("fx.DoWithRetry WithIgnoreErrors nil", err)
	}

	// DoWithTimeout: slow fn => DeadlineExceeded (fx.ErrTimeout); fast => nil.
	{
		slow := fx.DoWithTimeout(func() error {
			time.Sleep(100 * time.Millisecond)
			return nil
		}, 10*time.Millisecond)
		fwOK("fx.DoWithTimeout slow==ErrTimeout", errors.Is(slow, fx.ErrTimeout))

		fast := fx.DoWithTimeout(func() error { return nil }, time.Second)
		fwOK("fx.DoWithTimeout fast err", fast)
	}

	// DoWithTimeout WithContext: uses provided parent context.
	{
		err := fx.DoWithTimeout(func() error { return nil }, time.Second,
			fx.WithContext(context.Background()))
		fwOK("fx.DoWithTimeout WithContext err", err)
	}
}

// ---------------------------------------------------------------------------
// core/mr (MapReduce) -- listed in critic gaps + carpet rules
// ---------------------------------------------------------------------------

func mrSection() {
	gz_section("core/mr")

	// MapReduce: square each of 1..5, sum the squares (1+4+9+16+25 = 55).
	{
		v, err := mr.MapReduce(
			func(source chan<- int) {
				for i := 1; i <= 5; i++ {
					source <- i
				}
			},
			func(item int, writer mr.Writer[int], _ func(error)) {
				writer.Write(item * item)
			},
			func(pipe <-chan int, writer mr.Writer[int], _ func(error)) {
				sum := 0
				for v := range pipe {
					sum += v
				}
				writer.Write(sum)
			},
		)
		fwOK("mr.MapReduce sum-of-squares err", err)
		fwOK("mr.MapReduce sum-of-squares", v)
	}

	// MapReduceVoid: accumulate via atomic (no output value).
	{
		var total int64
		err := mr.MapReduceVoid(
			func(source chan<- int) {
				for i := 1; i <= 4; i++ {
					source <- i
				}
			},
			func(item int, writer mr.Writer[int], _ func(error)) {
				writer.Write(item)
			},
			func(pipe <-chan int, _ func(error)) {
				for v := range pipe {
					atomic.AddInt64(&total, int64(v))
				}
			},
		)
		fwOK("mr.MapReduceVoid err", err)
		fwOK("mr.MapReduceVoid total", atomic.LoadInt64(&total))
	}

	// Finish: run fns, return first error (nil here).
	{
		var n int32
		err := mr.Finish(
			func() error { atomic.AddInt32(&n, 1); return nil },
			func() error { atomic.AddInt32(&n, 1); return nil },
			func() error { atomic.AddInt32(&n, 1); return nil },
		)
		fwOK("mr.Finish err", err)
		fwOK("mr.Finish ran-all", atomic.LoadInt32(&n))
	}

	// Finish with an error => non-nil.
	{
		err := mr.Finish(
			func() error { return nil },
			func() error { return errors.New("boom") },
		)
		fwOK("mr.Finish error-propagated err!=nil", err != nil)
	}

	// FinishVoid: run fns, no error return.
	{
		var n int32
		mr.FinishVoid(
			func() { atomic.AddInt32(&n, 1) },
			func() { atomic.AddInt32(&n, 1) },
		)
		fwOK("mr.FinishVoid ran-all", atomic.LoadInt32(&n))
	}

	// ForEach: process each item (order not guaranteed -> use atomic count/sum).
	{
		var sum int64
		mr.ForEach(
			func(source chan<- int) {
				for i := 1; i <= 5; i++ {
					source <- i
				}
			},
			func(item int) { atomic.AddInt64(&sum, int64(item)) },
		)
		fwOK("mr.ForEach sum", atomic.LoadInt64(&sum))
	}

	// MapReduce with cancel: mapper cancels => non-nil error path.
	{
		_, err := mr.MapReduce(
			func(source chan<- int) {
				for i := 0; i < 3; i++ {
					source <- i
				}
			},
			func(item int, writer mr.Writer[int], cancel func(error)) {
				if item == 1 {
					cancel(errors.New("canceled-by-mapper"))
					return
				}
				writer.Write(item)
			},
			func(pipe <-chan int, writer mr.Writer[int], _ func(error)) {
				s := 0
				for v := range pipe {
					s += v
				}
				writer.Write(s)
			},
		)
		fwOK("mr.MapReduce cancel err!=nil", err != nil)
	}
}

// ---------------------------------------------------------------------------
// core/jsonx
// ---------------------------------------------------------------------------

func jsonxSection() {
	gz_section("core/jsonx")

	// Marshal / MarshalToString
	bs, merr := jsonx.Marshal(map[string]int{"a": 1})
	fwOK("jsonx.Marshal err", merr)
	fwOK("jsonx.Marshal bytes", string(bs))
	str, serr := jsonx.MarshalToString(map[string]int{"a": 1})
	fwOK("jsonx.MarshalToString err", serr)
	fwOK("jsonx.MarshalToString value", str)

	// Unmarshal / UnmarshalFromString
	{
		var out struct {
			A int `json:"a"`
		}
		uerr := jsonx.Unmarshal([]byte(`{"a":1}`), &out)
		fwOK("jsonx.Unmarshal err", uerr)
		fwOK("jsonx.Unmarshal A", out.A)
	}
	{
		var out struct {
			A int `json:"a"`
		}
		uerr := jsonx.UnmarshalFromString(`{"a":2}`, &out)
		fwOK("jsonx.UnmarshalFromString err", uerr)
		fwOK("jsonx.UnmarshalFromString A", out.A)
	}
	// malformed JSON => non-nil error
	{
		var out struct {
			A int `json:"a"`
		}
		uerr := jsonx.UnmarshalFromString(`{bad`, &out)
		fwOK("jsonx.UnmarshalFromString malformed err!=nil", uerr != nil)
	}

	// UnmarshalFromReader
	{
		var out struct {
			A int `json:"a"`
		}
		uerr := jsonx.UnmarshalFromReader(strings.NewReader(`{"a":3}`), &out)
		fwOK("jsonx.UnmarshalFromReader err", uerr)
		fwOK("jsonx.UnmarshalFromReader A", out.A)
	}

	// large-int preservation (jsonx uses UseNumber internally)
	{
		var out struct {
			N int64 `json:"n"`
		}
		uerr := jsonx.UnmarshalFromString(`{"n":9007199254740993}`, &out)
		fwOK("jsonx.UnmarshalFromString big-int err", uerr)
		fwOK("jsonx.UnmarshalFromString big-int N", out.N)
	}
}

// ---------------------------------------------------------------------------
// core/mapping
// ---------------------------------------------------------------------------

func mappingSection() {
	gz_section("core/mapping")

	// NewUnmarshaler("json").Unmarshal(map, &struct)
	{
		u := mapping.NewUnmarshaler("json")
		var v struct {
			Name string `json:"name"`
			Age  int    `json:"age"`
		}
		err := u.Unmarshal(map[string]any{"name": "a", "age": 3}, &v)
		fwOK("mapping.Unmarshaler err", err)
		fwOK("mapping.Unmarshaler Name", v.Name)
		fwOK("mapping.Unmarshaler Age", v.Age)
	}

	// Missing required field (no optional tag) => error.
	{
		u := mapping.NewUnmarshaler("json")
		var v struct {
			Name string `json:"name"`
			Age  int    `json:"age"` // required, missing
		}
		err := u.Unmarshal(map[string]any{"name": "a"}, &v)
		fwOK("mapping.Unmarshaler missing-required err!=nil", err != nil)
	}

	// UnmarshalKey: uses the "key" tag.
	{
		var v struct {
			Name string `key:"name"`
		}
		err := mapping.UnmarshalKey(map[string]any{"name": "x"}, &v)
		fwOK("mapping.UnmarshalKey err", err)
		fwOK("mapping.UnmarshalKey Name", v.Name)
	}

	// UnmarshalJsonBytes / UnmarshalJsonMap / UnmarshalJsonReader
	{
		var v struct {
			N int `json:"n"`
		}
		err := mapping.UnmarshalJsonBytes([]byte(`{"n":5}`), &v)
		fwOK("mapping.UnmarshalJsonBytes err", err)
		fwOK("mapping.UnmarshalJsonBytes N", v.N)
	}
	{
		var v struct {
			N int `json:"n"`
		}
		err := mapping.UnmarshalJsonMap(map[string]any{"n": 6}, &v)
		fwOK("mapping.UnmarshalJsonMap err", err)
		fwOK("mapping.UnmarshalJsonMap N", v.N)
	}
	{
		var v struct {
			N int `json:"n"`
		}
		err := mapping.UnmarshalJsonReader(strings.NewReader(`{"n":7}`), &v)
		fwOK("mapping.UnmarshalJsonReader err", err)
		fwOK("mapping.UnmarshalJsonReader N", v.N)
	}

	// WithDefault: missing field gets default value.
	{
		u := mapping.NewUnmarshaler("json", mapping.WithDefault())
		var v struct {
			X int `json:"x,default=7"`
		}
		err := u.Unmarshal(map[string]any{}, &v)
		fwOK("mapping.WithDefault err", err)
		fwOK("mapping.WithDefault X", v.X)
	}

	// WithStringValues: coerce string "3" into int field.
	{
		u := mapping.NewUnmarshaler("json", mapping.WithStringValues())
		var v struct {
			N int `json:"n"`
		}
		err := u.Unmarshal(map[string]any{"n": "3"}, &v)
		fwOK("mapping.WithStringValues err", err)
		fwOK("mapping.WithStringValues N", v.N)
	}

	// WithFromArray: convert single-element array into a non-array field.
	{
		u := mapping.NewUnmarshaler("json", mapping.WithFromArray())
		var v struct {
			S string `json:"s"`
		}
		err := u.Unmarshal(map[string]any{"s": []any{"hello"}}, &v)
		fwOK("mapping.WithFromArray err", err)
		fwOK("mapping.WithFromArray S", v.S)
	}

	// WithCanonicalKeyFunc: map source keys via a canonicalizer (upper-case here).
	{
		u := mapping.NewUnmarshaler("json", mapping.WithCanonicalKeyFunc(strings.ToUpper))
		var v struct {
			Name string `json:"NAME"`
		}
		err := u.Unmarshal(map[string]any{"name": "z"}, &v)
		fwOK("mapping.WithCanonicalKeyFunc err", err)
		fwOK("mapping.WithCanonicalKeyFunc Name", v.Name)
	}

	// WithOpaqueKeys: keys with dots are treated as literal (no nesting).
	{
		u := mapping.NewUnmarshaler("json", mapping.WithOpaqueKeys())
		var v struct {
			Dotted string `json:"a.b"`
		}
		err := u.Unmarshal(map[string]any{"a.b": "literal"}, &v)
		fwOK("mapping.WithOpaqueKeys err", err)
		fwOK("mapping.WithOpaqueKeys value", v.Dotted)
	}

	// UnmarshalYamlBytes
	{
		var v struct {
			Name string `json:"name"`
		}
		err := mapping.UnmarshalYamlBytes([]byte("name: a\n"), &v)
		fwOK("mapping.UnmarshalYamlBytes err", err)
		fwOK("mapping.UnmarshalYamlBytes Name", v.Name)
	}

	// UnmarshalTomlBytes
	{
		var v struct {
			Name string `json:"name"`
		}
		err := mapping.UnmarshalTomlBytes([]byte("name = \"a\"\n"), &v)
		fwOK("mapping.UnmarshalTomlBytes err", err)
		fwOK("mapping.UnmarshalTomlBytes Name", v.Name)
	}

	// Repr: canonical string forms.
	fwOK("mapping.Repr int", mapping.Repr(123))
	fwOK("mapping.Repr string", mapping.Repr("abc"))
	fwOK("mapping.Repr bool", mapping.Repr(true))
	fwOK("mapping.Repr float", mapping.Repr(1.5))
}
