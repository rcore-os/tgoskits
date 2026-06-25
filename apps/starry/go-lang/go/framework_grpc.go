// Industrial-grade, deterministic gRPC carpet for the Go language carpet suite.
//
// Framework: google.golang.org/grpc v1.81.1 (+ google.golang.org/protobuf v1.36.11,
//
//	google.golang.org/genproto/googleapis/rpc errdetails).
//
// Harness:   google.golang.org/grpc/test/bufconn in-memory listener — NO real network,
//
//	NO OS socket/port, fully in-process and deterministic.
//
// No protoc / no .proto codegen: hand-written request/response are plain Go structs,
// serialized by a custom JSON-based encoding.Codec (registered as "fwjson") so the
// transport carries them without proto messages. ServiceDesc/MethodDesc/StreamDesc are
// hand-built, exactly mirroring what generated *_grpc.pb.go would emit. Health and
// reflection use the library's real proto-backed services with the default proto codec.
//
// Output: one line per assertion `ok: <label> = <value>`, then `GRPC_COUNT=<n>`.
// Determinism: no timestamps/addresses/random/map-iteration order leaks into output;
// all collections sorted; deadlines reported only as bool presence within a window.
package main

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"sort"
	"sync"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/connectivity"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/encoding"
	grpcgzip "google.golang.org/grpc/encoding/gzip"
	"google.golang.org/grpc/health"
	healthpb "google.golang.org/grpc/health/grpc_health_v1"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/reflection"
	rpb "google.golang.org/grpc/reflection/grpc_reflection_v1"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"

	"google.golang.org/genproto/googleapis/rpc/errdetails"
)

// ----------------------------------------------------------------------------
// Assertion counter
// ----------------------------------------------------------------------------

// ----------------------------------------------------------------------------
// Custom JSON codec (replaces proto codec so we can use plain Go structs)
// ----------------------------------------------------------------------------

type grpc_jsonCodec struct{}

func (grpc_jsonCodec) Marshal(v any) ([]byte, error)      { return json.Marshal(v) }
func (grpc_jsonCodec) Unmarshal(data []byte, v any) error { return json.Unmarshal(data, v) }
func (grpc_jsonCodec) Name() string                       { return "fwjson" }

// ----------------------------------------------------------------------------
// Hand-written message shapes (would be the *.pb.go structs under protoc)
// ----------------------------------------------------------------------------

type grpc_EchoReq struct {
	Msg string `json:"msg"`
	N   int    `json:"n"`
}
type grpc_EchoResp struct {
	Msg string `json:"msg"`
	N   int    `json:"n"`
}

// ----------------------------------------------------------------------------
// Hand-written ServiceDesc/MethodDesc/StreamDesc (the manual codegen path)
//
// Service "echo.Echo" with:
//   UnaryEcho      (unary)
//   ServerStream   (server-streaming)
//   ClientStream   (client-streaming)
//   BidiChat       (bidi-streaming)
// ----------------------------------------------------------------------------

// grpc_EchoServerIface is the service handler interface. RegisterService requires
// ServiceDesc.HandlerType to be a *pointer to an interface*; it reflect-checks
// that the impl satisfies it (this mirrors the generated XServer interface).
type grpc_EchoServerIface interface {
	UnaryEcho(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error)
	ServerStream(req *grpc_EchoReq, stream grpc.ServerStream) error
	ClientStream(stream grpc.ServerStream) error
	BidiChat(stream grpc.ServerStream) error
}

// grpc_echoServer is the server-side implementation type; *grpc_echoServer satisfies
// grpc_EchoServerIface, so RegisterService accepts it.
type grpc_echoServer struct {
	// hooks let individual tests change behavior deterministically.
	unaryHook func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error)
}

// Interface methods so *grpc_echoServer implements grpc_EchoServerIface. The actual RPC
// dispatch happens through the handlers below (which type-assert *grpc_echoServer),
// exactly as generated code routes via _Echo_*_Handler.
func (s *grpc_echoServer) UnaryEcho(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
	return s.unaryEcho(ctx, req)
}
func (s *grpc_echoServer) ServerStream(req *grpc_EchoReq, stream grpc.ServerStream) error {
	return grpc_serverStreamHandler(s, stream)
}
func (s *grpc_echoServer) ClientStream(stream grpc.ServerStream) error {
	return grpc_clientStreamHandler(s, stream)
}
func (s *grpc_echoServer) BidiChat(stream grpc.ServerStream) error {
	return grpc_bidiHandler(s, stream)
}

func (s *grpc_echoServer) unaryEcho(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
	if s.unaryHook != nil {
		return s.unaryHook(ctx, req)
	}
	if req.Msg == "" {
		return nil, status.Error(codes.InvalidArgument, "empty msg")
	}
	return &grpc_EchoResp{Msg: req.Msg, N: req.N}, nil
}

// grpc_unaryHandler mirrors generated unary handler wiring.
func grpc_unaryHandler(srv any, ctx context.Context, dec func(any) error, interceptor grpc.UnaryServerInterceptor) (any, error) {
	in := new(grpc_EchoReq)
	if err := dec(in); err != nil {
		return nil, err
	}
	if interceptor == nil {
		return srv.(*grpc_echoServer).unaryEcho(ctx, in)
	}
	info := &grpc.UnaryServerInfo{Server: srv, FullMethod: "/echo.Echo/UnaryEcho"}
	handler := func(ctx context.Context, req any) (any, error) {
		return srv.(*grpc_echoServer).unaryEcho(ctx, req.(*grpc_EchoReq))
	}
	return interceptor(ctx, in, info, handler)
}

// server-streaming: emit req.N responses, then set a header+trailer.
func grpc_serverStreamHandler(srv any, stream grpc.ServerStream) error {
	in := new(grpc_EchoReq)
	if err := stream.RecvMsg(in); err != nil {
		return err
	}
	// header/trailer via the ServerStream methods (not the free funcs).
	_ = stream.SetHeader(metadata.Pairs("x-ss-hdr", "h"))
	stream.SetTrailer(metadata.Pairs("x-ss-trl", "t"))
	ss := &grpc.GenericServerStream[grpc_EchoReq, grpc_EchoResp]{ServerStream: stream}
	for i := 0; i < in.N; i++ {
		select {
		case <-stream.Context().Done():
			return status.FromContextError(stream.Context().Err()).Err()
		default:
		}
		if err := ss.Send(&grpc_EchoResp{Msg: in.Msg, N: i}); err != nil {
			return err
		}
	}
	return nil
}

// client-streaming: sum all req.N, SendAndClose one response.
func grpc_clientStreamHandler(srv any, stream grpc.ServerStream) error {
	ss := &grpc.GenericServerStream[grpc_EchoReq, grpc_EchoResp]{ServerStream: stream}
	sum := 0
	for {
		req, err := ss.Recv()
		if err == io.EOF {
			return ss.SendAndClose(&grpc_EchoResp{Msg: "sum", N: sum})
		}
		if err != nil {
			return err
		}
		sum += req.N
	}
}

// bidi: echo each message back with N negated, until client CloseSend.
func grpc_bidiHandler(srv any, stream grpc.ServerStream) error {
	ss := &grpc.GenericServerStream[grpc_EchoReq, grpc_EchoResp]{ServerStream: stream}
	for {
		req, err := ss.Recv()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		if err := ss.Send(&grpc_EchoResp{Msg: req.Msg, N: -req.N}); err != nil {
			return err
		}
	}
}

func grpc_echoServiceDesc(impl *grpc_echoServer) *grpc.ServiceDesc {
	return &grpc.ServiceDesc{
		ServiceName: "echo.Echo",
		HandlerType: (*grpc_EchoServerIface)(nil),
		Methods: []grpc.MethodDesc{
			{MethodName: "UnaryEcho", Handler: grpc_unaryHandler},
		},
		Streams: []grpc.StreamDesc{
			{StreamName: "ServerStream", Handler: grpc_serverStreamHandler, ServerStreams: true},
			{StreamName: "ClientStream", Handler: grpc_clientStreamHandler, ClientStreams: true},
			{StreamName: "BidiChat", Handler: grpc_bidiHandler, ServerStreams: true, ClientStreams: true},
		},
		Metadata: "echo.proto",
	}
}

// ----------------------------------------------------------------------------
// Interceptor grpc_capture state (deterministic single-shot via mutex-guarded vars)
// ----------------------------------------------------------------------------

type grpc_capture struct {
	mu                sync.Mutex
	unarySrvMethod    string
	unarySrvCount     int
	streamSrvMethod   string
	streamSrvIsClient bool
	streamSrvIsServer bool
	unaryCliMethod    string
	streamCliMethod   string
	cliWrapSendCount  int
	cliWrapRecvCount  int
}

var grpc_cp = &grpc_capture{}

// ----------------------------------------------------------------------------
// Helpers to spin up a server + client over bufconn
// ----------------------------------------------------------------------------

const grpc_bufSize = 1024 * 1024

type grpc_harness struct {
	lis    *bufconn.Listener
	srv    *grpc.Server
	conn   *grpc.ClientConn
	serveC chan error
}

func grpc_newHarness(srvOpts []grpc.ServerOption, dialOpts []grpc.DialOption, register func(s *grpc.Server)) (*grpc_harness, error) {
	lis := bufconn.Listen(grpc_bufSize)
	srv := grpc.NewServer(srvOpts...)
	register(srv)
	serveC := make(chan error, 1)
	go func() { serveC <- srv.Serve(lis) }()

	opts := append([]grpc.DialOption{
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
			return lis.DialContext(ctx)
		}),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	}, dialOpts...)
	conn, err := grpc.NewClient("passthrough:///bufnet", opts...)
	if err != nil {
		srv.Stop()
		return nil, err
	}
	return &grpc_harness{lis: lis, srv: srv, conn: conn, serveC: serveC}, nil
}

func (h *grpc_harness) close() {
	h.conn.Close()
	h.srv.Stop()
	<-h.serveC
}

// grpc_forceJSON forces the custom codec on a call (so default proto codec is bypassed).
func grpc_forceJSON() grpc.CallOption { return grpc.ForceCodec(grpc_jsonCodec{}) }

func grpc_bg() context.Context { return context.Background() }

// ----------------------------------------------------------------------------
// main
// ----------------------------------------------------------------------------

func runFrameworkGRPC() {
	// Register the JSON codec once, globally (encoding.RegisterCodec).
	encoding.RegisterCodec(grpc_jsonCodec{})

	cat0_bufconn()
	cat1_handwritten_shape()
	cat2_unary()
	cat3_server_stream()
	cat4_client_stream()
	cat5_bidi()
	cat6_metadata()
	cat7_status_codes()
	cat8_error_details()
	cat9_interceptors()
	cat10_deadline_cancel()
	cat_msgsize_limit()
	cat_compression()
	cat_calloptions_peer()
	cat_health()
	cat_reflection()
	cat_connectivity()
	cat_graceful_vs_stop()
	cat_transport_stream()

}

// ---------------------------------------------------------------------------
// 0. bufconn in-process grpc_harness
// ---------------------------------------------------------------------------

func cat0_bufconn() {
	lis := bufconn.Listen(grpc_bufSize)
	fwOK("bufconn.Listen non-nil", lis != nil)
	fwOK("bufconn Addr().String()", lis.Addr().String())
	fwOK("bufconn Addr().Network()", lis.Addr().Network())

	// Dial/Accept pairing: bytes written one side appear on the other.
	accepted := make(chan net.Conn, 1)
	go func() {
		c, err := lis.Accept()
		if err == nil {
			accepted <- c
		} else {
			accepted <- nil
		}
	}()
	cConn, derr := lis.DialContext(grpc_bg())
	fwOK("bufconn DialContext err==nil", derr == nil)
	sConn := <-accepted
	fwOK("bufconn Accept paired non-nil", sConn != nil)
	if cConn != nil && sConn != nil {
		_, _ = cConn.Write([]byte("ping"))
		buf := make([]byte, 4)
		_, rerr := io.ReadFull(sConn, buf)
		fwOK("bufconn in-mem byte transfer err==nil", rerr == nil)
		fwOK("bufconn in-mem byte transfer payload", string(buf))
		cConn.Close()
		sConn.Close()
	}
	lis.Close()

	// NewClient + WithContextDialer + insecure creds over bufconn.
	srvlis := bufconn.Listen(grpc_bufSize)
	s := grpc.NewServer()
	go func() { _ = s.Serve(srvlis) }()
	conn, err := grpc.NewClient("passthrough:///bufnet",
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
			return srvlis.DialContext(ctx)
		}),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	fwOK("grpc.NewClient err==nil", err == nil)
	fwOK("ClientConn non-nil", conn != nil)
	fwOK("ClientConn.Target()", conn.Target())
	fwOK("insecure.NewCredentials non-nil", insecure.NewCredentials() != nil)

	// Close semantics: Close returns nil; subsequent RPC fails Canceled.
	cerr := conn.Close()
	fwOK("ClientConn.Close() err==nil", cerr == nil)
	var reply grpc_EchoResp
	ierr := conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "x"}, &reply, grpc_forceJSON())
	fwOK("RPC after Close code==Canceled", status.Code(ierr) == codes.Canceled)

	s.Stop()
}

// ---------------------------------------------------------------------------
// 1. Hand-written service shape: ServiceDesc / RegisterService / GetServiceInfo
// ---------------------------------------------------------------------------

func cat1_handwritten_shape() {
	impl := &grpc_echoServer{}
	desc := grpc_echoServiceDesc(impl)
	fwOK("ServiceDesc.ServiceName", desc.ServiceName)
	fwOK("ServiceDesc Methods len", len(desc.Methods))
	fwOK("ServiceDesc Streams len", len(desc.Streams))
	fwOK("ServiceDesc Metadata", desc.Metadata)
	fwOK("MethodDesc[0].MethodName", desc.Methods[0].MethodName)

	// StreamDesc booleans per kind.
	for _, sd := range desc.Streams {
		switch sd.StreamName {
		case "ServerStream":
			fwOK("StreamDesc ServerStream ServerStreams", sd.ServerStreams)
			fwOK("StreamDesc ServerStream ClientStreams", sd.ClientStreams)
		case "ClientStream":
			fwOK("StreamDesc ClientStream ServerStreams", sd.ServerStreams)
			fwOK("StreamDesc ClientStream ClientStreams", sd.ClientStreams)
		case "BidiChat":
			fwOK("StreamDesc BidiChat ServerStreams", sd.ServerStreams)
			fwOK("StreamDesc BidiChat ClientStreams", sd.ClientStreams)
		}
	}

	// RegisterService succeeds + GetServiceInfo exposes the service & methods.
	s := grpc.NewServer()
	s.RegisterService(desc, impl)
	info := s.GetServiceInfo()
	_, present := info["echo.Echo"]
	fwOK("GetServiceInfo has echo.Echo", present)
	// method names sorted for deterministic output
	var methods []string
	for _, m := range info["echo.Echo"].Methods {
		methods = append(methods, m.Name)
	}
	sort.Strings(methods)
	fwOK("GetServiceInfo echo.Echo methods", methods)
	s.Stop()
}

// ---------------------------------------------------------------------------
// 2. Unary RPC (Invoke + UnaryHandler + status error)
// ---------------------------------------------------------------------------

func cat2_unary() {
	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat2 grpc_harness err", err)
		return
	}
	defer h.close()

	// Happy path via Invoke (what a generated stub calls internally).
	var resp grpc_EchoResp
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "ping", N: 7}, &resp, grpc_forceJSON())
	fwOK("unary Invoke err==nil", ierr == nil)
	fwOK("unary echoed Msg", resp.Msg)
	fwOK("unary echoed N", resp.N)

	// UnaryHandler signature: directly invoke the inner handler returns typed resp.
	var uh grpc.UnaryHandler = func(ctx context.Context, req any) (any, error) {
		return impl.unaryEcho(ctx, req.(*grpc_EchoReq))
	}
	hr, herr := uh(grpc_bg(), &grpc_EchoReq{Msg: "direct", N: 1})
	fwOK("UnaryHandler direct err==nil", herr == nil)
	fwOK("UnaryHandler direct resp Msg", hr.(*grpc_EchoResp).Msg)

	// status.Error from server -> InvalidArgument on empty msg.
	var resp2 grpc_EchoResp
	eerr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: ""}, &resp2, grpc_forceJSON())
	st, _ := status.FromError(eerr)
	fwOK("unary empty-msg code", st.Code().String())
	fwOK("unary empty-msg message", st.Message())
}

// ---------------------------------------------------------------------------
// 3. Server-streaming RPC
// ---------------------------------------------------------------------------

func cat3_server_stream() {
	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat3 grpc_harness err", err)
		return
	}
	defer h.close()

	desc := &grpc.StreamDesc{StreamName: "ServerStream", ServerStreams: true}
	cs, serr := h.conn.NewStream(grpc_bg(), desc, "/echo.Echo/ServerStream", grpc_forceJSON())
	fwOK("server-stream NewStream err==nil", serr == nil)
	if serr != nil {
		return
	}
	if e := cs.SendMsg(&grpc_EchoReq{Msg: "s", N: 3}); e != nil {
		fwOK("server-stream SendMsg err", e)
		return
	}
	_ = cs.CloseSend()

	gcs := &grpc.GenericClientStream[grpc_EchoReq, grpc_EchoResp]{ClientStream: cs}
	var got []int
	var recvErr error
	for {
		r, e := gcs.Recv()
		if e == io.EOF {
			recvErr = e
			break
		}
		if e != nil {
			recvErr = e
			break
		}
		got = append(got, r.N)
	}
	fwOK("server-stream received count", len(got))
	fwOK("server-stream received seq", got)
	fwOK("server-stream terminates io.EOF", recvErr == io.EOF)

	// Header set by the server-streaming handler is observable.
	hdr, _ := cs.Header()
	fwOK("server-stream header x-ss-hdr", hdr.Get("x-ss-hdr"))
	trl := cs.Trailer()
	fwOK("server-stream trailer x-ss-trl", trl.Get("x-ss-trl"))
}

// ---------------------------------------------------------------------------
// 4. Client-streaming RPC
// ---------------------------------------------------------------------------

func cat4_client_stream() {
	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat4 grpc_harness err", err)
		return
	}
	defer h.close()

	desc := &grpc.StreamDesc{StreamName: "ClientStream", ClientStreams: true}
	cs, serr := h.conn.NewStream(grpc_bg(), desc, "/echo.Echo/ClientStream", grpc_forceJSON())
	fwOK("client-stream NewStream err==nil", serr == nil)
	if serr != nil {
		return
	}
	gcs := &grpc.GenericClientStream[grpc_EchoReq, grpc_EchoResp]{ClientStream: cs}
	sendErrs := 0
	for _, n := range []int{1, 2, 3, 4} {
		if e := gcs.Send(&grpc_EchoReq{Msg: "c", N: n}); e != nil {
			sendErrs++
		}
	}
	fwOK("client-stream all Send err==nil", sendErrs == 0)
	resp, cerr := gcs.CloseAndRecv()
	fwOK("client-stream CloseAndRecv err==nil", cerr == nil)
	if cerr == nil {
		fwOK("client-stream aggregated sum", resp.N)
		fwOK("client-stream resp Msg", resp.Msg)
	}
}

// ---------------------------------------------------------------------------
// 5. Bidirectional-streaming RPC
// ---------------------------------------------------------------------------

func cat5_bidi() {
	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat5 grpc_harness err", err)
		return
	}
	defer h.close()

	desc := &grpc.StreamDesc{StreamName: "BidiChat", ServerStreams: true, ClientStreams: true}
	cs, serr := h.conn.NewStream(grpc_bg(), desc, "/echo.Echo/BidiChat", grpc_forceJSON())
	fwOK("bidi NewStream err==nil", serr == nil)
	if serr != nil {
		return
	}
	gcs := &grpc.GenericClientStream[grpc_EchoReq, grpc_EchoResp]{ClientStream: cs}

	// Recv in a goroutine, Send in main; safe (single Send goroutine, single Recv goroutine).
	recvDone := make(chan struct{})
	var recvN []int
	var recvEOF bool
	go func() {
		for {
			r, e := gcs.Recv()
			if e == io.EOF {
				recvEOF = true
				break
			}
			if e != nil {
				break
			}
			recvN = append(recvN, r.N)
		}
		close(recvDone)
	}()
	for _, n := range []int{10, 20, 30} {
		_ = gcs.Send(&grpc_EchoReq{Msg: "b", N: n})
	}
	closeErr := gcs.CloseSend()
	fwOK("bidi CloseSend err==nil", closeErr == nil)
	<-recvDone
	sort.Ints(recvN)
	fwOK("bidi echoed (negated, sorted)", recvN)
	fwOK("bidi terminates io.EOF", recvEOF)
}

// ---------------------------------------------------------------------------
// 6. Metadata: MD methods, outgoing->incoming, header, trailer
// ---------------------------------------------------------------------------

func cat6_metadata() {
	// MD constructors + methods (pure, no RPC).
	md := metadata.Pairs("k", "v1", "k", "v2")
	fwOK("metadata.Pairs Get(k)", md.Get("k"))
	fwOK("metadata.Pairs Len", md.Len())
	mdU := metadata.Pairs("K", "X") // uppercase key normalized to lowercase
	fwOK("metadata uppercase key normalized", mdU.Get("k"))
	mdN := metadata.New(map[string]string{"a": "1", "b": "2"})
	fwOK("metadata.New Get(a)", mdN.Get("a"))
	mdC := mdN.Copy()
	mdC.Set("a", "z")
	fwOK("metadata Copy independence (orig)", mdN.Get("a"))
	fwOK("metadata Copy independence (copy)", mdC.Get("a"))
	mdA := metadata.MD{}
	mdA.Append("x", "1")
	mdA.Append("x", "2")
	fwOK("metadata Append", mdA.Get("x"))
	mdA.Delete("x")
	fwOK("metadata Delete -> empty", len(mdA.Get("x")) == 0)
	j := metadata.Join(metadata.Pairs("p", "1"), metadata.Pairs("p", "2", "q", "9"))
	fwOK("metadata.Join p", j.Get("p"))
	fwOK("metadata.Join q", j.Get("q"))

	// RPC: outgoing metadata appears in server incoming; server header & trailer
	// flow back to client.
	var srvAuth []string
	var srvOK bool
	impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		in, ok2 := metadata.FromIncomingContext(ctx)
		srvOK = ok2
		if ok2 {
			srvAuth = in.Get("authorization")
		}
		_ = grpc.SetHeader(ctx, metadata.Pairs("x-srv", "1"))
		_ = grpc.SetTrailer(ctx, metadata.Pairs("x-trl", "done"))
		return &grpc_EchoResp{Msg: req.Msg, N: req.N}, nil
	}}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat6 grpc_harness err", err)
		return
	}
	defer h.close()

	ctx := metadata.AppendToOutgoingContext(grpc_bg(), "authorization", "token123")
	var resp grpc_EchoResp
	var hdr, tlr metadata.MD
	ierr := h.conn.Invoke(ctx, "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "m", N: 1}, &resp,
		grpc_forceJSON(), grpc.Header(&hdr), grpc.Trailer(&tlr))
	fwOK("metadata RPC err==nil", ierr == nil)
	fwOK("metadata FromIncomingContext ok", srvOK)
	fwOK("metadata server saw authorization", srvAuth)
	fwOK("metadata client header x-srv", hdr.Get("x-srv"))
	fwOK("metadata client trailer x-trl", tlr.Get("x-trl"))
}

// ---------------------------------------------------------------------------
// 7. Status + codes (all 17 codes, String round-trip, FromError branches)
// ---------------------------------------------------------------------------

func cat7_status_codes() {
	// All 17 valid codes + their String() round-trip, deterministic order.
	all := []struct {
		c codes.Code
		s string
	}{
		{codes.OK, "OK"},
		{codes.Canceled, "Canceled"},
		{codes.Unknown, "Unknown"},
		{codes.InvalidArgument, "InvalidArgument"},
		{codes.DeadlineExceeded, "DeadlineExceeded"},
		{codes.NotFound, "NotFound"},
		{codes.AlreadyExists, "AlreadyExists"},
		{codes.PermissionDenied, "PermissionDenied"},
		{codes.ResourceExhausted, "ResourceExhausted"},
		{codes.FailedPrecondition, "FailedPrecondition"},
		{codes.Aborted, "Aborted"},
		{codes.OutOfRange, "OutOfRange"},
		{codes.Unimplemented, "Unimplemented"},
		{codes.Internal, "Internal"},
		{codes.Unavailable, "Unavailable"},
		{codes.DataLoss, "DataLoss"},
		{codes.Unauthenticated, "Unauthenticated"},
	}
	fwOK("codes total count", len(all))
	for _, e := range all {
		fwOK("codes.Code value "+e.s, uint32(e.c))
		fwOK("codes.Code String "+e.s, e.c.String() == e.s)
	}
	// Invalid code String() -> "Code(99)".
	fwOK("invalid code String", codes.Code(99).String())

	// status.New/Error/Errorf, Err/Code/Message.
	st := status.New(codes.NotFound, "missing")
	fwOK("status.New Code", st.Code().String())
	fwOK("status.New Message", st.Message())
	fwOK("status.New Err non-nil", st.Err() != nil)
	stf := status.Newf(codes.Aborted, "x=%d", 5)
	fwOK("status.Newf Message", stf.Message())
	ef := status.Errorf(codes.Internal, "boom %d", 9)
	fwOK("status.Errorf code", status.Code(ef).String())
	// OK must yield nil error.
	fwOK("status.Error(OK) is nil", status.Error(codes.OK, "ignored") == nil)
	fwOK("status.New(OK).Err() is nil", status.New(codes.OK, "x").Err() == nil)

	// FromError three branches.
	gerr := status.Error(codes.PermissionDenied, "no")
	sa, oka := status.FromError(gerr)
	fwOK("FromError(grpc) ok", oka)
	fwOK("FromError(grpc) code", sa.Code().String())
	sb, okb := status.FromError(nil)
	fwOK("FromError(nil) ok", okb)
	fwOK("FromError(nil) code", sb.Code().String())
	plain := errors.New("plain failure")
	sc, okc := status.FromError(plain)
	fwOK("FromError(plain) ok==false", okc == false)
	fwOK("FromError(plain) code", sc.Code().String())
	fwOK("FromError(plain) message", sc.Message())

	// Convert + Code helpers.
	fwOK("status.Convert(grpc) code", status.Convert(gerr).Code().String())
	fwOK("status.Code(nil)", status.Code(nil).String())
	fwOK("status.Code(plain)", status.Code(plain).String())

	// FromContextError mapping.
	fwOK("FromContextError(DeadlineExceeded)", status.FromContextError(context.DeadlineExceeded).Code().String())
	fwOK("FromContextError(Canceled)", status.FromContextError(context.Canceled).Code().String())
	fwOK("FromContextError(nil)", status.FromContextError(nil).Code().String())

	// FromProto / Proto round-trip.
	pst := status.New(codes.OutOfRange, "rng").Proto()
	fwOK("Status.Proto code field", pst.GetCode())
	fwOK("Status.Proto message field", pst.GetMessage())
	rebuilt := status.FromProto(pst)
	fwOK("status.FromProto code", rebuilt.Code().String())
	fwOK("status.FromProto message", rebuilt.Message())

	// Server returns each code; client reads it back over bufconn.
	codesToTest := []codes.Code{
		codes.NotFound, codes.AlreadyExists, codes.PermissionDenied,
		codes.FailedPrecondition, codes.Aborted, codes.OutOfRange,
		codes.Unimplemented, codes.Internal, codes.Unavailable,
		codes.DataLoss, codes.Unauthenticated, codes.ResourceExhausted,
	}
	for _, c := range codesToTest {
		c := c
		impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
			return nil, status.Error(c, "code:"+c.String())
		}}
		h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
			s.RegisterService(grpc_echoServiceDesc(impl), impl)
		})
		if err != nil {
			fwOK("cat7 wire grpc_harness err "+c.String(), err)
			continue
		}
		var resp grpc_EchoResp
		ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "x"}, &resp, grpc_forceJSON())
		fwOK("wire status code "+c.String(), status.Code(ierr) == c)
		h.close()
	}
}

// ---------------------------------------------------------------------------
// 8. Error details (WithDetails / Details + genproto errdetails)
// ---------------------------------------------------------------------------

func cat8_error_details() {
	impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		st, derr := status.New(codes.InvalidArgument, "bad").WithDetails(
			&errdetails.BadRequest{FieldViolations: []*errdetails.BadRequest_FieldViolation{
				{Field: "name", Description: "required"},
			}},
			&errdetails.ErrorInfo{Reason: "OUT_OF_STOCK", Domain: "shop",
				Metadata: map[string]string{"sku": "42"}},
		)
		if derr != nil {
			return nil, status.Error(codes.Internal, "withdetails failed")
		}
		return nil, st.Err()
	}}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat8 grpc_harness err", err)
		return
	}
	defer h.close()

	var resp grpc_EchoResp
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "x"}, &resp, grpc_forceJSON())
	st := status.Convert(ierr)
	fwOK("details status code", st.Code().String())
	fwOK("details count", len(st.Details()))
	var sawBadReq, sawErrInfo bool
	var field, reason, domain, sku string
	for _, d := range st.Details() {
		switch v := d.(type) {
		case *errdetails.BadRequest:
			sawBadReq = true
			if len(v.GetFieldViolations()) > 0 {
				field = v.GetFieldViolations()[0].GetField()
			}
		case *errdetails.ErrorInfo:
			sawErrInfo = true
			reason = v.GetReason()
			domain = v.GetDomain()
			sku = v.GetMetadata()["sku"]
		}
	}
	fwOK("details has BadRequest", sawBadReq)
	fwOK("details BadRequest field", field)
	fwOK("details has ErrorInfo", sawErrInfo)
	fwOK("details ErrorInfo reason", reason)
	fwOK("details ErrorInfo domain", domain)
	fwOK("details ErrorInfo metadata sku", sku)
}

// ---------------------------------------------------------------------------
// 9. Interceptors: unary+stream, server+client (chains too)
// ---------------------------------------------------------------------------

// grpc_wrappedClientStream counts SendMsg/RecvMsg for the client stream interceptor.
type grpc_wrappedClientStream struct {
	grpc.ClientStream
}

func (w *grpc_wrappedClientStream) SendMsg(m any) error {
	grpc_cp.mu.Lock()
	grpc_cp.cliWrapSendCount++
	grpc_cp.mu.Unlock()
	return w.ClientStream.SendMsg(m)
}
func (w *grpc_wrappedClientStream) RecvMsg(m any) error {
	err := w.ClientStream.RecvMsg(m)
	if err == nil {
		grpc_cp.mu.Lock()
		grpc_cp.cliWrapRecvCount++
		grpc_cp.mu.Unlock()
	}
	return err
}

func cat9_interceptors() {
	grpc_cp.mu.Lock()
	grpc_cp.unarySrvMethod = ""
	grpc_cp.unarySrvCount = 0
	grpc_cp.streamSrvMethod = ""
	grpc_cp.streamSrvIsClient = false
	grpc_cp.streamSrvIsServer = false
	grpc_cp.unaryCliMethod = ""
	grpc_cp.streamCliMethod = ""
	grpc_cp.cliWrapSendCount = 0
	grpc_cp.cliWrapRecvCount = 0
	grpc_cp.mu.Unlock()

	var chainOrder []string

	srvUnary := grpc.ChainUnaryInterceptor(
		func(ctx context.Context, req any, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (any, error) {
			grpc_cp.mu.Lock()
			grpc_cp.unarySrvMethod = info.FullMethod
			grpc_cp.unarySrvCount++
			chainOrder = append(chainOrder, "outer")
			grpc_cp.mu.Unlock()
			return handler(ctx, req)
		},
		func(ctx context.Context, req any, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (any, error) {
			grpc_cp.mu.Lock()
			chainOrder = append(chainOrder, "inner")
			grpc_cp.mu.Unlock()
			return handler(ctx, req)
		},
	)
	srvStream := grpc.StreamInterceptor(
		func(srv any, ss grpc.ServerStream, info *grpc.StreamServerInfo, handler grpc.StreamHandler) error {
			grpc_cp.mu.Lock()
			grpc_cp.streamSrvMethod = info.FullMethod
			grpc_cp.streamSrvIsClient = info.IsClientStream
			grpc_cp.streamSrvIsServer = info.IsServerStream
			grpc_cp.mu.Unlock()
			return handler(srv, ss)
		},
	)

	cliUnary := grpc.WithChainUnaryInterceptor(
		func(ctx context.Context, method string, req, reply any, cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
			grpc_cp.mu.Lock()
			grpc_cp.unaryCliMethod = method
			grpc_cp.mu.Unlock()
			ctx = metadata.AppendToOutgoingContext(ctx, "x-from-client-interceptor", "1")
			return invoker(ctx, method, req, reply, cc, opts...)
		},
	)
	cliStream := grpc.WithStreamInterceptor(
		func(ctx context.Context, desc *grpc.StreamDesc, cc *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
			grpc_cp.mu.Lock()
			grpc_cp.streamCliMethod = method
			grpc_cp.mu.Unlock()
			cs, err := streamer(ctx, desc, cc, method, opts...)
			if err != nil {
				return nil, err
			}
			return &grpc_wrappedClientStream{ClientStream: cs}, nil
		},
	)

	var srvSawInjected []string
	impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		if in, ok2 := metadata.FromIncomingContext(ctx); ok2 {
			srvSawInjected = in.Get("x-from-client-interceptor")
		}
		return &grpc_EchoResp{Msg: req.Msg, N: req.N}, nil
	}}

	h, err := grpc_newHarness(
		[]grpc.ServerOption{srvUnary, srvStream},
		[]grpc.DialOption{cliUnary, cliStream},
		func(s *grpc.Server) { s.RegisterService(grpc_echoServiceDesc(impl), impl) },
	)
	if err != nil {
		fwOK("cat9 grpc_harness err", err)
		return
	}
	defer h.close()

	// Unary RPC -> exercises both server chain + client unary interceptor.
	var resp grpc_EchoResp
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "i", N: 2}, &resp, grpc_forceJSON())
	fwOK("interceptor unary RPC err==nil", ierr == nil)
	fwOK("interceptor unary resp passthrough", resp.Msg)
	grpc_cp.mu.Lock()
	fwOK("server unary interceptor FullMethod", grpc_cp.unarySrvMethod)
	fwOK("server unary interceptor ran once", grpc_cp.unarySrvCount == 1)
	fwOK("server unary chain order", append([]string{}, chainOrder...))
	fwOK("client unary interceptor method", grpc_cp.unaryCliMethod)
	grpc_cp.mu.Unlock()
	fwOK("server saw client-injected md", srvSawInjected)

	// Streaming RPC -> exercises server stream interceptor info + client stream wrapper.
	desc := &grpc.StreamDesc{StreamName: "BidiChat", ServerStreams: true, ClientStreams: true}
	cs, serr := h.conn.NewStream(grpc_bg(), desc, "/echo.Echo/BidiChat", grpc_forceJSON())
	fwOK("interceptor stream NewStream err==nil", serr == nil)
	if serr == nil {
		gcs := &grpc.GenericClientStream[grpc_EchoReq, grpc_EchoResp]{ClientStream: cs}
		_ = gcs.Send(&grpc_EchoReq{Msg: "s", N: 5})
		_ = gcs.CloseSend()
		for {
			_, e := gcs.Recv()
			if e != nil {
				break
			}
		}
	}
	grpc_cp.mu.Lock()
	fwOK("server stream interceptor FullMethod", grpc_cp.streamSrvMethod)
	fwOK("server stream interceptor IsClientStream", grpc_cp.streamSrvIsClient)
	fwOK("server stream interceptor IsServerStream", grpc_cp.streamSrvIsServer)
	fwOK("client stream interceptor method", grpc_cp.streamCliMethod)
	fwOK("client stream wrapper SendMsg counted", grpc_cp.cliWrapSendCount >= 1)
	fwOK("client stream wrapper RecvMsg counted", grpc_cp.cliWrapRecvCount >= 1)
	grpc_cp.mu.Unlock()
}

// ---------------------------------------------------------------------------
// 10. Deadline / cancellation / deadline visibility
// ---------------------------------------------------------------------------

func cat10_deadline_cancel() {
	// (a) Deadline exceeded: handler waits on ctx.Done(); client deadline fires.
	implDL := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		select {
		case <-ctx.Done():
			return nil, status.FromContextError(ctx.Err()).Err()
		case <-time.After(5 * time.Second):
			return &grpc_EchoResp{Msg: "late"}, nil
		}
	}}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(implDL), implDL)
	})
	if err != nil {
		fwOK("cat10 grpc_harness err", err)
		return
	}
	defer h.close()

	ctx, cancel := context.WithTimeout(grpc_bg(), 80*time.Millisecond)
	defer cancel()
	var resp grpc_EchoResp
	ierr := h.conn.Invoke(ctx, "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "slow"}, &resp, grpc_forceJSON())
	fwOK("deadline RPC code==DeadlineExceeded", status.Code(ierr) == codes.DeadlineExceeded)

	// (b) Deadline visibility: server reads ctx.Deadline(); report only presence
	// + that it sits within (now, now+timeout+epsilon) — no numeric closeness.
	var sawDeadline bool
	var deadlineInWindow bool
	start := time.Now()
	const window = 2 * time.Second
	implVis := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		dl, has := ctx.Deadline()
		sawDeadline = has
		if has {
			deadlineInWindow = dl.After(start) && dl.Before(start.Add(window+time.Second))
		}
		return &grpc_EchoResp{Msg: "ok"}, nil
	}}
	h2, err2 := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(implVis), implVis)
	})
	if err2 != nil {
		fwOK("cat10b grpc_harness err", err2)
	} else {
		ctx2, cancel2 := context.WithTimeout(grpc_bg(), window)
		var r grpc_EchoResp
		_ = h2.conn.Invoke(ctx2, "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "v"}, &r, grpc_forceJSON())
		fwOK("deadline propagated (ctx.Deadline ok)", sawDeadline)
		fwOK("deadline within window", deadlineInWindow)
		cancel2()
		h2.close()
	}

	// (c) Cancellation of a server-streaming RPC mid-flight.
	implCancel := &grpc_echoServer{}
	h3, err3 := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(implCancel), implCancel)
	})
	if err3 != nil {
		fwOK("cat10c grpc_harness err", err3)
		return
	}
	defer h3.close()
	cctx, ccancel := context.WithCancel(grpc_bg())
	desc := &grpc.StreamDesc{StreamName: "ServerStream", ServerStreams: true}
	cs, serr := h3.conn.NewStream(cctx, desc, "/echo.Echo/ServerStream", grpc_forceJSON())
	fwOK("cancel stream NewStream err==nil", serr == nil)
	if serr == nil {
		_ = cs.SendMsg(&grpc_EchoReq{Msg: "c", N: 1000})
		_ = cs.CloseSend()
		gcs := &grpc.GenericClientStream[grpc_EchoReq, grpc_EchoResp]{ClientStream: cs}
		_, _ = gcs.Recv() // receive at least one, then cancel
		ccancel()
		var lastCode codes.Code
		for {
			_, e := gcs.Recv()
			if e == io.EOF {
				lastCode = codes.OK
				break
			}
			if e != nil {
				lastCode = status.Code(e)
				break
			}
		}
		fwOK("cancel stream final code==Canceled", lastCode == codes.Canceled)
	} else {
		ccancel()
	}
}

// ---------------------------------------------------------------------------
// msgsize-limit -> ResourceExhausted (server MaxRecvMsgSize + client recv limit)
// ---------------------------------------------------------------------------

func cat_msgsize_limit() {
	impl := &grpc_echoServer{}
	// Server caps receive at 16 bytes; client receive limit also tiny.
	h, err := grpc_newHarness(
		[]grpc.ServerOption{
			grpc.MaxRecvMsgSize(32),
			grpc.MaxSendMsgSize(32),
			grpc.MaxConcurrentStreams(8),
			grpc.InitialWindowSize(64 * 1024),
			grpc.ConnectionTimeout(5 * time.Second),
		},
		nil,
		func(s *grpc.Server) { s.RegisterService(grpc_echoServiceDesc(impl), impl) },
	)
	if err != nil {
		fwOK("cat_msgsize grpc_harness err", err)
		return
	}
	defer h.close()

	// Oversized request -> server rejects with ResourceExhausted.
	big := ""
	for i := 0; i < 200; i++ {
		big += "A"
	}
	var resp grpc_EchoResp
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: big, N: 1}, &resp, grpc_forceJSON())
	fwOK("oversized recv -> ResourceExhausted", status.Code(ierr) == codes.ResourceExhausted)

	// A small message succeeds under the same server.
	var resp2 grpc_EchoResp
	ierr2 := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "ok", N: 1}, &resp2, grpc_forceJSON())
	fwOK("small msg under limit err==nil", ierr2 == nil)

	// Client-side per-call recv limit (MaxCallRecvMsgSize tiny) -> ResourceExhausted.
	implBig := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		payload := ""
		for i := 0; i < 100; i++ {
			payload += "B"
		}
		return &grpc_EchoResp{Msg: payload, N: 1}, nil
	}}
	h2, err2 := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(implBig), implBig)
	})
	if err2 != nil {
		fwOK("cat_msgsize h2 err", err2)
		return
	}
	defer h2.close()
	var resp3 grpc_EchoResp
	ierr3 := h2.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "x"}, &resp3,
		grpc_forceJSON(), grpc.MaxCallRecvMsgSize(8))
	fwOK("client recv limit -> ResourceExhausted", status.Code(ierr3) == codes.ResourceExhausted)

	// WithDefaultCallOptions(MaxCallSendMsgSize) demonstrated on a fresh conn.
	conn2, derr := grpc.NewClient("passthrough:///bufnet",
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
			return h2.lis.DialContext(ctx)
		}),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithDefaultCallOptions(grpc.MaxCallSendMsgSize(8), grpc.MaxCallRecvMsgSize(8)),
	)
	fwOK("WithDefaultCallOptions NewClient err==nil", derr == nil)
	if derr == nil {
		bigReq := ""
		for i := 0; i < 100; i++ {
			bigReq += "C"
		}
		var r grpc_EchoResp
		serr := conn2.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: bigReq}, &r, grpc_forceJSON())
		fwOK("default send limit -> ResourceExhausted", status.Code(serr) == codes.ResourceExhausted)
		conn2.Close()
	}
}

// ---------------------------------------------------------------------------
// Compression: gzip compressor end-to-end + standalone gzip round-trip
// ---------------------------------------------------------------------------

func cat_compression() {
	// gzip is registered as an encoding.Compressor by importing the package
	// (blank-import side effect equivalent — we reference grpcgzip.Name).
	fwOK("gzip compressor name", grpcgzip.Name)
	fwOK("encoding.GetCompressor(gzip) non-nil", encoding.GetCompressor(grpcgzip.Name) != nil)

	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat_compression grpc_harness err", err)
		return
	}
	defer h.close()

	// UseCompressor(gzip) on the call: server auto-decompresses (gzip registered).
	var resp grpc_EchoResp
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "compressme", N: 9}, &resp,
		grpc_forceJSON(), grpc.UseCompressor(grpcgzip.Name))
	fwOK("gzip RPC err==nil", ierr == nil)
	fwOK("gzip RPC echoed Msg", resp.Msg)
	fwOK("gzip RPC echoed N", resp.N)

	// Standalone gzip compress->decompress determinism check.
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	_, _ = zw.Write([]byte("hello-gzip"))
	_ = zw.Close()
	zr, _ := gzip.NewReader(&buf)
	dec, _ := io.ReadAll(zr)
	_ = zr.Close()
	fwOK("gzip stdlib round-trip", string(dec))
}

// ---------------------------------------------------------------------------
// CallOptions family + peer.FromContext
// ---------------------------------------------------------------------------

func cat_calloptions_peer() {
	var srvPeerOK bool
	var srvPeerNet string
	impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		p, has := peer.FromContext(ctx)
		srvPeerOK = has
		if has && p.Addr != nil {
			srvPeerNet = p.Addr.Network()
		}
		return &grpc_EchoResp{Msg: req.Msg, N: req.N}, nil
	}}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat_callopt grpc_harness err", err)
		return
	}
	defer h.close()

	// grpc.Peer(*peer.Peer) CallOption populates client peer; WaitForReady;
	// CallContentSubtype forces our codec by name.
	var pr peer.Peer
	var resp grpc_EchoResp
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "p", N: 3}, &resp,
		grpc.CallContentSubtype("fwjson"),
		grpc.WaitForReady(true),
		grpc.Peer(&pr),
		grpc.MaxCallRecvMsgSize(1<<20),
		grpc.MaxCallSendMsgSize(1<<20),
	)
	fwOK("callopt RPC err==nil", ierr == nil)
	fwOK("CallContentSubtype echoed Msg", resp.Msg)
	fwOK("grpc.Peer client addr non-nil", pr.Addr != nil)
	fwOK("peer.FromContext server ok", srvPeerOK)
	fwOK("peer.FromContext server addr network", srvPeerNet)

	// WaitForReady(false) is also a valid CallOption value.
	var resp2 grpc_EchoResp
	ierr2 := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "q", N: 1}, &resp2,
		grpc_forceJSON(), grpc.WaitForReady(false))
	fwOK("WaitForReady(false) RPC err==nil", ierr2 == nil)
}

// ---------------------------------------------------------------------------
// Health checking service (Check + status transitions)
// ---------------------------------------------------------------------------

func cat_health() {
	hs := health.NewServer()
	hs.SetServingStatus("", healthpb.HealthCheckResponse_SERVING)
	hs.SetServingStatus("echo.Echo", healthpb.HealthCheckResponse_SERVING)

	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
		healthpb.RegisterHealthServer(s, hs) // grpc_health_v1.RegisterHealthServer
	})
	if err != nil {
		fwOK("cat_health grpc_harness err", err)
		return
	}
	defer h.close()

	hc := healthpb.NewHealthClient(h.conn)
	// Overall server health.
	r1, e1 := hc.Check(grpc_bg(), &healthpb.HealthCheckRequest{Service: ""})
	fwOK("health Check overall err==nil", e1 == nil)
	if e1 == nil {
		fwOK("health Check overall status", r1.GetStatus().String())
	}
	// Named service health.
	r2, e2 := hc.Check(grpc_bg(), &healthpb.HealthCheckRequest{Service: "echo.Echo"})
	fwOK("health Check named err==nil", e2 == nil)
	if e2 == nil {
		fwOK("health Check named status", r2.GetStatus().String())
	}
	// Unknown service -> NotFound.
	_, e3 := hc.Check(grpc_bg(), &healthpb.HealthCheckRequest{Service: "does.not.exist"})
	fwOK("health Check unknown -> NotFound", status.Code(e3) == codes.NotFound)

	// Flip to NOT_SERVING and re-check.
	hs.SetServingStatus("echo.Echo", healthpb.HealthCheckResponse_NOT_SERVING)
	r4, e4 := hc.Check(grpc_bg(), &healthpb.HealthCheckRequest{Service: "echo.Echo"})
	fwOK("health Check after flip err==nil", e4 == nil)
	if e4 == nil {
		fwOK("health Check after flip status", r4.GetStatus().String())
	}
}

// ---------------------------------------------------------------------------
// Reflection registration + ListServices over bufconn
// ---------------------------------------------------------------------------

func cat_reflection() {
	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
		hs := health.NewServer()
		healthpb.RegisterHealthServer(s, hs)
		reflection.Register(s) // registers v1 + v1alpha reflection services
	})
	if err != nil {
		fwOK("cat_reflection grpc_harness err", err)
		return
	}
	defer h.close()

	rc := rpb.NewServerReflectionClient(h.conn)
	stream, serr := rc.ServerReflectionInfo(grpc_bg())
	fwOK("reflection stream open err==nil", serr == nil)
	if serr != nil {
		return
	}
	if e := stream.Send(&rpb.ServerReflectionRequest{
		MessageRequest: &rpb.ServerReflectionRequest_ListServices{ListServices: "*"},
	}); e != nil {
		fwOK("reflection Send err", e)
		return
	}
	resp, rerr := stream.Recv()
	fwOK("reflection Recv err==nil", rerr == nil)
	_ = stream.CloseSend()
	if rerr != nil {
		return
	}
	lsr := resp.GetListServicesResponse()
	fwOK("reflection ListServicesResponse non-nil", lsr != nil)
	if lsr != nil {
		var svcs []string
		for _, s := range lsr.GetService() {
			svcs = append(svcs, s.GetName())
		}
		sort.Strings(svcs)
		fwOK("reflection service count", len(svcs))
		// Assert our hand-written service is discoverable + reflection registered itself.
		has := func(name string) bool {
			for _, s := range svcs {
				if s == name {
					return true
				}
			}
			return false
		}
		fwOK("reflection lists echo.Echo", has("echo.Echo"))
		fwOK("reflection lists Health", has("grpc.health.v1.Health"))
		fwOK("reflection lists reflection v1", has("grpc.reflection.v1.ServerReflection"))
	}
}

// ---------------------------------------------------------------------------
// Connectivity state API over bufconn
// ---------------------------------------------------------------------------

func cat_connectivity() {
	// connectivity.State String() constants are deterministic.
	fwOK("connectivity Idle String", connectivity.Idle.String())
	fwOK("connectivity Connecting String", connectivity.Connecting.String())
	fwOK("connectivity Ready String", connectivity.Ready.String())
	fwOK("connectivity TransientFailure String", connectivity.TransientFailure.String())
	fwOK("connectivity Shutdown String", connectivity.Shutdown.String())

	impl := &grpc_echoServer{}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat_connectivity grpc_harness err", err)
		return
	}
	defer h.close()

	// Fresh NewClient connections start Idle (lazy connect).
	st0 := h.conn.GetState()
	fwOK("GetState initial is Idle", st0 == connectivity.Idle)

	// Trigger connection; an RPC moves it toward Ready.
	h.conn.Connect()
	var resp grpc_EchoResp
	_ = h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "warm", N: 1}, &resp, grpc_forceJSON())
	// After a successful RPC the conn must be Ready.
	wctx, wcancel := context.WithTimeout(grpc_bg(), 2*time.Second)
	for h.conn.GetState() != connectivity.Ready {
		if !h.conn.WaitForStateChange(wctx, h.conn.GetState()) {
			break
		}
	}
	wcancel()
	fwOK("GetState after RPC is Ready", h.conn.GetState() == connectivity.Ready)
}

// ---------------------------------------------------------------------------
// GracefulStop vs Stop: graceful lets an in-flight RPC complete.
// ---------------------------------------------------------------------------

func cat_graceful_vs_stop() {
	// GracefulStop: a long-running RPC started before GracefulStop completes OK.
	started := make(chan struct{})
	release := make(chan struct{})
	impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		close(started)
		<-release
		return &grpc_EchoResp{Msg: req.Msg, N: req.N}, nil
	}}
	lis := bufconn.Listen(grpc_bufSize)
	srv := grpc.NewServer()
	srv.RegisterService(grpc_echoServiceDesc(impl), impl)
	serveErr := make(chan error, 1)
	go func() { serveErr <- srv.Serve(lis) }()
	conn, _ := grpc.NewClient("passthrough:///bufnet",
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) { return lis.DialContext(ctx) }),
		grpc.WithTransportCredentials(insecure.NewCredentials()))

	rpcDone := make(chan error, 1)
	go func() {
		var r grpc_EchoResp
		rpcDone <- conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "g", N: 4}, &r, grpc_forceJSON())
	}()
	<-started
	gsDone := make(chan struct{})
	go func() { srv.GracefulStop(); close(gsDone) }()
	// Release the handler so the in-flight RPC can finish under GracefulStop.
	close(release)
	rerr := <-rpcDone
	fwOK("GracefulStop in-flight RPC completes OK", rerr == nil)
	<-gsDone
	// Serve returns nil (NOT ErrServerStopped) when GracefulStop is called while
	// it is actively serving (server.go:890 blocks until done, returns nil).
	srvRet := <-serveErr
	fwOK("Serve returns nil after GracefulStop", srvRet == nil)
	conn.Close()

	// Stop aborts an in-flight RPC: handler blocked, Stop() -> RPC errors.
	started2 := make(chan struct{})
	block2 := make(chan struct{})
	impl2 := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		close(started2)
		select {
		case <-block2:
		case <-ctx.Done():
		}
		return nil, status.FromContextError(ctx.Err()).Err()
	}}
	lis2 := bufconn.Listen(grpc_bufSize)
	srv2 := grpc.NewServer()
	srv2.RegisterService(grpc_echoServiceDesc(impl2), impl2)
	serveErr2 := make(chan error, 1)
	go func() { serveErr2 <- srv2.Serve(lis2) }()
	conn2, _ := grpc.NewClient("passthrough:///bufnet",
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) { return lis2.DialContext(ctx) }),
		grpc.WithTransportCredentials(insecure.NewCredentials()))
	rpcDone2 := make(chan error, 1)
	go func() {
		var r grpc_EchoResp
		rpcDone2 <- conn2.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "s", N: 4}, &r, grpc_forceJSON())
	}()
	<-started2
	srv2.Stop() // abrupt: aborts in-flight
	rerr2 := <-rpcDone2
	fwOK("Stop aborts in-flight RPC (err != nil)", rerr2 != nil)
	srvRet2 := <-serveErr2
	fwOK("Serve returns nil after Stop (during serving)", srvRet2 == nil)
	conn2.Close()

	// ErrServerStopped is returned only when Serve is called AFTER the server
	// has been stopped (server.go:882-883). Verify that distinct sentinel path.
	stoppedSrv := grpc.NewServer()
	stoppedSrv.Stop()
	lis3 := bufconn.Listen(grpc_bufSize)
	serr3 := stoppedSrv.Serve(lis3)
	fwOK("Serve on already-stopped server == ErrServerStopped", errors.Is(serr3, grpc.ErrServerStopped))
}

// ---------------------------------------------------------------------------
// ServerTransportStream APIs (grpc.ServerTransportStreamFromContext + Method)
// ---------------------------------------------------------------------------

func cat_transport_stream() {
	var sawMethod string
	var stsOK bool
	impl := &grpc_echoServer{unaryHook: func(ctx context.Context, req *grpc_EchoReq) (*grpc_EchoResp, error) {
		sts := grpc.ServerTransportStreamFromContext(ctx)
		stsOK = sts != nil
		if sts != nil {
			sawMethod = sts.Method()
			_ = sts.SetHeader(metadata.Pairs("x-sts-hdr", "1"))
			_ = sts.SetTrailer(metadata.Pairs("x-sts-trl", "1"))
		}
		return &grpc_EchoResp{Msg: req.Msg, N: req.N}, nil
	}}
	h, err := grpc_newHarness(nil, nil, func(s *grpc.Server) {
		s.RegisterService(grpc_echoServiceDesc(impl), impl)
	})
	if err != nil {
		fwOK("cat_transport grpc_harness err", err)
		return
	}
	defer h.close()

	var resp grpc_EchoResp
	var hdr, tlr metadata.MD
	ierr := h.conn.Invoke(grpc_bg(), "/echo.Echo/UnaryEcho", &grpc_EchoReq{Msg: "t", N: 1}, &resp,
		grpc_forceJSON(), grpc.Header(&hdr), grpc.Trailer(&tlr))
	fwOK("transport-stream RPC err==nil", ierr == nil)
	fwOK("ServerTransportStreamFromContext non-nil", stsOK)
	fwOK("ServerTransportStream.Method()", sawMethod)
	fwOK("ServerTransportStream SetHeader visible", hdr.Get("x-sts-hdr"))
	fwOK("ServerTransportStream SetTrailer visible", tlr.Get("x-sts-trl"))
}
