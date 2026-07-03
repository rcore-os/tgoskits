package org.starry.dod;

/*
 * WarCarpet — industrial-grade carpet for the real ".war deployment" surface on
 * Jetty 11.0.21 (jetty-server + jakarta.servlet 5.0 API, the only modules bundled
 * in jetty-demo.jar; jetty-webapp/jetty-servlet are NOT present, so a faithful war
 * pipeline is built directly on jetty-server + the jakarta.servlet API).
 *
 * Pipeline exercised (every step is real, nothing stubbed):
 *   1. write two minimal jakarta HttpServlet sources to /tmp
 *   2. compile them in-process with the JDK compiler (javax.tools)
 *   3. assemble a STANDARD .war (WEB-INF/web.xml + WEB-INF/classes/** + welcome file)
 *      and zip it to /tmp/.../app.war
 *   4. validate the .war artifact: zip entries + web.xml DOM (servlet/servlet-mapping/
 *      init-param/context-param/welcome-file/load-on-startup)
 *   5. explode the .war to a webapp dir (proving deployment reads FROM the artifact)
 *   6. deploy: URLClassLoader over WEB-INF/classes, parse web.xml, instantiate +
 *      init() each servlet with its ServletConfig/ServletContext + init-params
 *   7. run an embedded Jetty Server (bound 127.0.0.1) whose ContextHandler dispatches
 *      real HTTP requests through HttpServlet.service -> doGet/doPost/doPut/doDelete
 *      using the servlet-mapping algorithm (exact / prefix /x/* / extension *.ext /
 *      welcome-file / 404)
 *   8. hit it with HttpURLConnection: status line, headers, content-type, body exact
 *      values, GET/POST/PUT/DELETE/HEAD/OPTIONS, multiple mappings, init-params,
 *      redirect, cookie, 404, 405
 *   9. stop the server, destroy() the servlets, verify the port is released
 *
 * Deterministic, loopback-only, no external network, /tmp temp files only,
 * memory friendly (small thread pool), self-counting.
 */

import java.io.*;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.lang.reflect.*;
import java.util.*;
import java.util.zip.*;
import javax.tools.*;
import javax.xml.parsers.*;
import org.w3c.dom.*;

import jakarta.servlet.*;
import jakarta.servlet.http.*;
import org.eclipse.jetty.server.Server;
import org.eclipse.jetty.server.ServerConnector;
import org.eclipse.jetty.server.Request;
import org.eclipse.jetty.server.handler.AbstractHandler;
import org.eclipse.jetty.server.handler.ContextHandler;
import org.eclipse.jetty.util.thread.QueuedThreadPool;

public class WarCarpet {

    // ---- self-counting harness ----
    static int ok = 0, fail = 0;
    static void pass() { ok++; }
    static void fail(String name, String detail) {
        fail++;
        System.out.println("FAIL " + name + (detail == null ? "" : " :: " + detail));
    }
    static void check(String name, boolean cond) { if (cond) pass(); else fail(name, null); }
    static void eq(String name, Object exp, Object act) {
        if (Objects.equals(exp, act)) pass();
        else fail(name, "expected=[" + exp + "] actual=[" + act + "]");
    }
    static void contains(String name, String hay, String needle) {
        if (hay != null && hay.contains(needle)) pass();
        else fail(name, "needle=[" + needle + "] not in [" + hay + "]");
    }

    static final String CTX = "/app";
    static final int PORT = 18456;

    static File WORK, WAR, WEBAPP;

    public static void main(String[] args) {
        Server server = null;
        Deployer deployer = null;
        ServerConnector con = null;
        try {
            setupWorkDirs();
            buildWarSources();
            compileServlets();
            packageWar();

            validateWarArtifact();   // structure + web.xml DOM asserts (pre-deploy)
            explodeWar();            // deploy reads from the artifact

            deployer = new Deployer(WEBAPP, CTX);
            deployer.deploy();
            eq("deploy.initCount", 2, deployer.initCount);
            eq("deploy.servletCount", 2, deployer.servlets.size());
            eq("deploy.welcome", "index.html", deployer.welcomeFiles.isEmpty() ? null : deployer.welcomeFiles.get(0));

            // ---- start embedded Jetty (loopback) ----
            QueuedThreadPool pool = new QueuedThreadPool(10, 3);
            pool.setName("warcarpet");
            server = new Server(pool);
            con = new ServerConnector(server, 1, 1);
            con.setHost("127.0.0.1");
            con.setPort(PORT);
            con.setIdleTimeout(15000);
            server.addConnector(con);

            ContextHandler ctx = new ContextHandler();
            ctx.setContextPath(CTX);
            ctx.setAllowNullPathInfo(true);
            ctx.setHandler(new Dispatcher(deployer, CTX, WEBAPP));
            server.setHandler(ctx);
            server.start();

            check("server.started", server.isStarted());
            eq("connector.host", "127.0.0.1", con.getHost());
            int port = con.getLocalPort();
            check("connector.port>0", port > 0);
            eq("connector.port.fixed", PORT, port);

            runHttpAsserts(port);

            // ---- shutdown ----
            server.stop();
            check("server.stopped", server.isStopped());
            deployer.destroyAll();
            eq("deploy.destroyCount", 2, deployer.destroyCount);
            check("port.released", portFreeAfterStop(port));

        } catch (Throwable t) {
            fail("fatal", t.getClass().getName() + ": " + t.getMessage());
            t.printStackTrace(System.out);
            try { if (server != null) server.stop(); } catch (Exception ignore) {}
        }

        System.out.println("WAR_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("WAR_DONE");
    }

    // ================= work dirs =================
    static void setupWorkDirs() throws IOException {
        String tmp = System.getProperty("java.io.tmpdir", "/tmp");
        File base = new File(tmp, "starry_warcarpet_work");
        WORK = base;
        deleteRec(base);
        new File(base, "src/com/example/web").mkdirs();
        new File(base, "warroot/WEB-INF/classes").mkdirs();
        WAR = new File(base, "app.war");
        WEBAPP = new File(base, "webapp");
    }

    static void deleteRec(File f) throws IOException {
        if (!f.exists()) return;
        if (f.isDirectory()) for (File c : f.listFiles()) deleteRec(c);
        f.delete();
    }

    // ================= war source + web.xml =================
    static void buildWarSources() throws IOException {
        String echo =
            "package com.example.web;\n" +
            "import java.io.*;\n" +
            "import jakarta.servlet.*;\n" +
            "import jakarta.servlet.http.*;\n" +
            "public class EchoServlet extends HttpServlet {\n" +
            "  private String greeting = \"uninit\";\n" +
            "  private int initCount = 0;\n" +
            "  @Override public void init() throws ServletException {\n" +
            "    String g = getServletConfig().getInitParameter(\"greeting\");\n" +
            "    this.greeting = (g == null) ? \"none\" : g;\n" +
            "    this.initCount++;\n" +
            "  }\n" +
            "  @Override protected void doGet(HttpServletRequest req, HttpServletResponse resp)\n" +
            "      throws ServletException, IOException {\n" +
            "    String mode = req.getParameter(\"mode\");\n" +
            "    if (\"redirect\".equals(mode)) { resp.sendRedirect(req.getContextPath()+\"/info\"); return; }\n" +
            "    resp.setStatus(200);\n" +
            "    resp.setCharacterEncoding(\"UTF-8\");\n" +
            "    resp.setContentType(\"text/plain\");\n" +
            "    resp.setHeader(\"X-Servlet\", \"echo\");\n" +
            "    resp.addHeader(\"X-Init\", String.valueOf(initCount));\n" +
            "    Cookie ck = new Cookie(\"warcookie\", \"ck1\"); ck.setPath(\"/\"); resp.addCookie(ck);\n" +
            "    String name = req.getParameter(\"name\");\n" +
            "    String[] vals = req.getParameterValues(\"name\");\n" +
            "    int vc = (vals == null) ? 0 : vals.length;\n" +
            "    String hdr = req.getHeader(\"X-Req\");\n" +
            "    PrintWriter w = resp.getWriter();\n" +
            "    w.print(\"GET|greeting=\"+greeting+\"|name=\"+name+\"|vals=\"+vc+\"|hdr=\"+hdr\n" +
            "      +\"|sp=\"+req.getServletPath()+\"|pi=\"+req.getPathInfo()+\"|qs=\"+req.getQueryString()\n" +
            "      +\"|proto=\"+req.getProtocol()+\"|method=\"+req.getMethod()+\"|ctx=\"+req.getContextPath());\n" +
            "  }\n" +
            "  @Override protected void doPost(HttpServletRequest req, HttpServletResponse resp)\n" +
            "      throws ServletException, IOException {\n" +
            "    byte[] body = req.getInputStream().readAllBytes();\n" +
            "    resp.setStatus(200); resp.setContentType(\"text/plain\");\n" +
            "    resp.getWriter().print(\"POST|len=\"+body.length+\"|body=\"+new String(body,\"UTF-8\"));\n" +
            "  }\n" +
            "  @Override protected void doPut(HttpServletRequest req, HttpServletResponse resp)\n" +
            "      throws ServletException, IOException {\n" +
            "    byte[] body = req.getInputStream().readAllBytes();\n" +
            "    resp.setStatus(200); resp.setContentType(\"text/plain\");\n" +
            "    resp.getWriter().print(\"PUT|body=\"+new String(body,\"UTF-8\"));\n" +
            "  }\n" +
            "  @Override protected void doDelete(HttpServletRequest req, HttpServletResponse resp)\n" +
            "      throws ServletException, IOException {\n" +
            "    resp.setHeader(\"X-Deleted\", \"true\"); resp.setStatus(204);\n" +
            "  }\n" +
            "}\n";

        String info =
            "package com.example.web;\n" +
            "import java.io.*;\n" +
            "import jakarta.servlet.*;\n" +
            "import jakarta.servlet.http.*;\n" +
            "public class InfoServlet extends HttpServlet {\n" +
            "  @Override protected void doGet(HttpServletRequest req, HttpServletResponse resp)\n" +
            "      throws ServletException, IOException {\n" +
            "    ServletContext c = getServletContext();\n" +
            "    String appName = (c == null) ? \"noctx\" : c.getInitParameter(\"app.name\");\n" +
            "    resp.setStatus(200); resp.setCharacterEncoding(\"UTF-8\"); resp.setContentType(\"text/html\");\n" +
            "    resp.getWriter().print(\"<html><body><info app=\"+appName+\" servlet=\"+getServletName()+\"/></body></html>\");\n" +
            "  }\n" +
            "}\n";

        Files.writeString(new File(WORK, "src/com/example/web/EchoServlet.java").toPath(), echo);
        Files.writeString(new File(WORK, "src/com/example/web/InfoServlet.java").toPath(), info);

        String webxml =
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n" +
            "<web-app xmlns=\"https://jakarta.ee/xml/ns/jakartaee\"\n" +
            "         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n" +
            "         xsi:schemaLocation=\"https://jakarta.ee/xml/ns/jakartaee https://jakarta.ee/xml/ns/jakartaee/web-app_5_0.xsd\"\n" +
            "         version=\"5.0\">\n" +
            "  <display-name>WarCarpetApp</display-name>\n" +
            "  <context-param>\n" +
            "    <param-name>app.name</param-name>\n" +
            "    <param-value>warcarpet</param-value>\n" +
            "  </context-param>\n" +
            "  <servlet>\n" +
            "    <servlet-name>echo</servlet-name>\n" +
            "    <servlet-class>com.example.web.EchoServlet</servlet-class>\n" +
            "    <init-param><param-name>greeting</param-name><param-value>hello-from-war</param-value></init-param>\n" +
            "    <load-on-startup>1</load-on-startup>\n" +
            "  </servlet>\n" +
            "  <servlet>\n" +
            "    <servlet-name>info</servlet-name>\n" +
            "    <servlet-class>com.example.web.InfoServlet</servlet-class>\n" +
            "    <load-on-startup>2</load-on-startup>\n" +
            "  </servlet>\n" +
            "  <servlet-mapping><servlet-name>echo</servlet-name><url-pattern>/echo</url-pattern></servlet-mapping>\n" +
            "  <servlet-mapping><servlet-name>echo</servlet-name><url-pattern>/echo2/*</url-pattern></servlet-mapping>\n" +
            "  <servlet-mapping><servlet-name>echo</servlet-name><url-pattern>*.do</url-pattern></servlet-mapping>\n" +
            "  <servlet-mapping><servlet-name>info</servlet-name><url-pattern>/info</url-pattern></servlet-mapping>\n" +
            "  <welcome-file-list><welcome-file>index.html</welcome-file></welcome-file-list>\n" +
            "</web-app>\n";
        Files.writeString(new File(WORK, "warroot/WEB-INF/web.xml").toPath(), webxml);

        String index = "<html><body>Welcome WarCarpet</body></html>";
        Files.writeString(new File(WORK, "warroot/index.html").toPath(), index);
    }

    static void compileServlets() throws IOException {
        JavaCompiler jc = ToolProvider.getSystemJavaCompiler();
        if (jc == null) throw new IllegalStateException("no system Java compiler (need a JDK)");
        String cp = System.getProperty("java.class.path");
        File classesDir = new File(WORK, "warroot/WEB-INF/classes");
        ByteArrayOutputStream errOut = new ByteArrayOutputStream();
        int rc = jc.run(null, null, errOut,
                "-cp", cp,
                "-d", classesDir.getAbsolutePath(),
                new File(WORK, "src/com/example/web/EchoServlet.java").getAbsolutePath(),
                new File(WORK, "src/com/example/web/InfoServlet.java").getAbsolutePath());
        eq("compile.rc", 0, rc);
        if (rc != 0) System.out.println("compiler-output:\n" + errOut.toString(StandardCharsets.UTF_8));
        check("compile.EchoServlet.class",
                new File(classesDir, "com/example/web/EchoServlet.class").isFile());
        check("compile.InfoServlet.class",
                new File(classesDir, "com/example/web/InfoServlet.class").isFile());
    }

    static void packageWar() throws IOException {
        File root = new File(WORK, "warroot");
        try (ZipOutputStream zos = new ZipOutputStream(
                new BufferedOutputStream(new FileOutputStream(WAR)))) {
            zipDir(root, root, zos);
        }
        check("war.exists", WAR.isFile());
        check("war.size>0", WAR.length() > 0);
    }

    static void zipDir(File base, File dir, ZipOutputStream zos) throws IOException {
        File[] kids = dir.listFiles();
        Arrays.sort(kids, Comparator.comparing(File::getName)); // deterministic order
        for (File f : kids) {
            String rel = base.toPath().relativize(f.toPath()).toString().replace(File.separatorChar, '/');
            if (f.isDirectory()) {
                zos.putNextEntry(new ZipEntry(rel + "/"));
                zos.closeEntry();
                zipDir(base, f, zos);
            } else {
                zos.putNextEntry(new ZipEntry(rel));
                zos.write(Files.readAllBytes(f.toPath()));
                zos.closeEntry();
            }
        }
    }

    // ================= war artifact validation =================
    static void validateWarArtifact() throws Exception {
        Set<String> entries = new HashSet<>();
        byte[] webxmlBytes = null;
        try (ZipFile zf = new ZipFile(WAR)) {
            Enumeration<? extends ZipEntry> en = zf.entries();
            while (en.hasMoreElements()) {
                ZipEntry e = en.nextElement();
                entries.add(e.getName());
                if (e.getName().equals("WEB-INF/web.xml")) {
                    webxmlBytes = zf.getInputStream(e).readAllBytes();
                }
            }
        }
        check("war.entry.webxml", entries.contains("WEB-INF/web.xml"));
        check("war.entry.echo.class", entries.contains("WEB-INF/classes/com/example/web/EchoServlet.class"));
        check("war.entry.info.class", entries.contains("WEB-INF/classes/com/example/web/InfoServlet.class"));
        check("war.entry.index", entries.contains("index.html"));
        check("war.entry.webinf.dir", entries.contains("WEB-INF/"));
        check("war.webxml.read", webxmlBytes != null && webxmlBytes.length > 0);

        // DOM parse the deployment descriptor from inside the war
        DocumentBuilderFactory dbf = DocumentBuilderFactory.newInstance();
        dbf.setNamespaceAware(false);
        Document doc = dbf.newDocumentBuilder().parse(new ByteArrayInputStream(webxmlBytes));
        Element rootEl = doc.getDocumentElement();
        eq("webxml.root", "web-app", rootEl.getNodeName());
        eq("webxml.version", "5.0", rootEl.getAttribute("version"));

        NodeList servlets = doc.getElementsByTagName("servlet");
        eq("webxml.servlet.count", 2, servlets.getLength());

        Map<String, Element> byName = new HashMap<>();
        for (int i = 0; i < servlets.getLength(); i++) {
            Element s = (Element) servlets.item(i);
            byName.put(childText(s, "servlet-name"), s);
        }
        check("webxml.servlet.echo.present", byName.containsKey("echo"));
        check("webxml.servlet.info.present", byName.containsKey("info"));
        eq("webxml.echo.class", "com.example.web.EchoServlet", childText(byName.get("echo"), "servlet-class"));
        eq("webxml.info.class", "com.example.web.InfoServlet", childText(byName.get("info"), "servlet-class"));
        eq("webxml.echo.loadOnStartup", "1", childText(byName.get("echo"), "load-on-startup"));

        // echo init-param
        Element ip = (Element) byName.get("echo").getElementsByTagName("init-param").item(0);
        eq("webxml.echo.initParam.name", "greeting", childText(ip, "param-name"));
        eq("webxml.echo.initParam.value", "hello-from-war", childText(ip, "param-value"));

        // context-param
        Element cp = (Element) doc.getElementsByTagName("context-param").item(0);
        eq("webxml.contextParam.name", "app.name", childText(cp, "param-name"));
        eq("webxml.contextParam.value", "warcarpet", childText(cp, "param-value"));

        // mappings
        NodeList maps = doc.getElementsByTagName("servlet-mapping");
        eq("webxml.mapping.count", 4, maps.getLength());
        Set<String> patterns = new HashSet<>();
        for (int i = 0; i < maps.getLength(); i++) {
            Element m = (Element) maps.item(i);
            patterns.add(childText(m, "url-pattern"));
        }
        check("webxml.map.exact", patterns.contains("/echo"));
        check("webxml.map.prefix", patterns.contains("/echo2/*"));
        check("webxml.map.extension", patterns.contains("*.do"));
        check("webxml.map.info", patterns.contains("/info"));

        // welcome
        Element wfl = (Element) doc.getElementsByTagName("welcome-file-list").item(0);
        eq("webxml.welcome", "index.html", childText(wfl, "welcome-file"));
    }

    static String childText(Element parent, String tag) {
        NodeList nl = parent.getElementsByTagName(tag);
        if (nl.getLength() == 0) return null;
        return nl.item(0).getTextContent().trim();
    }

    // ================= explode war (deployment) =================
    static void explodeWar() throws IOException {
        deleteRec(WEBAPP);
        WEBAPP.mkdirs();
        try (ZipFile zf = new ZipFile(WAR)) {
            Enumeration<? extends ZipEntry> en = zf.entries();
            while (en.hasMoreElements()) {
                ZipEntry e = en.nextElement();
                File out = new File(WEBAPP, e.getName());
                if (e.isDirectory()) { out.mkdirs(); continue; }
                out.getParentFile().mkdirs();
                try (InputStream in = zf.getInputStream(e)) {
                    Files.copy(in, out.toPath(), StandardCopyOption.REPLACE_EXISTING);
                }
            }
        }
        check("explode.webxml", new File(WEBAPP, "WEB-INF/web.xml").isFile());
        check("explode.echo.class", new File(WEBAPP, "WEB-INF/classes/com/example/web/EchoServlet.class").isFile());
        check("explode.index", new File(WEBAPP, "index.html").isFile());
    }

    // ================= deployer (mini servlet container) =================
    static final class ServletDef {
        String name;
        HttpServlet servlet;
        Map<String, String> initParams = new LinkedHashMap<>();
    }

    static final class Resolution {
        HttpServlet servlet;
        String servletPath;
        String pathInfo;
        Resolution(HttpServlet s, String sp, String pi) { servlet = s; servletPath = sp; pathInfo = pi; }
    }

    static final class Deployer {
        final File webappDir;
        final String contextPath;
        final Map<String, ServletDef> servlets = new LinkedHashMap<>();
        final Map<String, String> contextParams = new LinkedHashMap<>();
        final Map<String, Object> contextAttrs = new HashMap<>();
        // mapping tables
        final Map<String, String> exact = new HashMap<>();      // /echo -> echo
        final List<String[]> prefixes = new ArrayList<>();      // {"/echo2","echo"}
        final Map<String, String> extensions = new HashMap<>(); // .do -> echo
        final List<String> welcomeFiles = new ArrayList<>();
        ServletContext servletContext;
        URLClassLoader loader;
        int initCount = 0, destroyCount = 0;

        Deployer(File webappDir, String contextPath) {
            this.webappDir = webappDir;
            this.contextPath = contextPath;
        }

        void deploy() throws Exception {
            File classes = new File(webappDir, "WEB-INF/classes");
            loader = new URLClassLoader(new URL[]{ classes.toURI().toURL() },
                    WarCarpet.class.getClassLoader());

            DocumentBuilderFactory dbf = DocumentBuilderFactory.newInstance();
            dbf.setNamespaceAware(false);
            Document doc = dbf.newDocumentBuilder().parse(new File(webappDir, "WEB-INF/web.xml"));

            // context-params
            NodeList cps = doc.getElementsByTagName("context-param");
            for (int i = 0; i < cps.getLength(); i++) {
                Element e = (Element) cps.item(i);
                contextParams.put(childText(e, "param-name"), childText(e, "param-value"));
            }
            servletContext = makeServletContext();

            // welcome files
            NodeList wfls = doc.getElementsByTagName("welcome-file");
            for (int i = 0; i < wfls.getLength(); i++) welcomeFiles.add(wfls.item(i).getTextContent().trim());

            // servlet definitions (ordered by load-on-startup for realism)
            NodeList sl = doc.getElementsByTagName("servlet");
            List<Element> ordered = new ArrayList<>();
            for (int i = 0; i < sl.getLength(); i++) ordered.add((Element) sl.item(i));
            ordered.sort(Comparator.comparingInt(e -> {
                String los = childText(e, "load-on-startup");
                try { return los == null ? Integer.MAX_VALUE : Integer.parseInt(los); }
                catch (NumberFormatException nfe) { return Integer.MAX_VALUE; }
            }));
            for (Element e : ordered) {
                ServletDef def = new ServletDef();
                def.name = childText(e, "servlet-name");
                String cls = childText(e, "servlet-class");
                NodeList ips = e.getElementsByTagName("init-param");
                for (int j = 0; j < ips.getLength(); j++) {
                    Element ip = (Element) ips.item(j);
                    def.initParams.put(childText(ip, "param-name"), childText(ip, "param-value"));
                }
                Class<?> c = Class.forName(cls, true, loader);
                def.servlet = (HttpServlet) c.getDeclaredConstructor().newInstance();
                def.servlet.init(new Cfg(def.name, def.initParams, servletContext));
                initCount++;
                servlets.put(def.name, def);
            }

            // servlet-mappings
            NodeList ms = doc.getElementsByTagName("servlet-mapping");
            for (int i = 0; i < ms.getLength(); i++) {
                Element m = (Element) ms.item(i);
                String name = childText(m, "servlet-name");
                NodeList ups = m.getElementsByTagName("url-pattern");
                for (int j = 0; j < ups.getLength(); j++) {
                    String pat = ups.item(j).getTextContent().trim();
                    if (pat.startsWith("*.")) extensions.put(pat.substring(1), name);            // ".do"
                    else if (pat.endsWith("/*")) prefixes.add(new String[]{ pat.substring(0, pat.length() - 2), name });
                    else exact.put(pat, name);
                }
            }
            // longest prefix first
            prefixes.sort((a, b) -> Integer.compare(b[0].length(), a[0].length()));
        }

        // servlet-mapping resolution algorithm (exact > longest-prefix > extension)
        Resolution resolve(String inPath) {
            String name = exact.get(inPath);
            if (name != null) return new Resolution(servlets.get(name).servlet, inPath, null);
            for (String[] p : prefixes) {
                String pre = p[0];
                if (inPath.equals(pre)) return new Resolution(servlets.get(p[1]).servlet, pre, null);
                if (inPath.startsWith(pre + "/"))
                    return new Resolution(servlets.get(p[1]).servlet, pre, inPath.substring(pre.length()));
            }
            int dot = inPath.lastIndexOf('.');
            if (dot >= 0) {
                String ext = inPath.substring(dot);
                String n2 = extensions.get(ext);
                if (n2 != null) return new Resolution(servlets.get(n2).servlet, inPath, null);
            }
            return null;
        }

        void destroyAll() {
            for (ServletDef d : servlets.values()) { d.servlet.destroy(); destroyCount++; }
            try { loader.close(); } catch (IOException ignore) {}
        }

        ServletContext makeServletContext() {
            InvocationHandler h = (proxy, method, a) -> {
                switch (method.getName()) {
                    case "getInitParameter": return contextParams.get((String) a[0]);
                    case "getInitParameterNames": return Collections.enumeration(contextParams.keySet());
                    case "getServletContextName": return "WarCarpetApp";
                    case "getContextPath": return contextPath;
                    case "getAttribute": return contextAttrs.get((String) a[0]);
                    case "getAttributeNames": return Collections.enumeration(contextAttrs.keySet());
                    case "setAttribute": contextAttrs.put((String) a[0], a[1]); return null;
                    case "removeAttribute": contextAttrs.remove((String) a[0]); return null;
                    case "getMajorVersion": return 5;
                    case "getMinorVersion": return 0;
                    case "getEffectiveMajorVersion": return 5;
                    case "getEffectiveMinorVersion": return 0;
                    case "getServerInfo": return "WarCarpet/1.0";
                    case "getVirtualServerName": return "warcarpet";
                    case "log": return null;
                    case "toString": return "WarCarpetServletContext";
                    case "hashCode": return System.identityHashCode(proxy);
                    case "equals": return proxy == a[0];
                    default:
                        Class<?> rt = method.getReturnType();
                        if (rt == int.class) return 0;
                        if (rt == boolean.class) return Boolean.FALSE;
                        if (rt == long.class) return 0L;
                        return null;
                }
            };
            return (ServletContext) java.lang.reflect.Proxy.newProxyInstance(
                    WarCarpet.class.getClassLoader(),
                    new Class<?>[]{ ServletContext.class }, h);
        }
    }

    // ServletConfig backing each servlet
    static final class Cfg implements ServletConfig {
        final String name;
        final Map<String, String> ip;
        final ServletContext ctx;
        Cfg(String name, Map<String, String> ip, ServletContext ctx) { this.name = name; this.ip = ip; this.ctx = ctx; }
        public String getServletName() { return name; }
        public ServletContext getServletContext() { return ctx; }
        public String getInitParameter(String n) { return ip.get(n); }
        public Enumeration<String> getInitParameterNames() { return Collections.enumeration(ip.keySet()); }
    }

    // request facade so the servlet sees the mapping-derived path triplet
    static final class Facade extends HttpServletRequestWrapper {
        final String ctxPath, servletPath, pathInfo;
        Facade(HttpServletRequest r, String c, String s, String p) {
            super(r); ctxPath = c; servletPath = s; pathInfo = p;
        }
        @Override public String getContextPath() { return ctxPath; }
        @Override public String getServletPath() { return servletPath; }
        @Override public String getPathInfo() { return pathInfo; }
    }

    // Jetty handler that dispatches into the deployed war
    static final class Dispatcher extends AbstractHandler {
        final Deployer dep;
        final String ctxPath;
        final File webappDir;
        Dispatcher(Deployer dep, String ctxPath, File webappDir) {
            this.dep = dep; this.ctxPath = ctxPath; this.webappDir = webappDir;
        }
        @Override public void handle(String target, Request baseRequest,
                                     HttpServletRequest req, HttpServletResponse resp)
                throws IOException, ServletException {
            baseRequest.setHandled(true);
            String uri = req.getRequestURI();
            String inPath = uri.length() >= ctxPath.length() ? uri.substring(ctxPath.length()) : "/";
            if (inPath.isEmpty()) inPath = "/";

            if (inPath.equals("/")) { serveWelcome(resp); return; }

            Resolution r = dep.resolve(inPath);
            if (r == null) {
                resp.setStatus(404);
                resp.setContentType("text/plain");
                resp.getWriter().print("404|" + inPath);
                return;
            }
            HttpServletRequest wrapped = new Facade(req, ctxPath, r.servletPath, r.pathInfo);
            r.servlet.service(wrapped, resp);
        }

        void serveWelcome(HttpServletResponse resp) throws IOException {
            for (String wf : dep.welcomeFiles) {
                File f = new File(webappDir, wf);
                if (f.isFile()) {
                    resp.setStatus(200);
                    resp.setContentType("text/html");
                    resp.setCharacterEncoding("UTF-8");
                    resp.getOutputStream().write(Files.readAllBytes(f.toPath()));
                    return;
                }
            }
            resp.setStatus(404);
            resp.setContentType("text/plain");
            resp.getWriter().print("no-welcome");
        }
    }

    // ================= HTTP client + asserts =================
    static final class Resp {
        int code;
        String body;
        Map<String, List<String>> headers;
        String contentType;
        String header(String n) {
            if (headers == null) return null;
            for (Map.Entry<String, List<String>> e : headers.entrySet())
                if (e.getKey() != null && e.getKey().equalsIgnoreCase(n))
                    return e.getValue().isEmpty() ? null : e.getValue().get(0);
            return null;
        }
    }

    static Resp http(int port, String method, String path, Map<String, String> reqHeaders, byte[] body)
            throws IOException {
        URL u = new URL("http://127.0.0.1:" + port + path);
        HttpURLConnection c = (HttpURLConnection) u.openConnection();
        c.setInstanceFollowRedirects(false);
        c.setConnectTimeout(3000);
        c.setReadTimeout(3000);
        c.setRequestMethod(method);
        if (reqHeaders != null) for (Map.Entry<String, String> e : reqHeaders.entrySet())
            c.setRequestProperty(e.getKey(), e.getValue());
        if (body != null) {
            c.setDoOutput(true);
            try (OutputStream os = c.getOutputStream()) { os.write(body); }
        }
        Resp r = new Resp();
        r.code = c.getResponseCode();
        r.headers = c.getHeaderFields();
        r.contentType = c.getContentType();
        InputStream in = (r.code >= 400) ? c.getErrorStream() : c.getInputStream();
        ByteArrayOutputStream bo = new ByteArrayOutputStream();
        if (in != null) { in.transferTo(bo); in.close(); }
        r.body = bo.toString("UTF-8");
        c.disconnect();
        return r;
    }

    static void runHttpAsserts(int port) throws IOException {
        // ---- GET /app/echo?name=alice : exact mapping, init-param, headers, body ----
        Map<String, String> h1 = new HashMap<>();
        h1.put("X-Req", "foo");
        Resp g = http(port, "GET", "/app/echo?name=alice", h1, null);
        eq("echo.get.status", 200, g.code);
        contains("echo.get.ctype", g.contentType, "text/plain");
        eq("echo.get.X-Servlet", "echo", g.header("X-Servlet"));
        eq("echo.get.X-Init", "1", g.header("X-Init"));
        check("echo.get.cookie", g.header("Set-Cookie") != null && g.header("Set-Cookie").contains("warcookie"));
        check("echo.get.body.prefix", g.body.startsWith("GET|"));
        contains("echo.get.greeting", g.body, "greeting=hello-from-war");
        contains("echo.get.name", g.body, "name=alice");
        contains("echo.get.hdr", g.body, "hdr=foo");
        contains("echo.get.servletPath", g.body, "sp=/echo");
        contains("echo.get.pathInfo.null", g.body, "pi=null");
        contains("echo.get.qs", g.body, "qs=name=alice");
        contains("echo.get.proto", g.body, "proto=HTTP/1.1");
        contains("echo.get.method", g.body, "method=GET");
        contains("echo.get.ctx", g.body, "ctx=/app");

        // ---- GET no name -> name=null ----
        Resp gn = http(port, "GET", "/app/echo", null, null);
        eq("echo.get.noname.status", 200, gn.code);
        contains("echo.get.noname", gn.body, "name=null");
        contains("echo.get.noname.vals0", gn.body, "vals=0");

        // ---- multi-valued param ----
        Resp gm = http(port, "GET", "/app/echo?name=a&name=b", null, null);
        contains("echo.get.multi.vals2", gm.body, "vals=2");
        contains("echo.get.multi.first", gm.body, "name=a");

        // ---- POST body echo (doPost) ----
        Resp p = http(port, "POST", "/app/echo", null, "payload123".getBytes(StandardCharsets.UTF_8));
        eq("echo.post.status", 200, p.code);
        eq("echo.post.body", "POST|len=10|body=payload123", p.body);

        // ---- PUT body echo (doPut) ----
        Resp pu = http(port, "PUT", "/app/echo", null, "putbody".getBytes(StandardCharsets.UTF_8));
        eq("echo.put.status", 200, pu.code);
        eq("echo.put.body", "PUT|body=putbody", pu.body);

        // ---- DELETE (doDelete) -> 204, header, empty body ----
        Resp d = http(port, "DELETE", "/app/echo", null, null);
        eq("echo.delete.status", 204, d.code);
        eq("echo.delete.header", "true", d.header("X-Deleted"));
        eq("echo.delete.empty", "", d.body);

        // ---- prefix mapping /echo2/* : servletPath + pathInfo ----
        Resp pf = http(port, "GET", "/app/echo2/sub/path", null, null);
        eq("echo.prefix.status", 200, pf.code);
        contains("echo.prefix.sp", pf.body, "sp=/echo2");
        contains("echo.prefix.pi", pf.body, "pi=/sub/path");

        Resp pf0 = http(port, "GET", "/app/echo2", null, null);
        eq("echo.prefix0.status", 200, pf0.code);
        contains("echo.prefix0.sp", pf0.body, "sp=/echo2");
        contains("echo.prefix0.pi.null", pf0.body, "pi=null");

        // ---- extension mapping *.do -> echo ----
        Resp ex = http(port, "GET", "/app/report.do", null, null);
        eq("echo.ext.status", 200, ex.code);
        contains("echo.ext.sp", ex.body, "sp=/report.do");

        // ---- redirect (sendRedirect -> 302 + Location) ----
        Resp rd = http(port, "GET", "/app/echo?mode=redirect", null, null);
        eq("echo.redirect.status", 302, rd.code);
        check("echo.redirect.location", rd.header("Location") != null && rd.header("Location").contains("/app/info"));

        // ---- OPTIONS -> Allow header lists implemented methods ----
        Resp op = http(port, "OPTIONS", "/app/echo", null, null);
        eq("echo.options.status", 200, op.code);
        String allow = op.header("Allow");
        check("echo.options.allow.get", allow != null && allow.contains("GET"));
        check("echo.options.allow.post", allow != null && allow.contains("POST"));
        check("echo.options.allow.put", allow != null && allow.contains("PUT"));
        check("echo.options.allow.delete", allow != null && allow.contains("DELETE"));

        // ---- HEAD -> 200, no body ----
        Resp hd = http(port, "HEAD", "/app/echo", null, null);
        eq("echo.head.status", 200, hd.code);
        eq("echo.head.empty", "", hd.body);

        // ---- info servlet : context-param + servlet-name, text/html ----
        Resp inf = http(port, "GET", "/app/info", null, null);
        eq("info.get.status", 200, inf.code);
        contains("info.get.ctype", inf.contentType, "text/html");
        contains("info.get.appName", inf.body, "app=warcarpet");
        contains("info.get.servletName", inf.body, "servlet=info");

        // ---- 405 : POST to info (only doGet implemented) ----
        Resp i405 = http(port, "POST", "/app/info", null, new byte[0]);
        eq("info.post.405", 405, i405.code);

        // ---- welcome file (context root, both forms) ----
        Resp w1 = http(port, "GET", "/app/", null, null);
        eq("welcome.slash.status", 200, w1.code);
        contains("welcome.slash.ctype", w1.contentType, "text/html");
        contains("welcome.slash.body", w1.body, "Welcome WarCarpet");

        Resp w2 = http(port, "GET", "/app", null, null);
        eq("welcome.noslash.status", 200, w2.code);
        contains("welcome.noslash.body", w2.body, "Welcome WarCarpet");

        // ---- 404 inside context (no mapping) ----
        Resp nf = http(port, "GET", "/app/does-not-exist", null, null);
        eq("notfound.inctx.status", 404, nf.code);
        contains("notfound.inctx.body", nf.body, "404|/does-not-exist");

        // ---- 404 outside context ----
        Resp out = http(port, "GET", "/elsewhere", null, null);
        eq("notfound.outctx.status", 404, out.code);

        // ---- two distinct servlets dispatched on distinct mappings ----
        check("dispatch.distinct", g.body.startsWith("GET|") && inf.body.contains("<info"));
    }

    static boolean portFreeAfterStop(int port) {
        for (int i = 0; i < 30; i++) {
            try (ServerSocket s = new ServerSocket()) {
                s.setReuseAddress(true);
                s.bind(new InetSocketAddress("127.0.0.1", port));
                return true;
            } catch (IOException e) {
                try { Thread.sleep(100); } catch (InterruptedException ie) { return false; }
            }
        }
        return false;
    }
}
