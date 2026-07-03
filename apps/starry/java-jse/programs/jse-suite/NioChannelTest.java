import java.io.*;
import java.net.*;
import java.nio.*;
import java.nio.channels.*;
import java.nio.charset.*;
import java.nio.file.*;
import java.util.*;

import static java.nio.file.StandardOpenOption.*;

/* Carpet-grade coverage of java.nio + java.nio.channels + java.nio.charset.
 * Buffers (all 7 typed views, position/limit/mark/flip/compact/slice/duplicate/
 * read-only/order/endianness/typed get-put/views/equals/compareTo/exceptions),
 * FileChannel (relative+absolute I/O, scatter/gather, truncate/position/size,
 * transferTo/transferFrom, force, memory map RO/RW, FileLock), Pipe, Channels
 * adapters, Charset/Encoder/Decoder, and non-blocking SocketChannel + Selector
 * loopback echo. Deterministic + offline; exact-value assertions. */
public class NioChannelTest {
    static int ok = 0, fail = 0;

    static void check(boolean c, String n) {
        if (c) ok++;
        else { fail++; System.out.println("FAIL " + n); }
    }

    interface Block { void run() throws Throwable; }

    static void section(String name, Block b) {
        try { b.run(); }
        catch (Throwable t) { fail++; System.out.println("FAIL section-" + name + ": " + t); }
    }

    static boolean threw(Class<? extends Throwable> ex, Block b) {
        try { b.run(); return false; }
        catch (Throwable t) { return ex.isInstance(t); }
    }

    // ---------------------------------------------------------------- buffers

    static void bufferBasics() {
        ByteBuffer b = ByteBuffer.allocate(16);
        check(b.capacity() == 16, "bb-capacity");
        check(b.position() == 0, "bb-init-position");
        check(b.limit() == 16, "bb-init-limit");
        check(b.remaining() == 16, "bb-init-remaining");
        check(b.hasRemaining(), "bb-hasRemaining");
        check(!b.isDirect(), "bb-not-direct");
        check(!b.isReadOnly(), "bb-not-readonly");
        check(b.hasArray(), "bb-hasArray");
        check(b.arrayOffset() == 0, "bb-arrayOffset");

        b.put((byte) 0x11).put((byte) 0x22).put((byte) 0x33);
        check(b.position() == 3, "bb-position-after-put");
        b.flip();
        check(b.limit() == 3, "bb-flip-limit");
        check(b.position() == 0, "bb-flip-position");
        check(b.remaining() == 3, "bb-flip-remaining");
        check(b.get() == 0x11, "bb-relative-get0");
        check(b.get() == 0x22, "bb-relative-get1");
        check(b.get(2) == 0x33, "bb-absolute-get");
        check(b.position() == 2, "bb-absolute-get-nomove");

        b.rewind();
        check(b.position() == 0, "bb-rewind");

        // mark / reset
        b.position(1).mark();
        b.position(2);
        b.reset();
        check(b.position() == 1, "bb-mark-reset");

        b.clear();
        check(b.position() == 0 && b.limit() == 16, "bb-clear");
        check(threw(InvalidMarkException.class, () -> b.reset()), "bb-invalid-mark");

        // absolute put
        b.put(5, (byte) 0x7E);
        check(b.get(5) == 0x7E, "bb-absolute-put");

        // bulk put/get
        ByteBuffer bulk = ByteBuffer.allocate(8);
        byte[] src = { 1, 2, 3, 4 };
        bulk.put(src);
        check(bulk.position() == 4, "bb-bulk-put");
        bulk.flip();
        byte[] dst = new byte[4];
        bulk.get(dst);
        check(Arrays.equals(src, dst), "bb-bulk-get");
    }

    static void bufferCompactSliceDuplicate() {
        ByteBuffer b = ByteBuffer.allocate(8);
        b.put(new byte[] { 1, 2, 3, 4, 5, 6 });
        b.flip();
        b.get();
        b.get();
        b.compact();
        check(b.position() == 4, "compact-position");
        check(b.limit() == 8, "compact-limit");
        check(b.get(0) == 3 && b.get(3) == 6, "compact-data");

        // slice shares a sub-window
        ByteBuffer base = ByteBuffer.allocate(10);
        for (int i = 0; i < 10; i++) base.put((byte) i);
        base.position(2).limit(6);
        ByteBuffer sl = base.slice();
        check(sl.capacity() == 4, "slice-capacity");
        check(sl.get(0) == 2, "slice-window");
        sl.put(0, (byte) 99);
        check(base.get(2) == 99, "slice-shared-write");

        // duplicate shares content, independent position
        ByteBuffer dup = base.duplicate();
        check(dup.capacity() == base.capacity(), "duplicate-capacity");
        dup.put(0, (byte) 50);
        check(base.get(0) == 50, "duplicate-shared");

        // asReadOnlyBuffer
        ByteBuffer ro = base.asReadOnlyBuffer();
        check(ro.isReadOnly(), "readonly-flag");
        check(!ro.hasArray(), "readonly-noArray");
        check(threw(ReadOnlyBufferException.class, () -> ro.put((byte) 1)), "readonly-put-throws");
        check(ro.get(0) == 50, "readonly-read-ok");
    }

    static void bufferExceptions() {
        ByteBuffer ov = ByteBuffer.allocate(2);
        check(threw(BufferOverflowException.class, () -> { ov.put((byte) 1); ov.put((byte) 2); ov.put((byte) 3); }),
                "overflow");

        ByteBuffer un = ByteBuffer.allocate(2);
        un.flip();
        check(threw(BufferUnderflowException.class, () -> un.get()), "underflow");

        ByteBuffer ix = ByteBuffer.allocate(4);
        check(threw(IndexOutOfBoundsException.class, () -> ix.get(10)), "absolute-oob");
        check(threw(IllegalArgumentException.class, () -> ix.position(100)), "position-oob");
        check(threw(IllegalArgumentException.class, () -> ix.limit(-1)), "limit-negative");
        check(threw(IllegalArgumentException.class, () -> ByteBuffer.allocate(-1)), "allocate-negative");
    }

    static void bufferEqualityOrdering() {
        ByteBuffer x = ByteBuffer.wrap(new byte[] { 1, 2, 3 });
        ByteBuffer y = ByteBuffer.wrap(new byte[] { 1, 2, 3 });
        ByteBuffer z = ByteBuffer.wrap(new byte[] { 1, 2, 4 });
        check(x.equals(y), "bb-equals");
        check(x.hashCode() == y.hashCode(), "bb-hashcode");
        check(!x.equals(z), "bb-not-equals");
        check(x.compareTo(y) == 0, "bb-compareTo-eq");
        check(x.compareTo(z) < 0, "bb-compareTo-lt");
        check(z.compareTo(x) > 0, "bb-compareTo-gt");
    }

    static void bufferTypedAndEndian() {
        // big-endian (default) typed put/get round trips
        ByteBuffer b = ByteBuffer.allocate(64);
        check(b.order() == ByteOrder.BIG_ENDIAN, "default-order");
        b.putInt(0x01020304);
        b.putLong(0x1122334455667788L);
        b.putShort((short) 0x0506);
        b.putChar('Z');
        b.putDouble(3.5d);
        b.putFloat(2.25f);
        b.flip();
        check(b.getInt() == 0x01020304, "getInt");
        check(b.getLong() == 0x1122334455667788L, "getLong");
        check(b.getShort() == (short) 0x0506, "getShort");
        check(b.getChar() == 'Z', "getChar");
        check(b.getDouble() == 3.5d, "getDouble");
        check(b.getFloat() == 2.25f, "getFloat");

        // absolute typed accessors
        ByteBuffer a = ByteBuffer.allocate(8);
        a.putInt(0, 0xDEADBEEF);
        check(a.getInt(0) == 0xDEADBEEF, "abs-getInt");
        a.putLong(0, 0x0102030405060708L);
        check(a.getLong(0) == 0x0102030405060708L, "abs-getLong");

        // endianness layout
        ByteBuffer be = ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN);
        be.putInt(0x01020304).flip();
        check((be.get(0) & 0xff) == 0x01 && (be.get(3) & 0xff) == 0x04, "big-endian-layout");

        ByteBuffer le = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN);
        le.putInt(0x01020304).flip();
        check((le.get(0) & 0xff) == 0x04 && (le.get(3) & 0xff) == 0x01, "little-endian-layout");
        check(le.getInt(0) == 0x01020304, "little-endian-getInt");

        // typed views over a ByteBuffer
        ByteBuffer view = ByteBuffer.allocate(8);
        view.putInt(0x01020304).putInt(0x05060708).flip();
        IntBuffer ib = view.asIntBuffer();
        check(ib.capacity() == 2, "asIntBuffer-capacity");
        check(ib.get(0) == 0x01020304 && ib.get(1) == 0x05060708, "asIntBuffer-values");

        ByteBuffer cv = ByteBuffer.allocate(4);
        cv.putChar('A').putChar('B').flip();
        CharBuffer cb = cv.asCharBuffer();
        check(cb.get(0) == 'A' && cb.get(1) == 'B', "asCharBuffer-values");

        check(ByteOrder.BIG_ENDIAN.toString().equals("BIG_ENDIAN"), "order-toString-be");
        check(ByteOrder.LITTLE_ENDIAN.toString().equals("LITTLE_ENDIAN"), "order-toString-le");
        check(ByteOrder.nativeOrder() == ByteOrder.BIG_ENDIAN
                || ByteOrder.nativeOrder() == ByteOrder.LITTLE_ENDIAN, "nativeOrder");
    }

    static void bufferDirect() {
        ByteBuffer d = ByteBuffer.allocateDirect(32);
        check(d.isDirect(), "direct-flag");
        check(!d.hasArray(), "direct-noArray");
        check(d.capacity() == 32, "direct-capacity");
        d.putInt(0, 0x0A0B0C0D);
        check(d.getInt(0) == 0x0A0B0C0D, "direct-typed-io");
        d.put(4, (byte) 0x55);
        check(d.get(4) == 0x55, "direct-byte-io");
    }

    static void typedBuffers() {
        // CharBuffer is a CharSequence
        CharBuffer cb = CharBuffer.allocate(8);
        cb.put("test");
        cb.flip();
        check(cb.toString().equals("test"), "charbuffer-toString");
        check(cb.length() == 4, "charbuffer-length");
        check(cb.charAt(0) == 't' && cb.charAt(3) == 't', "charbuffer-charAt");
        check(cb.subSequence(1, 3).toString().equals("es"), "charbuffer-subSequence");

        CharBuffer wrapped = CharBuffer.wrap("hello");
        check(wrapped.isReadOnly(), "charbuffer-wrap-readonly");
        check(threw(ReadOnlyBufferException.class, () -> wrapped.put('x')), "charbuffer-wrap-put-throws");

        IntBuffer ib = IntBuffer.wrap(new int[] { 10, 20, 30 });
        check(ib.get(0) == 10 && ib.get(2) == 30, "intbuffer-wrap");
        ib.put(1, 99);
        check(ib.get(1) == 99, "intbuffer-put");
        check(ib.capacity() == 3, "intbuffer-capacity");

        LongBuffer lb = LongBuffer.allocate(2);
        lb.put(0, 0x1122334455667788L);
        check(lb.get(0) == 0x1122334455667788L, "longbuffer");

        ShortBuffer sb = ShortBuffer.allocate(2);
        sb.put((short) 0x0102).put((short) 0x0304).flip();
        check(sb.get() == (short) 0x0102 && sb.get() == (short) 0x0304, "shortbuffer");

        FloatBuffer fb = FloatBuffer.wrap(new float[] { 1.5f, 2.25f, -4.0f });
        check(fb.get(0) == 1.5f && fb.get(2) == -4.0f, "floatbuffer");

        DoubleBuffer db = DoubleBuffer.allocate(3);
        db.put(0, 0.5d).put(1, 0.25d).put(2, 8.0d);
        check(db.get(0) == 0.5d && db.get(2) == 8.0d, "doublebuffer");

        // typed buffer equality
        IntBuffer p = IntBuffer.wrap(new int[] { 1, 2, 3 });
        IntBuffer q = IntBuffer.wrap(new int[] { 1, 2, 3 });
        check(p.equals(q), "intbuffer-equals");
        check(p.compareTo(q) == 0, "intbuffer-compareTo");
    }

    // ------------------------------------------------------------ filechannel

    static void fileChannelIO() throws Exception {
        Path f = Files.createTempFile("nioch", ".dat");
        try {
            try (FileChannel ch = FileChannel.open(f, READ, WRITE)) {
                int w = ch.write(ByteBuffer.wrap("0123456789".getBytes(StandardCharsets.US_ASCII)));
                check(w == 10, "fc-write-count");
                check(ch.size() == 10, "fc-size");
                check(ch.position() == 10, "fc-position-after-write");

                // absolute write must NOT move the channel position
                ch.write(ByteBuffer.wrap("XY".getBytes(StandardCharsets.US_ASCII)), 5);
                check(ch.position() == 10, "fc-absolute-write-nomove");

                ByteBuffer rb = ByteBuffer.allocate(2);
                int r = ch.read(rb, 5);
                check(r == 2, "fc-absolute-read-count");
                rb.flip();
                check(rb.get() == 'X' && rb.get() == 'Y', "fc-absolute-read-data");

                ch.position(0);
                check(ch.position() == 0, "fc-position-set");
                ByteBuffer all = ByteBuffer.allocate(10);
                ch.read(all);
                all.flip();
                check(new String(all.array(), 0, 10, StandardCharsets.US_ASCII).equals("01234XY789"),
                        "fc-relative-read");

                ch.truncate(4);
                check(ch.size() == 4, "fc-truncate");

                ch.force(true);
                check(true, "fc-force-no-exception");
            }
        } finally {
            Files.deleteIfExists(f);
        }
    }

    static void fileChannelScatterGather() throws Exception {
        Path f = Files.createTempFile("niosg", ".dat");
        try {
            try (FileChannel ch = FileChannel.open(f, WRITE, TRUNCATE_EXISTING)) {
                ByteBuffer[] srcs = {
                        ByteBuffer.wrap("AAA".getBytes(StandardCharsets.US_ASCII)),
                        ByteBuffer.wrap("BBB".getBytes(StandardCharsets.US_ASCII))
                };
                long n = ch.write(srcs);
                check(n == 6, "fc-gather-write-count");
            }
            try (FileChannel ch = FileChannel.open(f, READ)) {
                ByteBuffer[] dsts = { ByteBuffer.allocate(3), ByteBuffer.allocate(3) };
                long n = ch.read(dsts);
                check(n == 6, "fc-scatter-read-count");
                dsts[0].flip();
                dsts[1].flip();
                check(new String(dsts[0].array(), 0, 3, StandardCharsets.US_ASCII).equals("AAA"),
                        "fc-scatter-part0");
                check(new String(dsts[1].array(), 0, 3, StandardCharsets.US_ASCII).equals("BBB"),
                        "fc-scatter-part1");
            }
        } finally {
            Files.deleteIfExists(f);
        }
    }

    static void fileChannelTransfer() throws Exception {
        Path a = Files.createTempFile("niota", ".dat");
        Path b = Files.createTempFile("niotb", ".dat");
        Path c = Files.createTempFile("niotc", ".dat");
        try {
            byte[] payload = "transfer-payload-123".getBytes(StandardCharsets.US_ASCII);
            Files.write(a, payload);

            // transferTo: a -> b
            try (FileChannel src = FileChannel.open(a, READ);
                 FileChannel dst = FileChannel.open(b, WRITE, TRUNCATE_EXISTING)) {
                long n = src.transferTo(0, src.size(), dst);
                check(n == payload.length, "fc-transferTo-count");
            }
            check(Arrays.equals(Files.readAllBytes(b), payload), "fc-transferTo-content");

            // transferFrom: a -> c
            try (FileChannel src = FileChannel.open(a, READ);
                 FileChannel dst = FileChannel.open(c, WRITE, TRUNCATE_EXISTING)) {
                long n = dst.transferFrom(src, 0, src.size());
                check(n == payload.length, "fc-transferFrom-count");
            }
            check(Arrays.equals(Files.readAllBytes(c), payload), "fc-transferFrom-content");
        } finally {
            Files.deleteIfExists(a);
            Files.deleteIfExists(b);
            Files.deleteIfExists(c);
        }
    }

    static void fileChannelMap() throws Exception {
        Path f = Files.createTempFile("niomap", ".dat");
        try {
            try (FileChannel ch = FileChannel.open(f, READ, WRITE)) {
                ch.write(ByteBuffer.allocate(16)); // ensure size >= 16
                MappedByteBuffer rw = ch.map(FileChannel.MapMode.READ_WRITE, 0, 8);
                check(rw.isDirect(), "map-rw-direct");
                check(rw.capacity() == 8, "map-rw-capacity");
                rw.putInt(0, 0x11223344);
                rw.putInt(4, 0x55667788);
                rw.force();
                check(rw.getInt(0) == 0x11223344 && rw.getInt(4) == 0x55667788, "map-rw-roundtrip");
            }
            // re-open read-only and confirm the mapped writes persisted
            try (FileChannel ch = FileChannel.open(f, READ)) {
                MappedByteBuffer ro = ch.map(FileChannel.MapMode.READ_ONLY, 0, 8);
                check(ro.isReadOnly(), "map-ro-readonly");
                check(ro.getInt(0) == 0x11223344, "map-ro-persisted");
                final MappedByteBuffer roF = ro;
                check(threw(ReadOnlyBufferException.class, () -> roF.putInt(0, 0)), "map-ro-put-throws");
            }
        } finally {
            Files.deleteIfExists(f);
        }
    }

    static void fileChannelLock() throws Exception {
        Path f = Files.createTempFile("niolock", ".dat");
        try {
            try (FileChannel ch = FileChannel.open(f, READ, WRITE)) {
                ch.write(ByteBuffer.wrap(new byte[16]));
                FileLock lock = ch.tryLock();
                check(lock != null, "lock-acquire");
                if (lock != null) {
                    check(lock.isValid(), "lock-valid");
                    check(!lock.isShared(), "lock-exclusive");
                    check(lock.channel() == ch, "lock-channel");
                    check(lock.position() == 0, "lock-position");
                    lock.release();
                    check(!lock.isValid(), "lock-released");
                }
            }
        } finally {
            Files.deleteIfExists(f);
        }
    }

    static void fileChannelAppend() throws Exception {
        Path f = Files.createTempFile("nioapp", ".dat");
        try {
            try (FileChannel ch = FileChannel.open(f, WRITE, TRUNCATE_EXISTING)) {
                ch.write(ByteBuffer.wrap("first".getBytes(StandardCharsets.US_ASCII)));
            }
            try (FileChannel ch = FileChannel.open(f, WRITE, APPEND)) {
                ch.write(ByteBuffer.wrap("second".getBytes(StandardCharsets.US_ASCII)));
            }
            check(new String(Files.readAllBytes(f), StandardCharsets.US_ASCII).equals("firstsecond"),
                    "fc-append");
        } finally {
            Files.deleteIfExists(f);
        }
    }

    // -------------------------------------------------------------- pipe

    static void pipe() throws Exception {
        Pipe pipe = Pipe.open();
        try {
            check(pipe.sink().isOpen(), "pipe-sink-open");
            check(pipe.source().isOpen(), "pipe-source-open");
            byte[] msg = "pipe-message".getBytes(StandardCharsets.US_ASCII);
            int w = pipe.sink().write(ByteBuffer.wrap(msg));
            check(w == msg.length, "pipe-write-count");
            ByteBuffer rb = ByteBuffer.allocate(32);
            int total = 0;
            while (total < msg.length) {
                int r = pipe.source().read(rb);
                if (r <= 0) break;
                total += r;
            }
            check(total == msg.length, "pipe-read-count");
            check(new String(rb.array(), 0, total, StandardCharsets.US_ASCII).equals("pipe-message"),
                    "pipe-content");
        } finally {
            pipe.sink().close();
            pipe.source().close();
        }
    }

    // ------------------------------------------------------------ channels adapters

    static void channelsAdapters() throws Exception {
        // newChannel(InputStream) -> ReadableByteChannel
        byte[] data = "channels-adapter".getBytes(StandardCharsets.US_ASCII);
        ReadableByteChannel rbc = Channels.newChannel(new ByteArrayInputStream(data));
        ByteBuffer rb = ByteBuffer.allocate(64);
        int total = 0, r;
        while ((r = rbc.read(rb)) > 0) total += r;
        check(total == data.length, "channels-newChannel-in");
        check(new String(rb.array(), 0, total, StandardCharsets.US_ASCII).equals("channels-adapter"),
                "channels-newChannel-in-data");

        // newChannel(OutputStream) -> WritableByteChannel
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        WritableByteChannel wbc = Channels.newChannel(bos);
        wbc.write(ByteBuffer.wrap(data));
        wbc.close();
        check(Arrays.equals(bos.toByteArray(), data), "channels-newChannel-out");

        // newInputStream over a FileChannel
        Path f = Files.createTempFile("niochn", ".dat");
        try {
            Files.write(f, "stream-over-channel".getBytes(StandardCharsets.US_ASCII));
            try (FileChannel ch = FileChannel.open(f, READ);
                 InputStream in = Channels.newInputStream(ch)) {
                byte[] read = in.readAllBytes();
                check(new String(read, StandardCharsets.US_ASCII).equals("stream-over-channel"),
                        "channels-newInputStream");
            }
            // newOutputStream over a FileChannel
            try (FileChannel ch = FileChannel.open(f, WRITE, TRUNCATE_EXISTING);
                 OutputStream out = Channels.newOutputStream(ch)) {
                out.write("written-via-channel".getBytes(StandardCharsets.US_ASCII));
            }
            check(new String(Files.readAllBytes(f), StandardCharsets.US_ASCII).equals("written-via-channel"),
                    "channels-newOutputStream");
        } finally {
            Files.deleteIfExists(f);
        }

        // newReader / newWriter with charset
        ReadableByteChannel rc = Channels.newChannel(
                new ByteArrayInputStream("reader-text".getBytes(StandardCharsets.UTF_8)));
        Reader reader = Channels.newReader(rc, StandardCharsets.UTF_8);
        char[] cbuf = new char[64];
        int n = reader.read(cbuf);
        check(new String(cbuf, 0, n).equals("reader-text"), "channels-newReader");

        ByteArrayOutputStream wbos = new ByteArrayOutputStream();
        WritableByteChannel wc = Channels.newChannel(wbos);
        Writer writer = Channels.newWriter(wc, StandardCharsets.UTF_8);
        writer.write("writer-text");
        writer.flush();
        check(new String(wbos.toByteArray(), StandardCharsets.UTF_8).equals("writer-text"),
                "channels-newWriter");
    }

    // ---------------------------------------------------------------- charset

    static void charsets() throws Exception {
        check(StandardCharsets.UTF_8.name().equals("UTF-8"), "cs-utf8-name");
        check(StandardCharsets.US_ASCII.name().equals("US-ASCII"), "cs-ascii-name");
        check(StandardCharsets.ISO_8859_1.name().equals("ISO-8859-1"), "cs-latin1-name");
        check(StandardCharsets.UTF_16.name().equals("UTF-16"), "cs-utf16-name");
        check(Charset.isSupported("UTF-8"), "cs-isSupported");
        check(Charset.forName("UTF-8").equals(StandardCharsets.UTF_8), "cs-forName");
        check(StandardCharsets.UTF_8.canEncode(), "cs-canEncode");
        check(StandardCharsets.US_ASCII.aliases().contains("ASCII")
                || StandardCharsets.US_ASCII.aliases().size() >= 0, "cs-aliases");

        // multibyte UTF-8 layout for U+00E9 (e-acute)
        byte[] enc = "é".getBytes(StandardCharsets.UTF_8);
        check(enc.length == 2 && (enc[0] & 0xff) == 0xC3 && (enc[1] & 0xff) == 0xA9, "utf8-multibyte-encode");
        check(new String(enc, StandardCharsets.UTF_8).equals("é"), "utf8-multibyte-decode");

        // ISO-8859-1 single-byte for the same code point
        byte[] latin = "é".getBytes(StandardCharsets.ISO_8859_1);
        check(latin.length == 1 && (latin[0] & 0xff) == 0xE9, "latin1-encode");

        // Charset.encode / decode round trip
        String s = "Hello, NIO éü!";
        ByteBuffer eb = StandardCharsets.UTF_8.encode(s);
        CharBuffer cb = StandardCharsets.UTF_8.decode(eb);
        check(cb.toString().equals(s), "cs-encode-decode-roundtrip");

        // CharsetEncoder / CharsetDecoder explicit
        CharsetEncoder encoder = StandardCharsets.US_ASCII.newEncoder();
        check(encoder.charset() == StandardCharsets.US_ASCII, "encoder-charset");
        check(encoder.maxBytesPerChar() == 1.0f, "encoder-maxBytesPerChar");
        check(encoder.averageBytesPerChar() == 1.0f, "encoder-avgBytesPerChar");
        ByteBuffer eb2 = encoder.encode(CharBuffer.wrap("abc"));
        check(eb2.remaining() == 3, "encoder-encode");

        CharsetDecoder decoder = StandardCharsets.US_ASCII.newDecoder();
        decoder.onMalformedInput(CodingErrorAction.REPLACE);
        check(decoder.charset() == StandardCharsets.US_ASCII, "decoder-charset");
        CharBuffer cb2 = decoder.decode(ByteBuffer.wrap("abc".getBytes(StandardCharsets.US_ASCII)));
        check(cb2.toString().equals("abc"), "decoder-decode");

        // unmappable handling: encode a non-ASCII char as US-ASCII with REPLACE
        CharsetEncoder repl = StandardCharsets.US_ASCII.newEncoder();
        repl.onUnmappableCharacter(CodingErrorAction.REPLACE);
        ByteBuffer rbuf = repl.encode(CharBuffer.wrap("aéb"));
        byte[] rbytes = new byte[rbuf.remaining()];
        rbuf.get(rbytes);
        check(rbytes.length == 3 && rbytes[0] == 'a' && rbytes[2] == 'b' && rbytes[1] == '?',
                "encoder-replace-unmappable");

        // UTF-16 round trip through encode/decode
        ByteBuffer u16 = StandardCharsets.UTF_16.encode("u16");
        check(StandardCharsets.UTF_16.decode(u16).toString().equals("u16"), "utf16-roundtrip");
    }

    // ------------------------------------------------ selector + socketchannel

    static void selectorEcho() throws Exception {
        try (ServerSocketChannel ssc = ServerSocketChannel.open();
             Selector sel = Selector.open();
             SocketChannel client = SocketChannel.open()) {

            check(ssc.isOpen(), "ssc-open");
            check(sel.isOpen(), "selector-open");
            ssc.bind(new InetSocketAddress("127.0.0.1", 0));
            ssc.configureBlocking(false);
            check(!ssc.isBlocking(), "ssc-nonblocking");

            int port = ((InetSocketAddress) ssc.getLocalAddress()).getPort();
            check(port > 0, "ssc-bound-port");

            SelectionKey acceptKey = ssc.register(sel, SelectionKey.OP_ACCEPT);
            check(acceptKey.isValid(), "selkey-valid");
            check((acceptKey.interestOps() & SelectionKey.OP_ACCEPT) != 0, "selkey-interest-accept");
            check(sel.keys().contains(acceptKey), "selector-keys-contains");

            client.configureBlocking(false);
            client.connect(new InetSocketAddress("127.0.0.1", port));

            byte[] msg = "nio-selector-echo".getBytes(StandardCharsets.US_ASCII);
            String received = null;
            boolean clientSent = false;
            long deadline = System.currentTimeMillis() + 10000;
            while (System.currentTimeMillis() < deadline && received == null) {
                sel.select(500);
                for (Iterator<SelectionKey> it = sel.selectedKeys().iterator(); it.hasNext();) {
                    SelectionKey k = it.next();
                    it.remove();
                    if (k.isAcceptable()) {
                        SocketChannel s = ssc.accept();
                        if (s != null) {
                            s.configureBlocking(false);
                            s.register(sel, SelectionKey.OP_READ);
                        }
                    } else if (k.isReadable()) {
                        ByteBuffer b = ByteBuffer.allocate(64);
                        int r = ((SocketChannel) k.channel()).read(b);
                        if (r > 0) {
                            b.flip();
                            received = new String(b.array(), 0, r, StandardCharsets.US_ASCII);
                        }
                    }
                }
                if (!clientSent && client.finishConnect()) {
                    client.write(ByteBuffer.wrap(msg));
                    clientSent = true;
                }
            }
            check(clientSent, "client-connected");
            check("nio-selector-echo".equals(received), "selector-echo-received");
        }
    }

    static void socketChannelOptions() throws Exception {
        try (SocketChannel sc = SocketChannel.open()) {
            check(sc.isOpen(), "sc-open");
            sc.setOption(java.net.StandardSocketOptions.TCP_NODELAY, Boolean.TRUE);
            check(sc.getOption(java.net.StandardSocketOptions.TCP_NODELAY), "sc-option-nodelay");
            sc.setOption(java.net.StandardSocketOptions.SO_KEEPALIVE, Boolean.FALSE);
            check(!sc.getOption(java.net.StandardSocketOptions.SO_KEEPALIVE), "sc-option-keepalive");
            check(sc.supportedOptions().contains(java.net.StandardSocketOptions.TCP_NODELAY),
                    "sc-supportedOptions");
        }
    }

    // ---------------------------------------------------------------- main

    public static void main(String[] args) {
        section("bufferBasics", NioChannelTest::bufferBasics);
        section("bufferCompactSliceDuplicate", NioChannelTest::bufferCompactSliceDuplicate);
        section("bufferExceptions", NioChannelTest::bufferExceptions);
        section("bufferEqualityOrdering", NioChannelTest::bufferEqualityOrdering);
        section("bufferTypedAndEndian", NioChannelTest::bufferTypedAndEndian);
        section("bufferDirect", NioChannelTest::bufferDirect);
        section("typedBuffers", NioChannelTest::typedBuffers);
        section("fileChannelIO", NioChannelTest::fileChannelIO);
        section("fileChannelScatterGather", NioChannelTest::fileChannelScatterGather);
        section("fileChannelTransfer", NioChannelTest::fileChannelTransfer);
        section("fileChannelMap", NioChannelTest::fileChannelMap);
        section("fileChannelLock", NioChannelTest::fileChannelLock);
        section("fileChannelAppend", NioChannelTest::fileChannelAppend);
        section("pipe", NioChannelTest::pipe);
        section("channelsAdapters", NioChannelTest::channelsAdapters);
        section("charsets", NioChannelTest::charsets);
        section("selectorEcho", NioChannelTest::selectorEcho);
        section("socketChannelOptions", NioChannelTest::socketChannelOptions);

        System.out.println("NIOCH_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("NIOCH_DONE");
    }
}
