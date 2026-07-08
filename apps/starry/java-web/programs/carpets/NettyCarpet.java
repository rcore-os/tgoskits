package org.starry.dod;

import io.netty.bootstrap.Bootstrap;
import io.netty.bootstrap.ServerBootstrap;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.ByteBufUtil;
import io.netty.buffer.PooledByteBufAllocator;
import io.netty.buffer.Unpooled;
import io.netty.buffer.UnpooledByteBufAllocator;
import io.netty.channel.*;
import io.netty.channel.embedded.EmbeddedChannel;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.SocketChannel;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import io.netty.channel.socket.nio.NioSocketChannel;
import io.netty.handler.codec.ByteToMessageDecoder;
import io.netty.handler.codec.DelimiterBasedFrameDecoder;
import io.netty.handler.codec.Delimiters;
import io.netty.handler.codec.FixedLengthFrameDecoder;
import io.netty.handler.codec.LengthFieldBasedFrameDecoder;
import io.netty.handler.codec.LengthFieldPrepender;
import io.netty.handler.codec.LineBasedFrameDecoder;
import io.netty.handler.codec.MessageToMessageEncoder;
import io.netty.handler.codec.string.StringDecoder;
import io.netty.handler.codec.string.StringEncoder;
import io.netty.util.Attribute;
import io.netty.util.AttributeKey;
import io.netty.util.CharsetUtil;
import io.netty.util.ReferenceCountUtil;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/* Carpet-level coverage of Netty 4.x: ByteBuf (alloc / read-write / pooled / refcount / slice /
 * compose), EmbeddedChannel-driven codec/handler unit tests (LineBased / DelimiterBased /
 * FixedLength / LengthField(+Prepender) / String enc-dec / custom ByteToMessage /
 * MessageToMessage), pipeline manipulation, ChannelFuture / Promise, AttributeKey, and two real
 * loopback integrations (a TCP echo server + an HTTP-codec server). Deterministic + offline. */
public class NettyCarpet {
    static int ok = 0, fail = 0;
    static void chk(boolean c, String n) { if (c) ok++; else { fail++; System.out.println("FAIL " + n); } }

    public static void main(String[] args) throws Exception {
        byteBuf();
        codecsViaEmbedded();
        pipelineAndHandlers();
        futuresAndAttributes();
        tcpEchoLoopback();
        httpCodecLoopback();

        System.out.println("NETTY_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("NETTY_DONE");
    }

    // ---------------------------------------------------------------- ByteBuf
    static void byteBuf() {
        ByteBuf b = Unpooled.buffer(16);
        chk(b.capacity() >= 16 && b.readableBytes() == 0 && b.writableBytes() >= 16, "bytebuf-init");
        b.writeInt(0x01020304).writeByte(0xFF);
        chk(b.readableBytes() == 5 && b.writerIndex() == 5, "bytebuf-write-index");
        chk(b.readInt() == 0x01020304, "bytebuf-readInt");
        chk((b.readByte() & 0xFF) == 0xFF, "bytebuf-readByte");
        chk(b.readableBytes() == 0, "bytebuf-drained");
        b.clear();
        chk(b.readerIndex() == 0 && b.writerIndex() == 0, "bytebuf-clear");

        ByteBuf s = Unpooled.copiedBuffer("hello netty", CharsetUtil.UTF_8);
        chk(s.toString(CharsetUtil.UTF_8).equals("hello netty"), "bytebuf-copiedBuffer-string");
        chk(s.indexOf(0, s.writerIndex(), (byte) 'n') == 6, "bytebuf-indexOf");
        ByteBuf slice = s.slice(0, 5);
        chk(slice.toString(CharsetUtil.UTF_8).equals("hello"), "bytebuf-slice");
        ByteBuf dup = s.duplicate();
        chk(dup.readableBytes() == s.readableBytes(), "bytebuf-duplicate");

        ByteBuf composite = Unpooled.wrappedBuffer(
                Unpooled.copiedBuffer("ab", CharsetUtil.UTF_8),
                Unpooled.copiedBuffer("cd", CharsetUtil.UTF_8));
        chk(composite.toString(CharsetUtil.UTF_8).equals("abcd"), "bytebuf-wrapped-composite");

        chk(ByteBufUtil.hexDump(Unpooled.wrappedBuffer(new byte[]{0x0A, (byte) 0xFF})).equals("0aff"), "bytebufutil-hexdump");

        ByteBuf pooled = PooledByteBufAllocator.DEFAULT.buffer(8);
        chk(pooled.refCnt() == 1, "pooled-refcnt-init");
        pooled.retain();
        chk(pooled.refCnt() == 2, "pooled-retain");
        pooled.release();
        chk(pooled.refCnt() == 1, "pooled-release");
        chk(pooled.release(), "pooled-release-final");
        chk(pooled.refCnt() == 0, "pooled-refcnt-zero");

        ByteBuf un = UnpooledByteBufAllocator.DEFAULT.heapBuffer(4);
        chk(un.hasArray(), "unpooled-heap-hasArray");
        un.release();

        ByteBuf order = Unpooled.buffer(8);
        order.writeShort(0x1234);
        chk(order.getUnsignedShort(0) == 0x1234, "bytebuf-unsigned-short");
        order.release();
        b.release(); s.release(); composite.release();
    }

    // ---------------------------------------------------------------- codecs (EmbeddedChannel)
    static void codecsViaEmbedded() {
        // LineBasedFrameDecoder
        EmbeddedChannel line = new EmbeddedChannel(new LineBasedFrameDecoder(64), new StringDecoder(CharsetUtil.UTF_8));
        chk(line.writeInbound(Unpooled.copiedBuffer("one\ntwo\n", CharsetUtil.UTF_8)), "line-write");
        chk("one".equals(line.readInbound()), "line-frame-1");
        chk("two".equals(line.readInbound()), "line-frame-2");
        chk(line.readInbound() == null, "line-frame-none");
        chk(!line.finish(), "line-finish");

        // DelimiterBasedFrameDecoder
        EmbeddedChannel delim = new EmbeddedChannel(
                new DelimiterBasedFrameDecoder(64, Delimiters.lineDelimiter()), new StringDecoder(CharsetUtil.UTF_8));
        delim.writeInbound(Unpooled.copiedBuffer("a\r\nb\r\n", CharsetUtil.UTF_8));
        chk("a".equals(delim.readInbound()), "delim-frame-1");
        chk("b".equals(delim.readInbound()), "delim-frame-2");
        delim.finish();

        // FixedLengthFrameDecoder
        EmbeddedChannel fix = new EmbeddedChannel(new FixedLengthFrameDecoder(3), new StringDecoder(CharsetUtil.UTF_8));
        fix.writeInbound(Unpooled.copiedBuffer("abcdef", CharsetUtil.UTF_8));
        chk("abc".equals(fix.readInbound()), "fixed-frame-1");
        chk("def".equals(fix.readInbound()), "fixed-frame-2");
        fix.finish();

        // LengthFieldPrepender + LengthFieldBasedFrameDecoder round-trip
        EmbeddedChannel enc = new EmbeddedChannel(new LengthFieldPrepender(4));
        enc.writeOutbound(Unpooled.copiedBuffer("payload", CharsetUtil.UTF_8));
        ByteBuf header = enc.readOutbound();     // prepender emits the length header...
        ByteBuf payloadBuf = enc.readOutbound();  // ...and the original payload as a separate outbound
        chk(header.getInt(0) == 7, "lengthprepender-header");
        ByteBuf framed = Unpooled.wrappedBuffer(header, payloadBuf);
        chk(framed.readableBytes() == 11, "lengthprepender-framed-size");
        EmbeddedChannel dec = new EmbeddedChannel(new LengthFieldBasedFrameDecoder(1024, 0, 4, 0, 4), new StringDecoder(CharsetUtil.UTF_8));
        dec.writeInbound(framed);
        chk("payload".equals(dec.readInbound()), "lengthfield-decode");
        enc.finish(); dec.finish();

        // StringEncoder (outbound)
        EmbeddedChannel sEnc = new EmbeddedChannel(new StringEncoder(CharsetUtil.UTF_8));
        sEnc.writeOutbound("hi");
        ByteBuf encoded = sEnc.readOutbound();
        chk(encoded.toString(CharsetUtil.UTF_8).equals("hi"), "string-encoder");
        encoded.release(); sEnc.finish();

        // custom ByteToMessageDecoder: read 4-byte ints
        EmbeddedChannel intDec = new EmbeddedChannel(new ByteToMessageDecoder() {
            protected void decode(ChannelHandlerContext ctx, ByteBuf in, List<Object> out) {
                while (in.readableBytes() >= 4) out.add(in.readInt());
            }
        });
        ByteBuf two = Unpooled.buffer().writeInt(11).writeInt(22);
        intDec.writeInbound(two);
        chk(Integer.valueOf(11).equals(intDec.readInbound()), "b2m-decode-1");
        chk(Integer.valueOf(22).equals(intDec.readInbound()), "b2m-decode-2");
        intDec.finish();

        // custom MessageToMessageEncoder: Integer -> String
        EmbeddedChannel m2m = new EmbeddedChannel(new MessageToMessageEncoder<Integer>() {
            protected void encode(ChannelHandlerContext ctx, Integer msg, List<Object> out) { out.add("n=" + msg); }
        });
        m2m.writeOutbound(42);
        chk("n=42".equals(m2m.readOutbound()), "m2m-encode");
        m2m.finish();
    }

    // ---------------------------------------------------------------- pipeline + handlers
    static void pipelineAndHandlers() {
        EmbeddedChannel ch = new EmbeddedChannel();
        ChannelPipeline p = ch.pipeline();
        ChannelInboundHandlerAdapter h1 = new ChannelInboundHandlerAdapter();
        StringDecoder h2 = new StringDecoder(CharsetUtil.UTF_8);
        p.addLast("first", h1);
        p.addLast("second", h2);
        chk(p.get("first") == h1 && p.get("second") == h2, "pipeline-addLast-get");
        chk(p.first() == h1, "pipeline-first");
        p.addFirst("zero", new ChannelInboundHandlerAdapter());
        chk(p.first() != h1, "pipeline-addFirst");
        p.remove("zero");
        chk(p.first() == h1, "pipeline-remove");
        p.replace("first", "first2", new ChannelInboundHandlerAdapter());
        chk(p.get("first") == null && p.get("first2") != null, "pipeline-replace");
        List<String> names = p.names();
        chk(names.contains("first2") && names.contains("second"), "pipeline-names");
        ch.finish();

        // inbound flow: counting handler
        final int[] reads = {0};
        EmbeddedChannel flow = new EmbeddedChannel(new ChannelInboundHandlerAdapter() {
            public void channelRead(ChannelHandlerContext ctx, Object msg) { reads[0]++; ReferenceCountUtil.release(msg); }
        });
        flow.writeInbound(Unpooled.copiedBuffer("x", CharsetUtil.UTF_8));
        flow.writeInbound(Unpooled.copiedBuffer("y", CharsetUtil.UTF_8));
        chk(reads[0] == 2, "inbound-channelRead-count");
        flow.finish();

        // outbound flow: capture written
        final AtomicReference<Object> written = new AtomicReference<>();
        EmbeddedChannel out = new EmbeddedChannel(new ChannelOutboundHandlerAdapter() {
            public void write(ChannelHandlerContext ctx, Object msg, ChannelPromise promise) { written.set(msg); promise.setSuccess(); }
        });
        out.writeOutbound("captured");
        chk("captured".equals(written.get()), "outbound-write-capture");
        out.finish();
    }

    // ---------------------------------------------------------------- futures + attributes
    static void futuresAndAttributes() throws Exception {
        EmbeddedChannel ch = new EmbeddedChannel();
        AttributeKey<String> KEY = AttributeKey.valueOf("netty-carpet-key");
        Attribute<String> attr = ch.attr(KEY);
        chk(attr.get() == null, "attr-null-init");
        attr.set("v1");
        chk("v1".equals(ch.attr(KEY).get()), "attr-set-get");
        chk("v1".equals(attr.getAndSet("v2")), "attr-getAndSet");
        chk("v2".equals(attr.get()), "attr-after-getAndSet");

        ChannelPromise pr = ch.newPromise();
        chk(!pr.isDone(), "promise-not-done");
        pr.setSuccess();
        chk(pr.isDone() && pr.isSuccess(), "promise-success");

        ChannelPromise pf = ch.newPromise();
        pf.setFailure(new RuntimeException("boom"));
        chk(pf.isDone() && !pf.isSuccess() && pf.cause() instanceof RuntimeException, "promise-failure");

        ChannelFuture cf = ch.newSucceededFuture();
        chk(cf.isSuccess() && cf.await(2, TimeUnit.SECONDS), "succeeded-future");
        ch.finish();
    }

    // ---------------------------------------------------------------- real TCP echo (loopback)
    static void tcpEchoLoopback() throws Exception {
        NioEventLoopGroup boss = new NioEventLoopGroup(1);
        NioEventLoopGroup worker = new NioEventLoopGroup(1);
        Channel server = null;
        try {
            ServerBootstrap sb = new ServerBootstrap();
            sb.group(boss, worker).channel(NioServerSocketChannel.class)
              .option(ChannelOption.SO_BACKLOG, 16)
              .childOption(ChannelOption.TCP_NODELAY, true)
              .childHandler(new ChannelInitializer<SocketChannel>() {
                  protected void initChannel(SocketChannel c) {
                      c.pipeline().addLast(new ChannelInboundHandlerAdapter() {
                          public void channelRead(ChannelHandlerContext ctx, Object msg) { ctx.writeAndFlush(msg); }
                      });
                  }
              });
            server = sb.bind(new InetSocketAddress("127.0.0.1", 0)).sync().channel();
            int port = ((InetSocketAddress) server.localAddress()).getPort();
            chk(port > 0 && server.isActive(), "tcp-server-active");

            try (Socket sock = new Socket()) {
                sock.connect(new InetSocketAddress("127.0.0.1", port), 5000);
                sock.setSoTimeout(5000);
                byte[] payload = "echo-me\n".getBytes(StandardCharsets.UTF_8);
                sock.getOutputStream().write(payload);
                sock.getOutputStream().flush();
                byte[] buf = new byte[payload.length];
                int read = 0;
                while (read < buf.length) {
                    int r = sock.getInputStream().read(buf, read, buf.length - read);
                    if (r < 0) break;
                    read += r;
                }
                chk(read == payload.length && new String(buf, StandardCharsets.UTF_8).equals("echo-me\n"), "tcp-echo-roundtrip");
            }
        } finally {
            if (server != null) server.close().sync();
            boss.shutdownGracefully(0, 1, TimeUnit.SECONDS).sync();
            worker.shutdownGracefully(0, 1, TimeUnit.SECONDS).sync();
        }
        chk(true, "tcp-graceful-shutdown");
    }

    // ---------------------------------------------------------------- real HTTP (loopback)
    static void httpCodecLoopback() throws Exception {
        NioEventLoopGroup boss = new NioEventLoopGroup(1);
        NioEventLoopGroup worker = new NioEventLoopGroup(1);
        Channel server = null;
        try {
            ServerBootstrap sb = new ServerBootstrap();
            sb.group(boss, worker).channel(NioServerSocketChannel.class)
              .childHandler(new ChannelInitializer<SocketChannel>() {
                  protected void initChannel(SocketChannel c) {
                      c.pipeline().addLast(new io.netty.handler.codec.http.HttpServerCodec());
                      c.pipeline().addLast(new io.netty.handler.codec.http.HttpObjectAggregator(65536));
                      c.pipeline().addLast(new SimpleChannelInboundHandler<io.netty.handler.codec.http.FullHttpRequest>() {
                          protected void channelRead0(ChannelHandlerContext ctx, io.netty.handler.codec.http.FullHttpRequest req) {
                              String body = "method=" + req.method().name() + " uri=" + req.uri();
                              ByteBuf content = Unpooled.copiedBuffer(body, CharsetUtil.UTF_8);
                              io.netty.handler.codec.http.FullHttpResponse resp = new io.netty.handler.codec.http.DefaultFullHttpResponse(
                                      io.netty.handler.codec.http.HttpVersion.HTTP_1_1,
                                      io.netty.handler.codec.http.HttpResponseStatus.OK, content);
                              resp.headers().set(io.netty.handler.codec.http.HttpHeaderNames.CONTENT_TYPE, "text/plain; charset=UTF-8");
                              resp.headers().setInt(io.netty.handler.codec.http.HttpHeaderNames.CONTENT_LENGTH, content.readableBytes());
                              ctx.writeAndFlush(resp).addListener(ChannelFutureListener.CLOSE);
                          }
                      });
                  }
              });
            server = sb.bind(new InetSocketAddress("127.0.0.1", 0)).sync().channel();
            int port = ((InetSocketAddress) server.localAddress()).getPort();
            chk(server.isActive(), "http-server-active");

            HttpURLConnection conn = (HttpURLConnection) new URL("http://127.0.0.1:" + port + "/ping").openConnection();
            conn.setConnectTimeout(5000); conn.setReadTimeout(5000);
            chk(conn.getResponseCode() == 200, "http-status-200");
            chk(conn.getContentType().startsWith("text/plain"), "http-content-type");
            String body;
            try (BufferedReader r = new BufferedReader(new InputStreamReader(conn.getInputStream(), StandardCharsets.UTF_8))) {
                StringBuilder sbb = new StringBuilder(); String ln;
                while ((ln = r.readLine()) != null) sbb.append(ln);
                body = sbb.toString();
            }
            chk(body.equals("method=GET uri=/ping"), "http-response-body");

            HttpURLConnection post = (HttpURLConnection) new URL("http://127.0.0.1:" + port + "/data").openConnection();
            post.setRequestMethod("POST"); post.setDoOutput(true);
            post.setConnectTimeout(5000); post.setReadTimeout(5000);
            try (OutputStream os = post.getOutputStream()) { os.write("x".getBytes(StandardCharsets.UTF_8)); }
            chk(post.getResponseCode() == 200, "http-post-status");
            try (BufferedReader r = new BufferedReader(new InputStreamReader(post.getInputStream(), StandardCharsets.UTF_8))) {
                chk(r.readLine().equals("method=POST uri=/data"), "http-post-body");
            }
        } finally {
            if (server != null) server.close().sync();
            boss.shutdownGracefully(0, 1, TimeUnit.SECONDS).sync();
            worker.shutdownGracefully(0, 1, TimeUnit.SECONDS).sync();
        }
    }
}
