import java.net.*;
import java.io.*;
import java.util.*;
import java.nio.charset.StandardCharsets;

/* DoD-B carpet for the java.net module on StarryOS (#764).
 * Deterministic + offline: every datum is self-fabricated, all I/O is 127.0.0.1
 * loopback with bounded timeouts and deterministic teardown. Assertions are
 * exact (equals / == / fixed values), never "ran without throwing".
 *
 * Coverage matrix (java.net):
 *   - InetAddress / Inet4Address / Inet6Address: literal parsing, address-class
 *     predicates (loopback/anylocal/multicast/link-local/site-local), byte
 *     round-trip, getByAddress, getAllByName, equals/hashCode, length errors.
 *   - InetSocketAddress: host+port / wildcard / createUnresolved, accessors,
 *     equals/hashCode, port-range IllegalArgumentException.
 *   - URI: full component parse, opaque vs hierarchical, resolve / relativize /
 *     normalize, raw vs decoded path, multi-arg encoding ctor, URISyntaxException.
 *   - URL: protocol/host/port/path/query/ref/authority/userinfo, default ports
 *     per scheme, context ctor, toExternalForm, sameFile, toURI, openConnection
 *     (object only, no connect), MalformedURLException.
 *   - URLEncoder / URLDecoder: space/plus/percent/UTF-8 multibyte round-trips.
 *   - IDN: ASCII passthrough + punycode toASCII / toUnicode.
 *   - HttpCookie / CookieManager / CookiePolicy / CookieStore.
 *   - Proxy / Proxy.Type.
 *   - StandardSocketOptions constant descriptors.
 *   - ServerSocket + Socket loopback echo: state machine, options, shutdown,
 *     getOption/setOption, accept SocketTimeoutException.
 *   - DatagramSocket + DatagramPacket loopback: send/receive, packet accessors,
 *     receive SocketTimeoutException.
 *   - NetworkInterface / InterfaceAddress: enumeration (drives SIOCGIFCONF),
 *     loopback lookup, addresses, index, MTU.
 */
public class NetTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String n) { if (c) ok++; else { fail++; System.out.println("FAIL " + n); } }
    static void eq(Object a, Object b, String n) { check(a == null ? b == null : a.equals(b), n); }

    public static void main(String[] args) {
        inetAddress();
        inet6();
        socketAddress();
        uri();
        url();
        encoder();
        idn();
        cookies();
        proxy();
        socketOptions();
        tcpLoopback();
        udpLoopback();
        networkInterface();

        System.out.println("NET_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("NET_DONE");
    }

    // ---- InetAddress / Inet4Address: literal parsing never hits DNS ----
    static void inetAddress() {
        try {
            InetAddress lo = InetAddress.getByName("127.0.0.1");
            check(lo instanceof Inet4Address, "ina-inet4");
            eq(lo.getHostAddress(), "127.0.0.1", "ina-hostaddr");
            check(lo.isLoopbackAddress(), "ina-loopback");
            check(Arrays.equals(lo.getAddress(), new byte[]{127,0,0,1}), "ina-bytes");

            InetAddress byBytes = InetAddress.getByAddress(new byte[]{10,1,2,3});
            eq(byBytes.getHostAddress(), "10.1.2.3", "ina-bybytes");

            check(InetAddress.getByName("0.0.0.0").isAnyLocalAddress(), "ina-anylocal");
            check(InetAddress.getByName("224.0.0.1").isMulticastAddress(), "ina-multicast");
            check(!InetAddress.getByName("127.0.0.1").isMulticastAddress(), "ina-not-multicast");
            check(InetAddress.getByName("169.254.7.9").isLinkLocalAddress(), "ina-linklocal");
            check(InetAddress.getByName("192.168.1.1").isSiteLocalAddress(), "ina-sitelocal-192");
            check(InetAddress.getByName("10.0.0.5").isSiteLocalAddress(), "ina-sitelocal-10");
            check(!InetAddress.getByName("8.8.8.8").isSiteLocalAddress(), "ina-public-not-site");

            InetAddress a = InetAddress.getByName("127.0.0.1");
            InetAddress b = InetAddress.getByName("127.0.0.1");
            check(a.equals(b), "ina-equals");
            check(a.hashCode() == b.hashCode(), "ina-hashcode");

            InetAddress loopCanon = InetAddress.getLoopbackAddress();
            check(loopCanon.isLoopbackAddress(), "ina-getloopback");

            InetAddress[] all = InetAddress.getAllByName("127.0.0.1");
            check(all.length >= 1 && all[0].isLoopbackAddress(), "ina-getallbyname");

            // getByAddress with an illegal length must fail deterministically (no DNS).
            try { InetAddress.getByAddress(new byte[5]); fail++; System.out.println("FAIL ina-badlen-noexc"); }
            catch (UnknownHostException ex) { ok++; }
        } catch (Exception e) { fail++; System.out.println("FAIL inetAddress-exc " + e); }
    }

    // ---- Inet6Address ----
    static void inet6() {
        try {
            InetAddress v6 = InetAddress.getByName("::1");
            check(v6 instanceof Inet6Address, "in6-inet6");
            check(v6.isLoopbackAddress(), "in6-loopback");
            check(v6.getAddress().length == 16, "in6-len16");

            check(InetAddress.getByName("fe80::1").isLinkLocalAddress(), "in6-linklocal");
            check(InetAddress.getByName("ff02::1").isMulticastAddress(), "in6-multicast");

            byte[] raw = new byte[16];
            raw[15] = 1;
            Inet6Address scoped = Inet6Address.getByAddress("h", raw, 2);
            check(scoped.getScopeId() == 2, "in6-scopeid");

            InetAddress doc = InetAddress.getByName("2001:db8::1");
            check(doc.getHostAddress().startsWith("2001:db8"), "in6-hostaddr");
        } catch (Exception e) { fail++; System.out.println("FAIL inet6-exc " + e); }
    }

    // ---- InetSocketAddress ----
    static void socketAddress() {
        try {
            InetSocketAddress s = new InetSocketAddress("127.0.0.1", 8080);
            check(s.getPort() == 8080, "isa-port");
            check(s.getAddress().isLoopbackAddress(), "isa-addr-loopback");
            eq(s.getHostString(), "127.0.0.1", "isa-hoststring");
            check(!s.isUnresolved(), "isa-resolved");

            InetSocketAddress wild = new InetSocketAddress(0);
            check(wild.getAddress().isAnyLocalAddress(), "isa-wildcard");

            InetSocketAddress un = InetSocketAddress.createUnresolved("example.test", 80);
            check(un.isUnresolved(), "isa-unresolved");
            eq(un.getHostName(), "example.test", "isa-unresolved-host");
            check(un.getAddress() == null, "isa-unresolved-noaddr");
            check(un.getPort() == 80, "isa-unresolved-port");

            InetSocketAddress s2 = new InetSocketAddress("127.0.0.1", 8080);
            check(s.equals(s2), "isa-equals");
            check(s.hashCode() == s2.hashCode(), "isa-hashcode");

            try { new InetSocketAddress("127.0.0.1", 70000); fail++; System.out.println("FAIL isa-badport-noexc"); }
            catch (IllegalArgumentException ex) { ok++; }
        } catch (Exception e) { fail++; System.out.println("FAIL socketAddress-exc " + e); }
    }

    // ---- URI ----
    static void uri() {
        try {
            URI u = new URI("http://user@host:8080/path?q=1#frag");
            eq(u.getScheme(), "http", "uri-scheme");
            eq(u.getHost(), "host", "uri-host");
            check(u.getPort() == 8080, "uri-port");
            eq(u.getPath(), "/path", "uri-path");
            eq(u.getQuery(), "q=1", "uri-query");
            eq(u.getFragment(), "frag", "uri-fragment");
            eq(u.getUserInfo(), "user", "uri-userinfo");
            eq(u.getAuthority(), "user@host:8080", "uri-authority");
            check(u.isAbsolute(), "uri-absolute");
            check(!u.isOpaque(), "uri-not-opaque");

            URI mail = URI.create("mailto:a@b.test");
            check(mail.isOpaque(), "uri-opaque");
            eq(mail.getScheme(), "mailto", "uri-opaque-scheme");
            eq(mail.getSchemeSpecificPart(), "a@b.test", "uri-opaque-ssp");

            URI base = new URI("http://h/a/b/c");
            eq(base.resolve("d").toString(), "http://h/a/b/d", "uri-resolve-rel");
            eq(base.resolve("/d").toString(), "http://h/d", "uri-resolve-abs");
            eq(base.relativize(new URI("http://h/a/b/c/d")).toString(), "d", "uri-relativize");
            eq(new URI("http://h/a/./b/../c").normalize().getPath(), "/a/c", "uri-normalize");

            URI enc = new URI("http://h/a%20b");
            eq(enc.getRawPath(), "/a%20b", "uri-rawpath");
            eq(enc.getPath(), "/a b", "uri-decodedpath");

            URI multi = new URI("http", "host", "/p", "f");
            eq(multi.toString(), "http://host/p#f", "uri-multiarg-ctor");

            URI x = new URI("http://h/p");
            check(x.equals(new URI("http://h/p")), "uri-equals");
            check(x.hashCode() == new URI("http://h/p").hashCode(), "uri-hashcode");
            check(x.compareTo(new URI("http://h/p")) == 0, "uri-compareto");

            try { new URI("a b"); fail++; System.out.println("FAIL uri-syntax-noexc"); }
            catch (URISyntaxException ex) { ok++; }
        } catch (Exception e) { fail++; System.out.println("FAIL uri-exc " + e); }
    }

    // ---- URL (object construction only; never connects) ----
    static void url() {
        try {
            URL u = new URL("http://user:pwd@host:8080/p?x=1#f");
            eq(u.getProtocol(), "http", "url-protocol");
            eq(u.getHost(), "host", "url-host");
            check(u.getPort() == 8080, "url-port");
            check(u.getDefaultPort() == 80, "url-defaultport-http");
            eq(u.getPath(), "/p", "url-path");
            eq(u.getQuery(), "x=1", "url-query");
            eq(u.getRef(), "f", "url-ref");
            eq(u.getFile(), "/p?x=1", "url-file");
            eq(u.getUserInfo(), "user:pwd", "url-userinfo");
            eq(u.getAuthority(), "user:pwd@host:8080", "url-authority");

            check(new URL("https://h/").getDefaultPort() == 443, "url-defaultport-https");
            check(new URL("ftp://h/").getDefaultPort() == 21, "url-defaultport-ftp");

            URL base = new URL("http://h/dir/page.html");
            eq(new URL(base, "other.html").toString(), "http://h/dir/other.html", "url-context-ctor");
            check(base.sameFile(new URL("http://h/dir/page.html")), "url-samefile");

            URL ext = new URL("http://h:80/q");
            eq(ext.toExternalForm(), "http://h:80/q", "url-externalform");

            URI back = new URL("http://h/p?x=1").toURI();
            eq(back.getScheme(), "http", "url-touri");

            // openConnection builds a handler object only — no socket is opened.
            URLConnection conn = new URL("http://127.0.0.1/none").openConnection();
            check(conn instanceof HttpURLConnection, "url-openconn-http");
            check(conn.getReadTimeout() == 0, "url-readtimeout-default");
            ((HttpURLConnection) conn).setRequestMethod("HEAD");
            eq(((HttpURLConnection) conn).getRequestMethod(), "HEAD", "url-requestmethod");
            check(HttpURLConnection.HTTP_OK == 200, "url-http-ok-const");
            check(HttpURLConnection.HTTP_NOT_FOUND == 404, "url-http-404-const");

            try { new URL("zzunknown://x"); fail++; System.out.println("FAIL url-malformed-noexc"); }
            catch (MalformedURLException ex) { ok++; }
        } catch (Exception e) { fail++; System.out.println("FAIL url-exc " + e); }
    }

    // ---- URLEncoder / URLDecoder ----
    static void encoder() {
        try {
            eq(URLEncoder.encode("a b", "UTF-8"), "a+b", "enc-space");
            eq(URLEncoder.encode("a+b", "UTF-8"), "a%2Bb", "enc-plus");
            eq(URLEncoder.encode("100%", "UTF-8"), "100%25", "enc-percent");
            eq(URLEncoder.encode("ä", "UTF-8"), "%C3%A4", "enc-utf8");
            eq(URLDecoder.decode("a+b", "UTF-8"), "a b", "dec-plus");
            eq(URLDecoder.decode("%C3%A4", "UTF-8"), "ä", "dec-utf8");
            String round = "key=v a&l/ué";
            eq(URLDecoder.decode(URLEncoder.encode(round, "UTF-8"), "UTF-8"), round, "enc-roundtrip");
            // overload taking a Charset
            eq(URLEncoder.encode("x y", StandardCharsets.UTF_8), "x+y", "enc-charset-overload");
        } catch (Exception e) { fail++; System.out.println("FAIL encoder-exc " + e); }
    }

    // ---- IDN punycode ----
    static void idn() {
        try {
            eq(IDN.toASCII("example.com"), "example.com", "idn-ascii-passthrough");
            eq(IDN.toASCII("bücher.de"), "xn--bcher-kva.de", "idn-toascii-puny");
            eq(IDN.toUnicode("xn--bcher-kva.de"), "bücher.de", "idn-tounicode-puny");
        } catch (Exception e) { fail++; System.out.println("FAIL idn-exc " + e); }
    }

    // ---- HttpCookie / CookieManager / CookiePolicy / CookieStore ----
    static void cookies() {
        try {
            List<HttpCookie> simple = HttpCookie.parse("name=value");
            check(simple.size() == 1, "cookie-parse-size");
            HttpCookie c0 = simple.get(0);
            eq(c0.getName(), "name", "cookie-name");
            eq(c0.getValue(), "value", "cookie-value");
            check(c0.getVersion() == 0, "cookie-version-default");

            List<HttpCookie> attr = HttpCookie.parse("a=b; Domain=example.test; Path=/foo; Max-Age=3600");
            HttpCookie c1 = attr.get(0);
            eq(c1.getDomain(), "example.test", "cookie-domain");
            eq(c1.getPath(), "/foo", "cookie-path");
            check(c1.getMaxAge() == 3600, "cookie-maxage");
            check(HttpCookie.domainMatches("example.test", "example.test"), "cookie-domainmatch");

            HttpCookie made = new HttpCookie("k", "v");
            made.setSecure(true);
            check(made.getSecure(), "cookie-secure");
            eq(made, new HttpCookie("k", "anything"), "cookie-equals-by-name");

            check(CookiePolicy.ACCEPT_ALL != null, "cookiepolicy-acceptall");
            check(CookiePolicy.ACCEPT_NONE != null, "cookiepolicy-acceptnone");
            check(CookiePolicy.ACCEPT_ORIGINAL_SERVER != null, "cookiepolicy-original");

            CookieManager mgr = new CookieManager();
            CookieStore store = mgr.getCookieStore();
            check(store != null, "cookiemanager-store");
            store.add(URI.create("http://example.test/"), new HttpCookie("sid", "42"));
            check(store.getCookies().size() == 1, "cookiestore-add");
            eq(store.getCookies().get(0).getValue(), "42", "cookiestore-getvalue");
        } catch (Exception e) { fail++; System.out.println("FAIL cookies-exc " + e); }
    }

    // ---- Proxy ----
    static void proxy() {
        try {
            check(Proxy.NO_PROXY.type() == Proxy.Type.DIRECT, "proxy-noproxy-direct");
            InetSocketAddress pa = new InetSocketAddress("127.0.0.1", 3128);
            Proxy p = new Proxy(Proxy.Type.HTTP, pa);
            check(p.type() == Proxy.Type.HTTP, "proxy-type-http");
            check(p.address().equals(pa), "proxy-address");
            check(Proxy.Type.values().length == 3, "proxy-type-count");
            check(Proxy.Type.valueOf("SOCKS") == Proxy.Type.SOCKS, "proxy-type-socks");
        } catch (Exception e) { fail++; System.out.println("FAIL proxy-exc " + e); }
    }

    // ---- StandardSocketOptions descriptors ----
    static void socketOptions() {
        try {
            eq(StandardSocketOptions.SO_REUSEADDR.name(), "SO_REUSEADDR", "opt-reuseaddr-name");
            check(StandardSocketOptions.SO_REUSEADDR.type() == Boolean.class, "opt-reuseaddr-type");
            eq(StandardSocketOptions.TCP_NODELAY.name(), "TCP_NODELAY", "opt-nodelay-name");
            check(StandardSocketOptions.TCP_NODELAY.type() == Boolean.class, "opt-nodelay-type");
            check(StandardSocketOptions.SO_RCVBUF.type() == Integer.class, "opt-rcvbuf-type");
            check(StandardSocketOptions.SO_SNDBUF.type() == Integer.class, "opt-sndbuf-type");
        } catch (Exception e) { fail++; System.out.println("FAIL socketOptions-exc " + e); }
    }

    // ---- TCP loopback echo: full Socket/ServerSocket state machine ----
    static void tcpLoopback() {
        ServerSocket ss = null;
        Thread srv = null;
        try {
            ss = new ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"));
            ss.setSoTimeout(4000);
            final ServerSocket fss = ss;
            check(ss.isBound(), "tcp-srv-bound");
            check(!ss.isClosed(), "tcp-srv-open");
            check(ss.getLocalPort() > 0, "tcp-srv-port");
            check(ss.getReceiveBufferSize() > 0, "tcp-srv-rcvbuf");
            int port = ss.getLocalPort();

            srv = new Thread(() -> {
                try (Socket s = fss.accept()) {
                    BufferedReader r = new BufferedReader(new InputStreamReader(s.getInputStream()));
                    PrintWriter w = new PrintWriter(s.getOutputStream(), true);
                    w.println("echo:" + r.readLine());
                } catch (Exception ignored) {}
            });
            srv.start();

            try (Socket c = new Socket()) {
                c.connect(new InetSocketAddress("127.0.0.1", port), 4000);
                check(c.isConnected(), "tcp-cli-connected");
                check(c.isBound(), "tcp-cli-bound");
                check(!c.isClosed(), "tcp-cli-open");

                InetSocketAddress remote = (InetSocketAddress) c.getRemoteSocketAddress();
                check(remote.getPort() == port, "tcp-cli-remoteport");
                check(remote.getAddress().isLoopbackAddress(), "tcp-cli-remote-loopback");
                check(((InetSocketAddress) c.getLocalSocketAddress()).getAddress().isLoopbackAddress(), "tcp-cli-local-loopback");

                c.setTcpNoDelay(true);
                check(c.getTcpNoDelay(), "tcp-cli-nodelay");
                c.setSoTimeout(4000);
                check(c.getSoTimeout() == 4000, "tcp-cli-sotimeout");
                c.setKeepAlive(true);
                check(c.getKeepAlive(), "tcp-cli-keepalive");
                c.setReuseAddress(true);
                check(c.getReuseAddress(), "tcp-cli-reuseaddr");
                check(c.getSendBufferSize() > 0, "tcp-cli-sndbuf");

                // NIO-era getOption / setOption path.
                c.setOption(StandardSocketOptions.TCP_NODELAY, Boolean.TRUE);
                check(c.getOption(StandardSocketOptions.TCP_NODELAY), "tcp-cli-getoption");

                PrintWriter w = new PrintWriter(c.getOutputStream(), true);
                w.println("hello");
                String resp = new BufferedReader(new InputStreamReader(c.getInputStream())).readLine();
                eq(resp, "echo:hello", "tcp-echo");

                c.shutdownOutput();
                check(c.isOutputShutdown(), "tcp-cli-outshutdown");
            }
            srv.join(4000);

            // accept() on a fresh idle server with a short timeout -> SocketTimeoutException.
            try (ServerSocket idle = new ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))) {
                idle.setSoTimeout(250);
                try { idle.accept(); fail++; System.out.println("FAIL tcp-accept-timeout-noexc"); }
                catch (SocketTimeoutException ex) { ok++; }
            }
        } catch (Exception e) { fail++; System.out.println("FAIL tcpLoopback-exc " + e); }
        finally {
            try { if (ss != null) ss.close(); } catch (Exception ignored) {}
            check(ss != null && ss.isClosed(), "tcp-srv-closed");
        }
    }

    // ---- UDP loopback datagram ----
    static void udpLoopback() {
        try (DatagramSocket srv = new DatagramSocket(0, InetAddress.getByName("127.0.0.1"));
             DatagramSocket cli = new DatagramSocket()) {
            check(srv.getLocalPort() > 0, "udp-srv-port");
            check(srv.isBound(), "udp-srv-bound");
            srv.setSoTimeout(4000);
            check(srv.getSoTimeout() == 4000, "udp-srv-sotimeout");
            int port = srv.getLocalPort();

            byte[] msg = "ping".getBytes(StandardCharsets.UTF_8);
            DatagramPacket out = new DatagramPacket(msg, msg.length, InetAddress.getByName("127.0.0.1"), port);
            check(out.getPort() == port, "udp-pkt-port");
            check(out.getLength() == 4, "udp-pkt-len");
            cli.send(out);

            byte[] buf = new byte[32];
            DatagramPacket in = new DatagramPacket(buf, buf.length);
            srv.receive(in);
            eq(new String(in.getData(), in.getOffset(), in.getLength(), StandardCharsets.UTF_8), "ping", "udp-echo");
            check(in.getLength() == 4, "udp-recv-len");
            check(in.getAddress().isLoopbackAddress(), "udp-recv-addr");
            check(in.getPort() > 0, "udp-recv-port");

            // setData / setLength / getOffset on a fresh packet.
            DatagramPacket p = new DatagramPacket(new byte[8], 8);
            byte[] nb = "abcd".getBytes(StandardCharsets.UTF_8);
            p.setData(nb);
            p.setLength(3);
            check(p.getLength() == 3, "udp-pkt-setlen");
            check(p.getOffset() == 0, "udp-pkt-offset");
            check(p.getData() == nb, "udp-pkt-setdata");

            // receive with no sender + short timeout -> SocketTimeoutException.
            try (DatagramSocket idle = new DatagramSocket(0, InetAddress.getByName("127.0.0.1"))) {
                idle.setSoTimeout(250);
                try { idle.receive(new DatagramPacket(new byte[8], 8)); fail++; System.out.println("FAIL udp-timeout-noexc"); }
                catch (SocketTimeoutException ex) { ok++; }
            }
        } catch (Exception e) { fail++; System.out.println("FAIL udpLoopback-exc " + e); }
    }

    // ---- NetworkInterface / InterfaceAddress (drives SIOCGIFCONF) ----
    static void networkInterface() {
        try {
            List<NetworkInterface> nis = Collections.list(NetworkInterface.getNetworkInterfaces());
            check(nis.size() >= 1, "nif-enumerate");

            NetworkInterface loop = null;
            for (NetworkInterface ni : nis) {
                if (ni.isLoopback()) { loop = ni; break; }
            }
            check(loop != null, "nif-has-loopback");
            if (loop != null) {
                check(loop.isUp(), "nif-loopback-up");
                check(loop.getName() != null, "nif-name");
                check(loop.getIndex() >= 0, "nif-index");
                check(loop.getMTU() > 0, "nif-mtu");

                NetworkInterface byName = NetworkInterface.getByName(loop.getName());
                check(byName != null && byName.getName().equals(loop.getName()), "nif-byname");

                NetworkInterface byIdx = NetworkInterface.getByIndex(loop.getIndex());
                check(byIdx != null, "nif-byindex");

                boolean hasLoopAddr = false;
                for (InetAddress a : Collections.list(loop.getInetAddresses())) {
                    if (a.isLoopbackAddress()) { hasLoopAddr = true; break; }
                }
                check(hasLoopAddr, "nif-loopback-addr");

                NetworkInterface byAddr = NetworkInterface.getByInetAddress(InetAddress.getByName("127.0.0.1"));
                check(byAddr != null && byAddr.isLoopback(), "nif-byinetaddr");

                List<InterfaceAddress> ias = loop.getInterfaceAddresses();
                check(ias.size() >= 1, "nif-ifaceaddrs");
                boolean prefixOk = false;
                for (InterfaceAddress ia : ias) {
                    short plen = ia.getNetworkPrefixLength();
                    if (ia.getAddress() != null && plen >= 0 && plen <= 128) { prefixOk = true; break; }
                }
                check(prefixOk, "nif-prefixlen");
            }
        } catch (Exception e) { fail++; System.out.println("FAIL networkInterface-exc " + e); }
    }
}
