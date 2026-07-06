package org.starry.dod;

import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.eclipse.jetty.server.Connector;
import org.eclipse.jetty.server.Request;
import org.eclipse.jetty.server.HttpConfiguration;
import org.eclipse.jetty.server.HttpConnectionFactory;
import org.eclipse.jetty.server.handler.AbstractHandler;
import org.eclipse.jetty.server.handler.ContextHandler;
import org.eclipse.jetty.server.handler.ContextHandlerCollection;
import org.eclipse.jetty.server.handler.HandlerList;
import org.eclipse.jetty.server.handler.ResourceHandler;
import org.eclipse.jetty.server.handler.ErrorHandler;
import org.eclipse.jetty.util.thread.QueuedThreadPool;
import org.eclipse.jetty.util.resource.Resource;

import jakarta.servlet.ServletException;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;

import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.PrintWriter;
import java.net.HttpURLConnection;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.URI;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Carpet-level coverage for embedded Eclipse Jetty 11.0.21 (jetty-server + jakarta.servlet).
 *
 * Exercises: QueuedThreadPool sizing/naming, Server(ThreadPool) wiring, HttpConfiguration +
 * HttpConnectionFactory, ServerConnector bound strictly to 127.0.0.1 on a high free port,
 * AbstractHandler routing, ContextHandler / ContextHandlerCollection / HandlerList composition,
 * ResourceHandler static file serving, custom ErrorHandler, full HTTP method matrix
 * (GET/POST/PUT/DELETE/HEAD), request parsing (query/header/form/raw body), response shaping
 * (status line / reason phrase / headers / content-type / body), error codes (404/405/500),
 * and the Server start/isRunning/stop lifecycle. All traffic is real HttpURLConnection /
 * loopback round-trips. No external network, no servlet container module needed.
 */
public final class JettyCarpet {

    static final String MARKER = "JETTY_DONE";
    static int ok = 0;
    static int fail = 0;

    // ---- assertion helpers (self counting) -------------------------------
    static void check(String name, boolean cond) {
        if (cond) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name);
        }
    }

    static void eq(String name, Object expected, Object actual) {
        if (Objects.equals(expected, actual)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " expected=[" + expected + "] actual=[" + actual + "]");
        }
    }

    static void startsWith(String name, String value, String prefix) {
        if (value != null && value.startsWith(prefix)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " value=[" + value + "] prefix=[" + prefix + "]");
        }
    }

    static void has(String name, String haystack, String needle) {
        if (haystack != null && haystack.contains(needle)) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + name + " value=[" + haystack + "] needle=[" + needle + "]");
        }
    }

    // ---- tiny HTTP client over loopback ----------------------------------
    static final class Resp {
        int code;
        String reason;
        String contentType;
        String body;
        Map<String, String> headers = new HashMap<>();

        String h(String k) {
            return headers.get(k.toLowerCase());
        }
    }

    static String readAll(InputStream in) throws IOException {
        if (in == null) {
            return "";
        }
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        byte[] buf = new byte[4096];
        int n;
        while ((n = in.read(buf)) >= 0) {
            bos.write(buf, 0, n);
        }
        return new String(bos.toByteArray(), StandardCharsets.UTF_8);
    }

    static Resp http(int port, String method, String path,
                     Map<String, String> reqHeaders, byte[] body, String bodyContentType) throws IOException {
        URL url = new URL("http://127.0.0.1:" + port + path);
        HttpURLConnection c = (HttpURLConnection) url.openConnection();
        c.setRequestMethod(method);
        c.setInstanceFollowRedirects(false);
        c.setConnectTimeout(8000);
        c.setReadTimeout(8000);
        if (reqHeaders != null) {
            for (Map.Entry<String, String> e : reqHeaders.entrySet()) {
                c.setRequestProperty(e.getKey(), e.getValue());
            }
        }
        if (body != null) {
            c.setDoOutput(true);
            if (bodyContentType != null) {
                c.setRequestProperty("Content-Type", bodyContentType);
            }
            try (OutputStream os = c.getOutputStream()) {
                os.write(body);
            }
        }
        Resp r = new Resp();
        r.code = c.getResponseCode();
        r.reason = c.getResponseMessage();
        r.contentType = c.getContentType();
        for (Map.Entry<String, List<String>> en : c.getHeaderFields().entrySet()) {
            if (en.getKey() != null && en.getValue() != null && !en.getValue().isEmpty()) {
                r.headers.put(en.getKey().toLowerCase(), en.getValue().get(0));
            }
        }
        InputStream in = (r.code >= 400) ? c.getErrorStream() : c.getInputStream();
        r.body = readAll(in);
        c.disconnect();
        return r;
    }

    static Resp get(int port, String path) throws IOException {
        return http(port, "GET", path, null, null, null);
    }

    // ---- application routing handler -------------------------------------
    static final class ApiHandler extends AbstractHandler {
        final AtomicInteger seq = new AtomicInteger(0);

        @Override
        public void handle(String target, Request baseRequest,
                           HttpServletRequest req, HttpServletResponse resp)
                throws IOException, ServletException {
            String method = req.getMethod();
            String path = req.getPathInfo();
            if (path == null) {
                path = "/";
            }
            baseRequest.setHandled(true);
            resp.setHeader("X-Handler", "api");

            if (path.equals("/hello")) {
                if (method.equals("GET") || method.equals("HEAD")) {
                    resp.setStatus(HttpServletResponse.SC_OK);
                    resp.setContentType("text/plain; charset=utf-8");
                    resp.getWriter().print("hello");
                } else {
                    resp.setStatus(405);
                    resp.setHeader("Allow", "GET, HEAD");
                    resp.setContentType("application/json");
                    resp.getWriter().print("{\"error\":\"method\"}");
                }
                return;
            }
            if (path.equals("/json")) {
                resp.setStatus(HttpServletResponse.SC_OK);
                resp.setContentType("application/json");
                resp.getWriter().print("{\"msg\":\"ok\",\"n\":42}");
                return;
            }
            if (path.equals("/echo")) {
                String name = req.getParameter("name");
                if (name == null) {
                    name = "anon";
                }
                resp.setStatus(HttpServletResponse.SC_OK);
                resp.setContentType("text/plain; charset=utf-8");
                resp.getWriter().print("echo:" + name);
                return;
            }
            if (path.equals("/multi")) {
                String[] vs = req.getParameterValues("v");
                int len = (vs == null) ? 0 : vs.length;
                resp.setStatus(HttpServletResponse.SC_OK);
                resp.setContentType("text/plain");
                resp.getWriter().print(String.valueOf(len));
                return;
            }
            if (path.equals("/headers")) {
                String token = req.getHeader("X-Token");
                resp.setStatus(HttpServletResponse.SC_OK);
                resp.setHeader("X-Echo-Token", token);
                resp.setContentType("text/plain");
                resp.getWriter().print("token=" + token);
                return;
            }
            if (path.equals("/utf8")) {
                resp.setStatus(HttpServletResponse.SC_OK);
                resp.setContentType("text/plain; charset=utf-8");
                resp.getWriter().print("héllo-世界");
                return;
            }
            if (path.equals("/users")) {
                if (method.equals("POST")) {
                    int id = seq.incrementAndGet();
                    String name = req.getParameter("name");
                    String age = req.getParameter("age");
                    resp.setStatus(HttpServletResponse.SC_CREATED);
                    resp.setHeader("Location", "/api/users/" + id);
                    resp.setContentType("application/json");
                    resp.getWriter().print("{\"id\":" + id + ",\"name\":\"" + name + "\",\"age\":" + age + "}");
                } else {
                    resp.setStatus(405);
                    resp.setHeader("Allow", "POST");
                    resp.getWriter().print("{\"error\":\"method\"}");
                }
                return;
            }
            if (path.startsWith("/users/")) {
                String id = path.substring("/users/".length());
                if (method.equals("PUT")) {
                    String body = readBody(req);
                    resp.setStatus(HttpServletResponse.SC_OK);
                    resp.setContentType("text/plain");
                    resp.getWriter().print("updated:" + id + ":" + body);
                } else if (method.equals("DELETE")) {
                    resp.setStatus(HttpServletResponse.SC_NO_CONTENT);
                } else {
                    resp.setStatus(405);
                    resp.setHeader("Allow", "PUT, DELETE");
                    resp.getWriter().print("{\"error\":\"method\"}");
                }
                return;
            }
            if (path.equals("/raw")) {
                String body = readBody(req);
                resp.setStatus(HttpServletResponse.SC_OK);
                resp.setContentType("text/plain");
                resp.getWriter().print("recv:" + body);
                return;
            }
            if (path.equals("/boom")) {
                throw new ServletException("intentional failure for 500 path");
            }
            // default: not found inside /api
            resp.setStatus(HttpServletResponse.SC_NOT_FOUND);
            resp.setContentType("application/json");
            resp.getWriter().print("{\"error\":\"not_found\"}");
        }

        private static String readBody(HttpServletRequest req) throws IOException {
            StringBuilder sb = new StringBuilder();
            try (BufferedReader r = req.getReader()) {
                char[] buf = new char[1024];
                int n;
                while ((n = r.read(buf)) >= 0) {
                    sb.append(buf, 0, n);
                }
            }
            return sb.toString();
        }
    }

    // fallback handler placed last in the HandlerList: anything unmatched -> deterministic 404
    static final class RootFallback extends AbstractHandler {
        @Override
        public void handle(String target, Request baseRequest,
                           HttpServletRequest req, HttpServletResponse resp) throws IOException {
            if (baseRequest.isHandled()) {
                return;
            }
            baseRequest.setHandled(true);
            resp.setStatus(HttpServletResponse.SC_NOT_FOUND);
            resp.setContentType("text/plain");
            resp.getWriter().print("ROOT-404");
        }
    }

    static int findFreePort(int from, int to) throws IOException {
        for (int p = from; p < to; p++) {
            try (ServerSocket s = new ServerSocket()) {
                s.setReuseAddress(true);
                s.bind(new InetSocketAddress(InetAddress.getByName("127.0.0.1"), p));
                return p;
            } catch (IOException ignore) {
                // port busy -> try next
            }
        }
        throw new IOException("no free 127.0.0.1 port in [" + from + "," + to + ")");
    }

    public static void main(String[] args) throws Exception {
        // ---- static content directory under /tmp -------------------------
        Path staticDir = Files.createTempDirectory(Paths.get("/tmp"), "jetty-carpet-");
        Files.write(staticDir.resolve("data.txt"), "STATIC-FILE-OK".getBytes(StandardCharsets.UTF_8));
        Files.write(staticDir.resolve("data.json"), "{\"k\":1}".getBytes(StandardCharsets.UTF_8));
        Files.write(staticDir.resolve("index.html"), "INDEX-PAGE".getBytes(StandardCharsets.UTF_8));

        // ---- thread pool -------------------------------------------------
        QueuedThreadPool pool = new QueuedThreadPool(16, 4);
        pool.setName("starry-jetty");
        eq("pool.maxThreads", 16, pool.getMaxThreads());
        eq("pool.minThreads", 4, pool.getMinThreads());
        eq("pool.name", "starry-jetty", pool.getName());

        // ---- server + connector (HttpConfiguration) ----------------------
        Server server = new Server(pool);
        check("server.usesGivenPool", server.getThreadPool() == pool);

        HttpConfiguration httpConfig = new HttpConfiguration();
        httpConfig.setSendServerVersion(false);
        httpConfig.setSendDateHeader(true);
        httpConfig.setOutputBufferSize(32768);
        eq("httpConfig.sendServerVersion", false, httpConfig.getSendServerVersion());
        eq("httpConfig.sendDateHeader", true, httpConfig.getSendDateHeader());
        eq("httpConfig.outputBufferSize", 32768, httpConfig.getOutputBufferSize());

        int port = findFreePort(18080, 18280);
        ServerConnector connector = new ServerConnector(server, new HttpConnectionFactory(httpConfig));
        connector.setHost("127.0.0.1");
        connector.setPort(port);
        connector.setIdleTimeout(30000L);
        server.addConnector(connector);

        eq("connector.host", "127.0.0.1", connector.getHost());
        eq("connector.port", port, connector.getPort());
        eq("connector.idleTimeout", 30000L, connector.getIdleTimeout());
        eq("server.connectorCount", 1, server.getConnectors().length);
        check("server.notRunningBeforeStart", !server.isRunning());
        check("server.notStartedBeforeStart", !server.isStarted());
        eq("connector.localPortBeforeStart", -1, connector.getLocalPort());

        // ---- handler tree ------------------------------------------------
        ApiHandler api = new ApiHandler();
        ContextHandler apiCtx = new ContextHandler("/api");
        apiCtx.setAllowNullPathInfo(true);
        apiCtx.setHandler(api);
        eq("apiCtx.contextPath", "/api", apiCtx.getContextPath());

        ResourceHandler resourceHandler = new ResourceHandler();
        resourceHandler.setBaseResource(Resource.newResource(staticDir.toFile()));
        resourceHandler.setDirAllowed(false);
        resourceHandler.setWelcomeFiles(new String[]{"index.html"});
        ContextHandler staticCtx = new ContextHandler("/static");
        staticCtx.setHandler(resourceHandler);
        eq("staticCtx.contextPath", "/static", staticCtx.getContextPath());

        ContextHandlerCollection contexts = new ContextHandlerCollection();
        contexts.addHandler(apiCtx);
        contexts.addHandler(staticCtx);
        eq("contexts.childCount", 2, contexts.getHandlers().length);

        HandlerList root = new HandlerList();
        root.addHandler(contexts);
        root.addHandler(new RootFallback());
        eq("root.childCount", 2, root.getHandlers().length);
        server.setHandler(root);

        ErrorHandler errorHandler = new ErrorHandler();
        errorHandler.setShowStacks(false);
        server.setErrorHandler(errorHandler);

        try {
            server.start();

            // ---- post-start lifecycle --------------------------------
            check("server.isStarted", server.isStarted());
            check("server.isRunning", server.isRunning());
            check("server.notStopped", !server.isStopped());
            check("pool.isRunning", pool.isRunning());
            eq("connector.localPort", port, connector.getLocalPort());
            URI uri = server.getURI();
            check("server.uriNotNull", uri != null);
            if (uri != null) {
                eq("server.uri.port", port, uri.getPort());
            } else {
                fail++;
                System.out.println("FAIL server.uri.port (uri null)");
            }

            // ---- GET /api/hello : text/plain -------------------------
            Resp r = get(port, "/api/hello");
            eq("hello.code", 200, r.code);
            eq("hello.reason", "OK", r.reason);
            startsWith("hello.contentType", r.contentType, "text/plain");
            eq("hello.body", "hello", r.body);
            eq("hello.xHandler", "api", r.h("X-Handler"));
            check("hello.noServerHeader", r.h("Server") == null);
            check("hello.hasDateHeader", r.h("Date") != null);

            // ---- GET /api/json : application/json --------------------
            r = get(port, "/api/json");
            eq("json.code", 200, r.code);
            startsWith("json.contentType", r.contentType, "application/json");
            eq("json.body", "{\"msg\":\"ok\",\"n\":42}", r.body);

            // ---- GET /api/echo?name=Starry ---------------------------
            r = get(port, "/api/echo?name=Starry");
            eq("echo.code", 200, r.code);
            eq("echo.body", "echo:Starry", r.body);

            // ---- GET /api/echo (default param) -----------------------
            r = get(port, "/api/echo");
            eq("echoDefault.body", "echo:anon", r.body);

            // ---- GET /api/echo?name=a%20b (url-encoded space) --------
            r = get(port, "/api/echo?name=a%20b");
            eq("echoEncoded.body", "echo:a b", r.body);

            // ---- GET /api/multi?v=1&v=2&v=3 (multi-value param) ------
            r = get(port, "/api/multi?v=1&v=2&v=3");
            eq("multi.code", 200, r.code);
            eq("multi.body", "3", r.body);

            // ---- GET /api/headers with request header ----------------
            Map<String, String> hh = new HashMap<>();
            hh.put("X-Token", "abc123");
            r = http(port, "GET", "/api/headers", hh, null, null);
            eq("headers.code", 200, r.code);
            eq("headers.body", "token=abc123", r.body);
            eq("headers.echoHeader", "abc123", r.h("X-Echo-Token"));

            // ---- GET /api/utf8 (charset) -----------------------------
            r = get(port, "/api/utf8");
            eq("utf8.code", 200, r.code);
            has("utf8.contentTypeCharset", r.contentType.toLowerCase(), "charset=utf-8");
            eq("utf8.body", "héllo-世界", r.body);

            // ---- POST /api/users (form body) -------------------------
            byte[] form = "name=alice&age=30".getBytes(StandardCharsets.UTF_8);
            r = http(port, "POST", "/api/users", null, form, "application/x-www-form-urlencoded");
            eq("createUser.code", 201, r.code);
            eq("createUser.reason", "Created", r.reason);
            eq("createUser.location", "/api/users/1", r.h("Location"));
            startsWith("createUser.contentType", r.contentType, "application/json");
            eq("createUser.body", "{\"id\":1,\"name\":\"alice\",\"age\":30}", r.body);

            // second create -> id increments
            r = http(port, "POST", "/api/users", null,
                    "name=bob&age=25".getBytes(StandardCharsets.UTF_8), "application/x-www-form-urlencoded");
            eq("createUser2.code", 201, r.code);
            eq("createUser2.location", "/api/users/2", r.h("Location"));
            eq("createUser2.body", "{\"id\":2,\"name\":\"bob\",\"age\":25}", r.body);

            // ---- POST /api/raw (raw text body) -----------------------
            r = http(port, "POST", "/api/raw", null,
                    "hello world".getBytes(StandardCharsets.UTF_8), "text/plain");
            eq("raw.code", 200, r.code);
            eq("raw.body", "recv:hello world", r.body);

            // ---- PUT /api/users/7 (path id + body) -------------------
            r = http(port, "PUT", "/api/users/7", null,
                    "patch".getBytes(StandardCharsets.UTF_8), "text/plain");
            eq("put.code", 200, r.code);
            eq("put.body", "updated:7:patch", r.body);

            // ---- DELETE /api/users/9 (204 no content) ----------------
            r = http(port, "DELETE", "/api/users/9", null, null, null);
            eq("delete.code", 204, r.code);
            eq("delete.reason", "No Content", r.reason);
            eq("delete.emptyBody", "", r.body);

            // ---- HEAD /api/hello (no body) ---------------------------
            r = http(port, "HEAD", "/api/hello", null, null, null);
            eq("head.code", 200, r.code);
            eq("head.emptyBody", "", r.body);
            eq("head.xHandler", "api", r.h("X-Handler"));

            // ---- 405 method not allowed ------------------------------
            r = http(port, "POST", "/api/hello", null, new byte[0], "text/plain");
            eq("notAllowed.code", 405, r.code);
            eq("notAllowed.reason", "Method Not Allowed", r.reason);
            eq("notAllowed.allow", "GET, HEAD", r.h("Allow"));

            // ---- 404 inside /api context -----------------------------
            r = get(port, "/api/nope");
            eq("apiNotFound.code", 404, r.code);
            eq("apiNotFound.reason", "Not Found", r.reason);
            eq("apiNotFound.body", "{\"error\":\"not_found\"}", r.body);

            // ---- 404 from root fallback (unmatched context) ----------
            r = get(port, "/no/such/path");
            eq("rootNotFound.code", 404, r.code);
            eq("rootNotFound.body", "ROOT-404", r.body);

            // ---- 500 server error path -------------------------------
            r = get(port, "/api/boom");
            eq("boom.code", 500, r.code);
            has("boom.body", r.body, "500");

            // ---- static resource serving -----------------------------
            r = get(port, "/static/data.txt");
            eq("static.code", 200, r.code);
            startsWith("static.contentType", r.contentType, "text/plain");
            eq("static.body", "STATIC-FILE-OK", r.body);

            r = get(port, "/static/data.json");
            eq("staticJson.code", 200, r.code);
            startsWith("staticJson.contentType", r.contentType, "application/json");
            eq("staticJson.body", "{\"k\":1}", r.body);

            // welcome file for directory root
            r = get(port, "/static/");
            eq("welcome.code", 200, r.code);
            startsWith("welcome.contentType", r.contentType, "text/html");
            eq("welcome.body", "INDEX-PAGE", r.body);

            // missing static resource
            r = get(port, "/static/missing.txt");
            eq("staticMissing.code", 404, r.code);

            // ---- repeated requests (stability / keepalive churn) -----
            int loopOk = 0;
            for (int i = 0; i < 20; i++) {
                Resp lr = get(port, "/api/hello");
                if (lr.code == 200 && "hello".equals(lr.body)) {
                    loopOk++;
                }
            }
            eq("loop.allOk", 20, loopOk);

            // ---- thread pool stayed within bounds --------------------
            check("pool.boundedThreads", pool.getThreads() <= pool.getMaxThreads());

        } finally {
            server.stop();
        }

        // ---- post-stop lifecycle ------------------------------------
        check("server.stoppedAfterStop", server.isStopped());
        check("server.notStartedAfterStop", !server.isStarted());
        check("server.notRunningAfterStop", !server.isRunning());
        check("connector.localPortClosed", connector.getLocalPort() <= 0);

        // connection refused after stop
        boolean refused = false;
        try {
            get(port, "/api/hello");
        } catch (IOException e) {
            refused = true;
        }
        check("server.connectionRefusedAfterStop", refused);

        // ---- cleanup temp files -------------------------------------
        try {
            Files.deleteIfExists(staticDir.resolve("data.txt"));
            Files.deleteIfExists(staticDir.resolve("data.json"));
            Files.deleteIfExists(staticDir.resolve("index.html"));
            Files.deleteIfExists(staticDir);
        } catch (IOException ignore) {
            // best effort
        }

        System.out.println(MARKER + "_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println(MARKER);
        }
    }
}
