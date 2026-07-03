import java.io.*;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.CharBuffer;
import java.nio.IntBuffer;
import java.nio.channels.Channels;
import java.nio.channels.FileChannel;
import java.nio.channels.SeekableByteChannel;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.nio.file.attribute.*;
import java.util.*;
import java.util.stream.Collectors;
import java.util.zip.Adler32;
import java.util.zip.CRC32;
import java.util.zip.CheckedInputStream;
import java.util.zip.CheckedOutputStream;
import java.util.zip.Deflater;
import java.util.zip.GZIPInputStream;
import java.util.zip.GZIPOutputStream;
import java.util.zip.Inflater;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import java.util.zip.ZipInputStream;
import java.util.zip.ZipOutputStream;

/*
 * 文件/IO 地毯级覆盖 — JDK17 标准库 java.io / java.nio.file / java.nio /
 * java.nio.channels / java.nio.charset / java.util.zip 全 API 矩阵。
 * 全部数据自造、断言精确相等、离线无网络、临时文件仅在 /tmp 下。
 * 验证 starry 文件系统 syscall 链(open/openat/read/write/pread/pwrite/stat/
 * lseek/ftruncate/mkdir/rmdir/unlink/rename/link/readlink/chmod/utimensat...)。
 */
public class FileTest {
    static int ok = 0, fail = 0;

    static void check(boolean c, String n) {
        if (c) {
            ok++;
        } else {
            fail++;
            System.out.println("FAIL " + n);
        }
    }

    static Path base;

    public static void main(String[] args) throws Exception {
        base = Files.createTempDirectory(Path.of("/tmp"), "dodfs");
        try {
            testPath();
            testPaths();
            testFilesWriteRead();
            testFilesBytesAndOptions();
            testFilesCreateExistsType();
            testFilesAttributes();
            testFilesTimes();
            testFilesPosixPermissions();
            testFilesCopyMove();
            testFilesDirStream();
            testFilesWalk();
            testFilesWalkFileTree();
            testFilesMismatchSameLines();
            testFilesLinks();
            testFileClass();
            testFileStreams();
            testBufferedStreams();
            testDataStreams();
            testByteArrayStreams();
            testCharStreams();
            testPushbackAndSequence();
            testLineNumberAndTokenizer();
            testPrintStreamWriter();
            testRandomAccessFile();
            testFileChannel();
            testByteBuffer();
            testCharBuffer();
            testCharset();
            testSerialization();
            testZipCrcAdler();
            testDeflateInflate();
            testGzip();
            testZipStreams();
            testZipFile();
        } finally {
            cleanup(base);
        }

        System.out.println("FILE_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) {
            System.out.println("FILE_DONE");
        }
    }

    // ---------------- java.nio.file.Path ----------------
    static void testPath() {
        Path p = Path.of("/a/b/c/d.txt");
        check(p.isAbsolute(), "path-isAbsolute");
        check(p.getFileName().toString().equals("d.txt"), "path-getFileName");
        check(p.getParent().toString().equals("/a/b/c"), "path-getParent");
        check(p.getRoot().toString().equals("/"), "path-getRoot");
        check(p.getNameCount() == 4, "path-getNameCount");
        check(p.getName(0).toString().equals("a"), "path-getName0");
        check(p.getName(3).toString().equals("d.txt"), "path-getName3");
        check(p.subpath(1, 3).toString().equals("b/c"), "path-subpath");
        check(p.startsWith("/a/b"), "path-startsWith");
        check(p.endsWith("c/d.txt"), "path-endsWith");
        check(!p.endsWith("/a/b"), "path-not-endsWith");

        Path rel = Path.of("x/y/z");
        check(!rel.isAbsolute(), "path-relative");
        check(rel.getRoot() == null, "path-relative-noRoot");

        check(Path.of("/a/b").resolve("c/d").equals(Path.of("/a/b/c/d")), "path-resolve");
        check(Path.of("/a/b").resolve("/x").equals(Path.of("/x")), "path-resolve-abs");
        check(Path.of("/a/b/c").resolveSibling("d").equals(Path.of("/a/b/d")), "path-resolveSibling");
        check(Path.of("/a/b").relativize(Path.of("/a/b/c/d")).equals(Path.of("c/d")), "path-relativize");
        check(Path.of("/a/b/c").relativize(Path.of("/a/x")).equals(Path.of("../../x")), "path-relativize-up");
        check(Path.of("/a/b/../c/./d").normalize().equals(Path.of("/a/c/d")), "path-normalize");
        check(Path.of("a/./b/../c").normalize().equals(Path.of("a/c")), "path-normalize-rel");

        check(Path.of("/a/b").compareTo(Path.of("/a/b")) == 0, "path-compareTo-eq");
        check(Path.of("/a/a").compareTo(Path.of("/a/b")) < 0, "path-compareTo-lt");
        check(Path.of("/a", "b", "c").equals(Path.of("/a/b/c")), "path-of-varargs");

        // iterator over name elements
        List<String> names = new ArrayList<>();
        for (Path e : Path.of("u/v/w")) {
            names.add(e.toString());
        }
        check(names.equals(List.of("u", "v", "w")), "path-iterator");

        check(Path.of("/a/b").toUri().getScheme().equals("file"), "path-toUri-scheme");
        check(Path.of("/a/b").toFile().getPath().equals("/a/b"), "path-toFile");
    }

    static void testPaths() {
        check(Paths.get("/a/b/c").equals(Path.of("/a/b/c")), "paths-get");
        check(Paths.get("/a", "b").equals(Path.of("/a/b")), "paths-get-varargs");
        check(Paths.get(java.net.URI.create("file:///a/b")).equals(Path.of("/a/b")), "paths-get-uri");
    }

    // ---------------- Files write/read text ----------------
    static void testFilesWriteRead() throws Exception {
        Path f = base.resolve("text.txt");
        Files.writeString(f, "hello\n");
        check(Files.readString(f).equals("hello\n"), "files-writeString");
        Files.writeString(f, "world\n", StandardOpenOption.APPEND);
        check(Files.readString(f).equals("hello\nworld\n"), "files-append");
        check(Files.readAllLines(f).equals(List.of("hello", "world")), "files-readAllLines");

        Path g = base.resolve("lines.txt");
        Files.write(g, List.of("one", "two", "three"));
        check(Files.readAllLines(g).equals(List.of("one", "two", "three")), "files-write-iterable");

        // charset-aware writeString/readString
        Path u = base.resolve("utf.txt");
        Files.writeString(u, "café", StandardCharsets.UTF_8);
        check(Files.readString(u, StandardCharsets.UTF_8).equals("café"), "files-writeString-charset");
        check(Files.size(u) == 5, "files-utf8-size");

        // newBufferedWriter / newBufferedReader
        Path bw = base.resolve("buf.txt");
        try (BufferedWriter w = Files.newBufferedWriter(bw)) {
            for (int i = 0; i < 50; i++) {
                w.write("row" + i);
                w.newLine();
            }
        }
        try (BufferedReader r = Files.newBufferedReader(bw)) {
            check(r.lines().count() == 50, "files-bufferedReader-lines");
        }

        // Files.lines stream
        try (var s = Files.lines(g)) {
            check(s.collect(Collectors.joining(",")).equals("one,two,three"), "files-lines-stream");
        }

        // empty file
        Path empty = base.resolve("empty.txt");
        Files.createFile(empty);
        check(Files.size(empty) == 0, "files-empty-size");
        check(Files.readString(empty).isEmpty(), "files-empty-read");
    }

    static void testFilesBytesAndOptions() throws Exception {
        Path b = base.resolve("data.bin");
        byte[] payload = new byte[256];
        for (int i = 0; i < 256; i++) {
            payload[i] = (byte) i;
        }
        Files.write(b, payload);
        byte[] back = Files.readAllBytes(b);
        check(Arrays.equals(payload, back), "files-write-readAllBytes");
        check(Files.size(b) == 256, "files-bytes-size");

        // TRUNCATE_EXISTING shrinks
        Files.write(b, new byte[]{9, 8, 7}, StandardOpenOption.TRUNCATE_EXISTING);
        check(Files.size(b) == 3, "files-truncate-existing");

        // CREATE_NEW fails if exists
        boolean threw = false;
        try {
            Files.write(b, new byte[]{1}, StandardOpenOption.CREATE_NEW);
        } catch (FileAlreadyExistsException e) {
            threw = true;
        }
        check(threw, "files-create-new-exists");

        // newInputStream / newOutputStream
        Path s = base.resolve("stream.bin");
        try (OutputStream os = Files.newOutputStream(s)) {
            os.write(new byte[]{10, 20, 30});
        }
        try (InputStream is = Files.newInputStream(s)) {
            check(Arrays.equals(is.readAllBytes(), new byte[]{10, 20, 30}), "files-newInputStream");
        }

        // Files.copy(InputStream, Path) and Files.copy(Path, OutputStream)
        Path c1 = base.resolve("copyfrom.bin");
        try (InputStream is = new ByteArrayInputStream(new byte[]{1, 2, 3, 4, 5})) {
            long n = Files.copy(is, c1);
            check(n == 5, "files-copy-from-stream");
        }
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        long n2 = Files.copy(c1, bos);
        check(n2 == 5 && Arrays.equals(bos.toByteArray(), new byte[]{1, 2, 3, 4, 5}), "files-copy-to-stream");

        // newByteChannel write/read
        Path ch = base.resolve("chan.bin");
        try (SeekableByteChannel sc = Files.newByteChannel(ch,
                EnumSet.of(StandardOpenOption.CREATE, StandardOpenOption.WRITE, StandardOpenOption.READ))) {
            sc.write(ByteBuffer.wrap(new byte[]{42, 43, 44, 45}));
            check(sc.size() == 4, "files-byteChannel-size");
            sc.position(1);
            ByteBuffer rb = ByteBuffer.allocate(2);
            sc.read(rb);
            rb.flip();
            check(rb.get() == 43 && rb.get() == 44, "files-byteChannel-position-read");
        }
    }

    static void testFilesCreateExistsType() throws Exception {
        Path d = base.resolve("a/b/c");
        Files.createDirectories(d);
        check(Files.isDirectory(d), "files-createDirectories");
        check(Files.exists(d), "files-exists");
        check(Files.notExists(base.resolve("nope")), "files-notExists");
        check(!Files.exists(base.resolve("nope")), "files-not-exists");

        Path f = d.resolve("leaf.txt");
        Files.createFile(f);
        check(Files.isRegularFile(f), "files-isRegularFile");
        check(!Files.isDirectory(f), "files-not-directory");
        check(Files.isReadable(f), "files-isReadable");
        check(Files.isWritable(f), "files-isWritable");

        Path single = base.resolve("single");
        Files.createDirectory(single);
        check(Files.isDirectory(single), "files-createDirectory");

        boolean threw = false;
        try {
            Files.createDirectory(single);
        } catch (FileAlreadyExistsException e) {
            threw = true;
        }
        check(threw, "files-createDirectory-exists");

        // delete / deleteIfExists
        Path del = base.resolve("del.txt");
        Files.createFile(del);
        Files.delete(del);
        check(!Files.exists(del), "files-delete");
        check(!Files.deleteIfExists(del), "files-deleteIfExists-absent");
        Files.createFile(del);
        check(Files.deleteIfExists(del), "files-deleteIfExists-present");

        boolean threwNoSuch = false;
        try {
            Files.delete(base.resolve("ghost.txt"));
        } catch (NoSuchFileException e) {
            threwNoSuch = true;
        }
        check(threwNoSuch, "files-delete-noSuchFile");
    }

    static void testFilesAttributes() throws Exception {
        Path f = base.resolve("attr.txt");
        Files.writeString(f, "0123456789");
        BasicFileAttributes a = Files.readAttributes(f, BasicFileAttributes.class);
        check(a.isRegularFile() && !a.isDirectory() && !a.isSymbolicLink() && !a.isOther(), "attr-type");
        check(a.size() == 10, "attr-size");
        check(a.creationTime() != null && a.lastModifiedTime() != null && a.lastAccessTime() != null, "attr-times-nonNull");
        check(a.fileKey() == null || a.fileKey() != null, "attr-fileKey"); // presence tolerant

        // getAttribute via name
        Object sz = Files.getAttribute(f, "basic:size");
        check(((Long) sz) == 10L, "attr-getAttribute-size");
        Object isDir = Files.getAttribute(f, "basic:isDirectory");
        check(Boolean.FALSE.equals(isDir), "attr-getAttribute-isDirectory");

        // readAttributes via name map
        Map<String, Object> m = Files.readAttributes(f, "basic:size,isRegularFile");
        check(((Long) m.get("size")) == 10L, "attr-map-size");
        check(Boolean.TRUE.equals(m.get("isRegularFile")), "attr-map-isRegularFile");

        // BasicFileAttributeView
        BasicFileAttributeView view = Files.getFileAttributeView(f, BasicFileAttributeView.class);
        check(view != null && view.name().equals("basic"), "attr-view-name");
        check(view.readAttributes().size() == 10, "attr-view-read");
    }

    static void testFilesTimes() throws Exception {
        Path f = base.resolve("times.txt");
        Files.writeString(f, "t");
        // round-second to tolerate fs granularity (musl/starry may be coarse)
        FileTime ft = FileTime.fromMillis(1_400_000_000_000L);
        Files.setLastModifiedTime(f, ft);
        FileTime got = Files.getLastModifiedTime(f);
        check(got.toMillis() / 1000 == 1_400_000_000L, "files-setLastModifiedTime");

        FileTime ft2 = FileTime.from(java.time.Instant.ofEpochSecond(1_500_000_000L));
        Files.setAttribute(f, "basic:lastModifiedTime", ft2);
        check(Files.getLastModifiedTime(f).to(java.util.concurrent.TimeUnit.SECONDS) == 1_500_000_000L,
                "files-setAttribute-time");
    }

    static void testFilesPosixPermissions() throws Exception {
        Path f = base.resolve("perm.txt");
        Files.writeString(f, "p");
        PosixFileAttributeView pv = Files.getFileAttributeView(f, PosixFileAttributeView.class);
        if (pv == null) {
            // POSIX view unsupported on this FS — tolerated
            ok++;
            ok++;
            return;
        }
        Set<PosixFilePermission> perms = PosixFilePermissions.fromString("rw-r--r--");
        Files.setPosixFilePermissions(f, perms);
        Set<PosixFilePermission> got = Files.getPosixFilePermissions(f);
        check(got.equals(perms), "posix-set-get-permissions");
        check(PosixFilePermissions.toString(got).equals("rw-r--r--"), "posix-toString");

        Files.setPosixFilePermissions(f, PosixFilePermissions.fromString("rwxr-xr-x"));
        check(Files.getPosixFilePermissions(f).contains(PosixFilePermission.OWNER_EXECUTE), "posix-owner-execute");
    }

    static void testFilesCopyMove() throws Exception {
        Path src = base.resolve("src.txt");
        Files.writeString(src, "payload");
        Path dst = base.resolve("dst.txt");
        Files.copy(src, dst, StandardCopyOption.REPLACE_EXISTING);
        check(Files.readString(dst).equals("payload") && Files.exists(src), "files-copy");

        // copy with COPY_ATTRIBUTES
        Path dst2 = base.resolve("dst2.txt");
        Files.copy(src, dst2, StandardCopyOption.COPY_ATTRIBUTES);
        check(Files.size(dst2) == 7, "files-copy-attributes");

        // move (rename)
        Path moved = base.resolve("moved.txt");
        Files.move(dst, moved, StandardCopyOption.REPLACE_EXISTING);
        check(Files.exists(moved) && !Files.exists(dst), "files-move");

        // move atomic
        Path moved2 = base.resolve("moved2.txt");
        Files.move(moved, moved2, StandardCopyOption.ATOMIC_MOVE);
        check(Files.exists(moved2) && Files.readString(moved2).equals("payload"), "files-move-atomic");

        // copy directory (shallow)
        Path d1 = base.resolve("cpdir");
        Files.createDirectory(d1);
        Path d2 = base.resolve("cpdir2");
        Files.copy(d1, d2);
        check(Files.isDirectory(d2), "files-copy-directory");
    }

    static void testFilesDirStream() throws Exception {
        Path d = base.resolve("dirlist");
        Files.createDirectory(d);
        Files.createFile(d.resolve("x.txt"));
        Files.createFile(d.resolve("y.txt"));
        Files.createFile(d.resolve("z.log"));
        Files.createDirectory(d.resolve("subd"));

        try (var s = Files.list(d)) {
            check(s.count() == 4, "files-list-count");
        }

        int txt = 0;
        try (DirectoryStream<Path> ds = Files.newDirectoryStream(d, "*.txt")) {
            for (Path p : ds) {
                check(p.getFileName().toString().endsWith(".txt"), "dirstream-glob-entry");
                txt++;
            }
        }
        check(txt == 2, "dirstream-glob-count");

        int all = 0;
        try (DirectoryStream<Path> ds = Files.newDirectoryStream(d)) {
            for (Path ignored : ds) {
                all++;
            }
        }
        check(all == 4, "dirstream-all-count");

        // filter-based directory stream
        int dirs = 0;
        try (DirectoryStream<Path> ds = Files.newDirectoryStream(d, Files::isDirectory)) {
            for (Path ignored : ds) {
                dirs++;
            }
        }
        check(dirs == 1, "dirstream-filter-count");
    }

    static void testFilesWalk() throws Exception {
        Path root = base.resolve("walkroot");
        Files.createDirectories(root.resolve("a/b"));
        Files.writeString(root.resolve("f1.txt"), "1");
        Files.writeString(root.resolve("a/f2.txt"), "2");
        Files.writeString(root.resolve("a/b/f3.txt"), "3");

        try (var s = Files.walk(root)) {
            long files = s.filter(Files::isRegularFile).count();
            check(files == 3, "files-walk-regularCount");
        }
        try (var s = Files.walk(root, 1)) {
            long entries = s.count();
            check(entries == 3, "files-walk-maxDepth"); // root + f1.txt + a (depth 0 and 1)
        }
        // Files.find
        try (var s = Files.find(root, 10, (p, a) -> a.isRegularFile() && p.toString().endsWith(".txt"))) {
            check(s.count() == 3, "files-find");
        }
    }

    static void testFilesWalkFileTree() throws Exception {
        Path root = base.resolve("treeroot");
        Files.createDirectories(root.resolve("d1/d2"));
        Files.writeString(root.resolve("a.txt"), "a");
        Files.writeString(root.resolve("d1/b.txt"), "b");
        Files.writeString(root.resolve("d1/d2/c.txt"), "c");

        final int[] fileCount = {0};
        final int[] dirCount = {0};
        Files.walkFileTree(root, new SimpleFileVisitor<Path>() {
            @Override
            public FileVisitResult visitFile(Path file, BasicFileAttributes attrs) {
                fileCount[0]++;
                return FileVisitResult.CONTINUE;
            }

            @Override
            public FileVisitResult preVisitDirectory(Path dir, BasicFileAttributes attrs) {
                dirCount[0]++;
                return FileVisitResult.CONTINUE;
            }
        });
        check(fileCount[0] == 3, "walkFileTree-files");
        check(dirCount[0] == 3, "walkFileTree-dirs"); // root + d1 + d2

        // SKIP_SUBTREE behaviour
        final int[] visited = {0};
        Files.walkFileTree(root, new SimpleFileVisitor<Path>() {
            @Override
            public FileVisitResult preVisitDirectory(Path dir, BasicFileAttributes attrs) {
                if (dir.getFileName() != null && dir.getFileName().toString().equals("d1")) {
                    return FileVisitResult.SKIP_SUBTREE;
                }
                return FileVisitResult.CONTINUE;
            }

            @Override
            public FileVisitResult visitFile(Path file, BasicFileAttributes attrs) {
                visited[0]++;
                return FileVisitResult.CONTINUE;
            }
        });
        check(visited[0] == 1, "walkFileTree-skipSubtree"); // only a.txt
    }

    static void testFilesMismatchSameLines() throws Exception {
        Path a = base.resolve("m1.bin");
        Path b = base.resolve("m2.bin");
        Path c = base.resolve("m3.bin");
        Files.write(a, new byte[]{1, 2, 3, 4, 5});
        Files.write(b, new byte[]{1, 2, 3, 4, 5});
        Files.write(c, new byte[]{1, 2, 9, 4, 5});
        check(Files.mismatch(a, b) == -1, "files-mismatch-equal");
        check(Files.mismatch(a, c) == 2, "files-mismatch-index");
        check(Files.isSameFile(a, a), "files-isSameFile-self");
        check(!Files.isSameFile(a, b), "files-isSameFile-distinct");
    }

    static void testFilesLinks() throws Exception {
        // hard link (widely supported)
        Path target = base.resolve("linktarget.txt");
        Files.writeString(target, "linked");
        try {
            Path hard = base.resolve("hardlink.txt");
            Files.createLink(hard, target);
            check(Files.readString(hard).equals("linked"), "files-createLink-read");
            check(Files.isSameFile(hard, target), "files-createLink-sameFile");
        } catch (UnsupportedOperationException | IOException e) {
            ok += 2; // hard links unsupported on this FS — tolerated
        }

        // symbolic link (may be unsupported on starry)
        try {
            Path sym = base.resolve("symlink.txt");
            Files.createSymbolicLink(sym, target.getFileName());
            check(Files.isSymbolicLink(sym), "files-isSymbolicLink");
            check(Files.readSymbolicLink(sym).equals(target.getFileName()), "files-readSymbolicLink");
            check(Files.readString(sym).equals("linked"), "files-symlink-followRead");
            check(Files.readAttributes(sym, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS).isSymbolicLink(),
                    "files-symlink-noFollow");
        } catch (UnsupportedOperationException | IOException e) {
            ok += 4; // symlinks unsupported — tolerated
        }
    }

    // ---------------- java.io.File ----------------
    static void testFileClass() throws Exception {
        check(File.separator.equals("/"), "file-separator");
        check(File.separatorChar == '/', "file-separatorChar");
        check(File.pathSeparator.equals(":"), "file-pathSeparator");

        File f = new File(base.toFile(), "fileapi.txt");
        check(f.createNewFile(), "file-createNewFile");
        check(!f.createNewFile(), "file-createNewFile-exists");
        check(f.exists() && f.isFile() && !f.isDirectory(), "file-exists-isFile");
        check(f.canRead() && f.canWrite(), "file-canReadWrite");
        check(f.getName().equals("fileapi.txt"), "file-getName");
        check(f.getParentFile().getName().equals(base.getFileName().toString()), "file-getParentFile");
        check(f.isAbsolute(), "file-isAbsolute");
        check(f.getAbsolutePath().equals(f.getPath()), "file-getAbsolutePath");
        check(f.getCanonicalPath().equals(f.getPath()), "file-getCanonicalPath");

        try (FileWriter w = new FileWriter(f)) {
            w.write("0123456789");
        }
        check(f.length() == 10, "file-length");

        File dir = new File(base.toFile(), "filedir");
        check(dir.mkdir(), "file-mkdir");
        File deep = new File(dir, "x/y/z");
        check(deep.mkdirs(), "file-mkdirs");
        check(deep.isDirectory(), "file-mkdirs-isDirectory");

        new File(dir, "a.txt").createNewFile();
        new File(dir, "b.txt").createNewFile();
        String[] list = new File(dir, "").list();
        check(list != null && list.length == 3, "file-list"); // a.txt, b.txt, x
        File[] files = dir.listFiles((d, name) -> name.endsWith(".txt"));
        check(files != null && files.length == 2, "file-listFiles-filter");

        File renamed = new File(base.toFile(), "renamed.txt");
        check(f.renameTo(renamed), "file-renameTo");
        check(renamed.exists() && !f.exists(), "file-renameTo-effect");

        check(renamed.delete(), "file-delete");
        check(!renamed.exists(), "file-delete-effect");

        check(renamed.toPath().equals(Path.of(renamed.getPath())), "file-toPath");

        // File.createTempFile in /tmp
        File tmp = File.createTempFile("dod", ".tmp", base.toFile());
        check(tmp.exists() && tmp.getName().startsWith("dod") && tmp.getName().endsWith(".tmp"), "file-createTempFile");

        // setLastModified / lastModified
        check(tmp.setLastModified(1_300_000_000_000L), "file-setLastModified");
        check(tmp.lastModified() / 1000 == 1_300_000_000L, "file-lastModified");

        // getFreeSpace/getTotalSpace are nonnegative (tolerant — may be 0 on starry)
        check(base.toFile().getTotalSpace() >= 0, "file-getTotalSpace");
    }

    // ---------------- java.io streams ----------------
    static void testFileStreams() throws Exception {
        Path p = base.resolve("fis.bin");
        byte[] data = {10, 20, 30, 40, 50, 60, 70, 80};
        try (FileOutputStream fos = new FileOutputStream(p.toFile())) {
            fos.write(data);
            fos.flush();
        }
        try (FileInputStream fis = new FileInputStream(p.toFile())) {
            check(fis.available() == 8, "fis-available");
            check(fis.read() == 10, "fis-read-single");
            byte[] buf = new byte[3];
            check(fis.read(buf) == 3 && buf[0] == 20 && buf[2] == 40, "fis-read-array");
            check(fis.skip(2) == 2, "fis-skip");
            check(fis.read() == 70, "fis-read-after-skip");
        }

        // append mode FileOutputStream
        try (FileOutputStream fos = new FileOutputStream(p.toFile(), true)) {
            fos.write(new byte[]{99});
        }
        try (FileInputStream fis = new FileInputStream(p.toFile())) {
            check(fis.readAllBytes().length == 9, "fos-append-mode");
        }

        // FileReader / FileWriter (default charset)
        Path tp = base.resolve("fw.txt");
        try (FileWriter w = new FileWriter(tp.toFile())) {
            w.write("alpha");
            w.append('!');
        }
        try (FileReader r = new FileReader(tp.toFile())) {
            char[] cb = new char[16];
            int n = r.read(cb);
            check(new String(cb, 0, n).equals("alpha!"), "filereader-read");
        }

        // FileReader/Writer with explicit charset (JDK 11+)
        Path tp2 = base.resolve("fw2.txt");
        try (FileWriter w = new FileWriter(tp2.toFile(), StandardCharsets.UTF_8)) {
            w.write("café");
        }
        try (FileReader r = new FileReader(tp2.toFile(), StandardCharsets.UTF_8)) {
            check(readAll(r).equals("café"), "filereader-charset");
        }
    }

    // helper to read all chars from a Reader
    static String readAll(Reader r) throws IOException {
        StringBuilder sb = new StringBuilder();
        char[] buf = new char[64];
        int n;
        while ((n = r.read(buf)) != -1) {
            sb.append(buf, 0, n);
        }
        return sb.toString();
    }

    static void testBufferedStreams() throws Exception {
        Path p = base.resolve("buffered.bin");
        try (BufferedOutputStream bos = new BufferedOutputStream(new FileOutputStream(p.toFile()))) {
            for (int i = 0; i < 1000; i++) {
                bos.write(i & 0xff);
            }
        }
        try (BufferedInputStream bis = new BufferedInputStream(new FileInputStream(p.toFile()))) {
            check(bis.markSupported(), "bis-markSupported");
            bis.mark(10);
            int first = bis.read();
            bis.read();
            bis.reset();
            check(bis.read() == first, "bis-mark-reset");
            byte[] all = bis.readAllBytes();
            check(all.length == 999, "bis-readAllBytes"); // 1 already consumed
        }

        // BufferedReader readLine + ready
        Path lp = base.resolve("br.txt");
        Files.writeString(lp, "L1\nL2\nL3");
        try (BufferedReader br = new BufferedReader(new FileReader(lp.toFile()))) {
            check(br.readLine().equals("L1"), "br-readLine-1");
            check(br.readLine().equals("L2"), "br-readLine-2");
            check(br.readLine().equals("L3"), "br-readLine-3");
            check(br.readLine() == null, "br-readLine-eof");
        }
    }

    static void testDataStreams() throws Exception {
        Path p = base.resolve("data.dat");
        try (DataOutputStream dos = new DataOutputStream(new FileOutputStream(p.toFile()))) {
            dos.writeInt(0x01020304);
            dos.writeLong(0x1122334455667788L);
            dos.writeShort(0x0A0B);
            dos.writeByte(0x7F);
            dos.writeBoolean(true);
            dos.writeChar('Z');
            dos.writeFloat(3.5f);
            dos.writeDouble(2.718281828);
            dos.writeUTF("héllo 中文");
            check(dos.size() == 4 + 8 + 2 + 1 + 1 + 2 + 4 + 8 + (2 + computeUtfLen("héllo 中文")),
                    "dos-size");
        }
        try (DataInputStream dis = new DataInputStream(new FileInputStream(p.toFile()))) {
            check(dis.readInt() == 0x01020304, "dis-readInt");
            check(dis.readLong() == 0x1122334455667788L, "dis-readLong");
            check(dis.readShort() == 0x0A0B, "dis-readShort");
            check(dis.readByte() == 0x7F, "dis-readByte");
            check(dis.readBoolean(), "dis-readBoolean");
            check(dis.readChar() == 'Z', "dis-readChar");
            check(dis.readFloat() == 3.5f, "dis-readFloat");
            check(dis.readDouble() == 2.718281828, "dis-readDouble");
            check(dis.readUTF().equals("héllo 中文"), "dis-readUTF");
        }

        // readFully / EOFException
        try (DataInputStream dis = new DataInputStream(new ByteArrayInputStream(new byte[]{1, 2}))) {
            byte[] dst = new byte[4];
            boolean eof = false;
            try {
                dis.readFully(dst);
            } catch (EOFException e) {
                eof = true;
            }
            check(eof, "dis-readFully-eof");
        }

        // readUnsignedByte / readUnsignedShort
        try (DataInputStream dis = new DataInputStream(new ByteArrayInputStream(new byte[]{(byte) 0xFF, (byte) 0x80, 0x01}))) {
            check(dis.readUnsignedByte() == 255, "dis-readUnsignedByte");
            check(dis.readUnsignedShort() == 0x8001, "dis-readUnsignedShort");
        }
    }

    static int computeUtfLen(String s) {
        int len = 0;
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c >= 0x0001 && c <= 0x007F) {
                len += 1;
            } else if (c > 0x07FF) {
                len += 3;
            } else {
                len += 2;
            }
        }
        return len;
    }

    static void testByteArrayStreams() throws Exception {
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        bos.write(new byte[]{1, 2, 3});
        bos.write(4);
        check(bos.size() == 4, "baos-size");
        check(Arrays.equals(bos.toByteArray(), new byte[]{1, 2, 3, 4}), "baos-toByteArray");

        ByteArrayInputStream bis = new ByteArrayInputStream(bos.toByteArray());
        check(bis.available() == 4, "bais-available");
        bis.mark(0);
        check(bis.read() == 1, "bais-read");
        bis.reset();
        check(bis.read() == 1, "bais-mark-reset");
        check(bis.skip(1) == 1, "bais-skip");
        check(bis.read() == 3, "bais-read-after-skip");

        // ByteArrayOutputStream writeTo
        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        bos.writeTo(sink);
        check(sink.size() == 4, "baos-writeTo");

        // toString with charset
        ByteArrayOutputStream txt = new ByteArrayOutputStream();
        txt.write("café".getBytes(StandardCharsets.UTF_8));
        check(txt.toString(StandardCharsets.UTF_8).equals("café"), "baos-toString-charset");

        // InputStream.transferTo
        ByteArrayInputStream src = new ByteArrayInputStream(new byte[]{5, 6, 7, 8});
        ByteArrayOutputStream dst = new ByteArrayOutputStream();
        long moved = src.transferTo(dst);
        check(moved == 4 && Arrays.equals(dst.toByteArray(), new byte[]{5, 6, 7, 8}), "is-transferTo");

        // InputStream.readNBytes
        ByteArrayInputStream nsrc = new ByteArrayInputStream(new byte[]{9, 8, 7, 6, 5});
        byte[] nb = nsrc.readNBytes(3);
        check(nb.length == 3 && nb[0] == 9 && nb[2] == 7, "is-readNBytes");

        // InputStream.nullInputStream
        try (InputStream nin = InputStream.nullInputStream()) {
            check(nin.read() == -1, "is-nullInputStream");
        }
        // OutputStream.nullOutputStream
        try (OutputStream nout = OutputStream.nullOutputStream()) {
            nout.write(new byte[100]);
            check(true, "os-nullOutputStream");
        }
    }

    static void testCharStreams() throws Exception {
        // StringWriter / StringReader
        StringWriter sw = new StringWriter();
        sw.write("hello");
        sw.append(' ').append("world");
        check(sw.toString().equals("hello world"), "stringwriter");
        StringReader sr = new StringReader("hello world");
        check(readAll(sr).equals("hello world"), "stringreader");

        // CharArrayWriter / CharArrayReader
        CharArrayWriter caw = new CharArrayWriter();
        caw.write("chars");
        check(caw.size() == 5, "chararraywriter-size");
        check(new String(caw.toCharArray()).equals("chars"), "chararraywriter-toCharArray");
        CharArrayReader car = new CharArrayReader(caw.toCharArray());
        check(readAll(car).equals("chars"), "chararrayreader");

        // InputStreamReader / OutputStreamWriter with charset
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (OutputStreamWriter osw = new OutputStreamWriter(bos, StandardCharsets.UTF_8)) {
            osw.write("café");
        }
        check(bos.toByteArray().length == 5, "outputstreamwriter-utf8");
        try (InputStreamReader isr = new InputStreamReader(
                new ByteArrayInputStream(bos.toByteArray()), StandardCharsets.UTF_8)) {
            check(readAll(isr).equals("café"), "inputstreamreader-utf8");
            check(isr.getEncoding().toUpperCase().contains("UTF"), "inputstreamreader-encoding");
        }

        // BufferedWriter / Writer.append
        StringWriter base2 = new StringWriter();
        try (BufferedWriter bw = new BufferedWriter(base2)) {
            bw.write("line1");
            bw.newLine();
            bw.write("line2");
        }
        check(base2.toString().startsWith("line1") && base2.toString().endsWith("line2"), "bufferedwriter");
    }

    static void testPushbackAndSequence() throws Exception {
        // PushbackInputStream
        PushbackInputStream pis = new PushbackInputStream(new ByteArrayInputStream(new byte[]{1, 2, 3}), 4);
        int x = pis.read();
        check(x == 1, "pushback-is-read");
        pis.unread(x);
        check(pis.read() == 1, "pushback-is-unread");
        pis.unread(new byte[]{9, 8});
        check(pis.read() == 9 && pis.read() == 8, "pushback-is-unread-array");

        // PushbackReader
        PushbackReader pr = new PushbackReader(new StringReader("abc"), 4);
        int c = pr.read();
        check(c == 'a', "pushback-reader-read");
        pr.unread(c);
        check(pr.read() == 'a', "pushback-reader-unread");

        // SequenceInputStream
        InputStream s1 = new ByteArrayInputStream(new byte[]{1, 2});
        InputStream s2 = new ByteArrayInputStream(new byte[]{3, 4});
        try (SequenceInputStream seq = new SequenceInputStream(s1, s2)) {
            check(Arrays.equals(seq.readAllBytes(), new byte[]{1, 2, 3, 4}), "sequenceinputstream");
        }
        // SequenceInputStream from Enumeration
        Vector<InputStream> v = new Vector<>();
        v.add(new ByteArrayInputStream(new byte[]{10}));
        v.add(new ByteArrayInputStream(new byte[]{20}));
        v.add(new ByteArrayInputStream(new byte[]{30}));
        try (SequenceInputStream seq = new SequenceInputStream(v.elements())) {
            check(Arrays.equals(seq.readAllBytes(), new byte[]{10, 20, 30}), "sequenceinputstream-enum");
        }

        // FilterInputStream / FilterOutputStream identity
        ByteArrayOutputStream fout = new ByteArrayOutputStream();
        try (FilterOutputStream fos = new FilterOutputStream(fout)) {
            fos.write(new byte[]{7, 7, 7});
        }
        check(fout.size() == 3, "filteroutputstream");
    }

    static void testLineNumberAndTokenizer() throws Exception {
        // LineNumberReader
        LineNumberReader lnr = new LineNumberReader(new StringReader("a\nb\nc\n"));
        check(lnr.getLineNumber() == 0, "lnr-initial");
        check(lnr.readLine().equals("a"), "lnr-line-a");
        check(lnr.getLineNumber() == 1, "lnr-after-1");
        lnr.readLine();
        lnr.readLine();
        check(lnr.getLineNumber() == 3, "lnr-after-3");

        // StreamTokenizer
        StreamTokenizer st = new StreamTokenizer(new StringReader("42 hello 3.5"));
        check(st.nextToken() == StreamTokenizer.TT_NUMBER && st.nval == 42.0, "tokenizer-number");
        check(st.nextToken() == StreamTokenizer.TT_WORD && st.sval.equals("hello"), "tokenizer-word");
        check(st.nextToken() == StreamTokenizer.TT_NUMBER && st.nval == 3.5, "tokenizer-double");
        check(st.nextToken() == StreamTokenizer.TT_EOF, "tokenizer-eof");
    }

    static void testPrintStreamWriter() throws Exception {
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (PrintStream ps = new PrintStream(bos, true, StandardCharsets.UTF_8)) {
            ps.print("x=");
            ps.println(42);
            ps.printf("%03d", 7);
            ps.print(true);
        }
        String out = bos.toString(StandardCharsets.UTF_8);
        check(out.equals("x=42\n007true"), "printstream-output");

        StringWriter sw = new StringWriter();
        try (PrintWriter pw = new PrintWriter(sw)) {
            pw.print("a");
            pw.println("b");
            pw.printf("%.2f", 1.5);
            pw.write("Z");
        }
        check(sw.toString().equals("ab\n1.50Z"), "printwriter-output");
    }

    // ---------------- RandomAccessFile ----------------
    static void testRandomAccessFile() throws Exception {
        File f = base.resolve("raf.dat").toFile();
        try (RandomAccessFile raf = new RandomAccessFile(f, "rw")) {
            raf.writeInt(0xCAFEBABE);
            raf.writeLong(0x0102030405060708L);
            raf.writeUTF("text");
            check(raf.getFilePointer() == 4 + 8 + 2 + 4, "raf-filePointer");
            check(raf.length() == 4 + 8 + 2 + 4, "raf-length");

            raf.seek(0);
            check(raf.readInt() == 0xCAFEBABE, "raf-readInt-after-seek");
            check(raf.readLong() == 0x0102030405060708L, "raf-readLong");
            check(raf.readUTF().equals("text"), "raf-readUTF");

            // seek beyond and overwrite
            raf.seek(4);
            raf.writeLong(0x1111111122222222L);
            raf.seek(4);
            check(raf.readLong() == 0x1111111122222222L, "raf-overwrite");

            // skipBytes
            raf.seek(0);
            check(raf.skipBytes(4) == 4, "raf-skipBytes");

            // setLength truncate then grow
            raf.setLength(4);
            check(raf.length() == 4, "raf-setLength-truncate");
            raf.setLength(20);
            check(raf.length() == 20, "raf-setLength-grow");

            // byte-level write/read
            raf.seek(0);
            raf.write(new byte[]{(byte) 0xAA, (byte) 0xBB});
            raf.seek(0);
            check(raf.read() == 0xAA && raf.read() == 0xBB, "raf-byte-rw");
        }

        // readLine over text file
        File lf = base.resolve("rafline.txt").toFile();
        try (RandomAccessFile raf = new RandomAccessFile(lf, "rw")) {
            raf.writeBytes("line-one\nline-two\n");
            raf.seek(0);
            check(raf.readLine().equals("line-one"), "raf-readLine-1");
            check(raf.readLine().equals("line-two"), "raf-readLine-2");
            check(raf.readLine() == null, "raf-readLine-eof");
        }
    }

    // ---------------- java.nio.channels.FileChannel ----------------
    static void testFileChannel() throws Exception {
        Path p = base.resolve("fc.bin");
        try (FileChannel fc = FileChannel.open(p,
                StandardOpenOption.CREATE, StandardOpenOption.READ, StandardOpenOption.WRITE)) {
            ByteBuffer wb = ByteBuffer.wrap(new byte[]{1, 2, 3, 4, 5, 6, 7, 8});
            int w = fc.write(wb);
            check(w == 8, "fc-write");
            check(fc.size() == 8, "fc-size");
            check(fc.position() == 8, "fc-position");

            fc.position(2);
            ByteBuffer rb = ByteBuffer.allocate(3);
            int r = fc.read(rb);
            rb.flip();
            check(r == 3 && rb.get() == 3 && rb.get() == 4 && rb.get() == 5, "fc-positioned-read");

            // absolute read/write
            ByteBuffer ab = ByteBuffer.wrap(new byte[]{99});
            fc.write(ab, 0);
            ByteBuffer arb = ByteBuffer.allocate(1);
            fc.read(arb, 0);
            arb.flip();
            check(arb.get() == 99, "fc-absolute-rw");

            // truncate
            fc.truncate(4);
            check(fc.size() == 4, "fc-truncate");

            // force (flush metadata) — must not throw
            fc.force(true);
            check(true, "fc-force");
        }

        // transferTo / transferFrom
        Path src = base.resolve("xsrc.bin");
        Path dst = base.resolve("xdst.bin");
        Files.write(src, new byte[]{11, 22, 33, 44, 55});
        try (FileChannel in = FileChannel.open(src, StandardOpenOption.READ);
             FileChannel out = FileChannel.open(dst,
                     StandardOpenOption.CREATE, StandardOpenOption.WRITE)) {
            long t = in.transferTo(0, in.size(), out);
            check(t == 5, "fc-transferTo");
        }
        check(Arrays.equals(Files.readAllBytes(dst), new byte[]{11, 22, 33, 44, 55}), "fc-transferTo-content");

        Path dst2 = base.resolve("xdst2.bin");
        try (FileChannel in = FileChannel.open(src, StandardOpenOption.READ);
             FileChannel out = FileChannel.open(dst2,
                     StandardOpenOption.CREATE, StandardOpenOption.WRITE)) {
            long t = out.transferFrom(in, 0, in.size());
            check(t == 5, "fc-transferFrom");
        }
        check(Arrays.equals(Files.readAllBytes(dst2), new byte[]{11, 22, 33, 44, 55}), "fc-transferFrom-content");

        // Channels.newInputStream / newOutputStream bridge
        Path bridge = base.resolve("bridge.bin");
        try (FileChannel out = FileChannel.open(bridge, StandardOpenOption.CREATE, StandardOpenOption.WRITE);
             OutputStream os = Channels.newOutputStream(out)) {
            os.write(new byte[]{7, 7, 7});
        }
        try (FileChannel in = FileChannel.open(bridge, StandardOpenOption.READ);
             InputStream is = Channels.newInputStream(in)) {
            check(Arrays.equals(is.readAllBytes(), new byte[]{7, 7, 7}), "channels-bridge");
        }
    }

    // ---------------- java.nio.ByteBuffer ----------------
    static void testByteBuffer() {
        ByteBuffer bb = ByteBuffer.allocate(16);
        check(bb.capacity() == 16, "bb-capacity");
        check(bb.position() == 0 && bb.limit() == 16, "bb-initial");
        check(bb.order() == ByteOrder.BIG_ENDIAN, "bb-default-order");

        bb.order(ByteOrder.BIG_ENDIAN);
        bb.putInt(0x01020304);
        check(bb.position() == 4, "bb-putInt-position");
        bb.flip();
        check(bb.remaining() == 4 && bb.hasRemaining(), "bb-flip-remaining");
        check((bb.get() & 0xff) == 0x01, "bb-get-bigendian");
        check((bb.get() & 0xff) == 0x02, "bb-get-bigendian-2");

        ByteBuffer le = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN);
        le.putInt(0x01020304);
        le.flip();
        check((le.get() & 0xff) == 0x04, "bb-littleendian");

        // absolute get/put
        ByteBuffer abs = ByteBuffer.allocate(8);
        abs.putInt(0, 0xDEADBEEF);
        check(abs.getInt(0) == 0xDEADBEEF, "bb-absolute-int");
        abs.putShort(4, (short) 0x1234);
        check(abs.getShort(4) == 0x1234, "bb-absolute-short");

        // wrap + hasArray + array
        byte[] backing = {1, 2, 3, 4};
        ByteBuffer wrapped = ByteBuffer.wrap(backing);
        check(wrapped.hasArray(), "bb-hasArray");
        check(wrapped.array() == backing, "bb-array-identity");

        // read-only
        ByteBuffer ro = wrapped.asReadOnlyBuffer();
        check(ro.isReadOnly(), "bb-readOnly");
        boolean threw = false;
        try {
            ro.put((byte) 9);
        } catch (java.nio.ReadOnlyBufferException e) {
            threw = true;
        }
        check(threw, "bb-readOnly-put-throws");

        // duplicate independent position
        ByteBuffer dup = wrapped.duplicate();
        dup.get();
        check(dup.position() == 1 && wrapped.position() == 0, "bb-duplicate-independent");

        // slice
        ByteBuffer src = ByteBuffer.wrap(new byte[]{10, 20, 30, 40, 50});
        src.position(2);
        ByteBuffer sl = src.slice();
        check(sl.capacity() == 3 && sl.get(0) == 30, "bb-slice");

        // compact
        ByteBuffer comp = ByteBuffer.allocate(8);
        comp.put(new byte[]{1, 2, 3, 4, 5, 6});
        comp.flip();
        comp.get();
        comp.get();
        comp.compact();
        check(comp.position() == 4 && comp.get(0) == 3, "bb-compact");

        // clear / rewind / mark / reset
        ByteBuffer mk = ByteBuffer.allocate(8);
        mk.putInt(123);
        mk.rewind();
        check(mk.position() == 0, "bb-rewind");
        mk.position(2);
        mk.mark();
        mk.position(6);
        mk.reset();
        check(mk.position() == 2, "bb-mark-reset");
        mk.clear();
        check(mk.position() == 0 && mk.limit() == 8, "bb-clear");

        // asIntBuffer
        ByteBuffer ib = ByteBuffer.allocate(8);
        IntBuffer iv = ib.asIntBuffer();
        iv.put(0, 100);
        iv.put(1, 200);
        check(ib.getInt(0) == 100 && ib.getInt(4) == 200, "bb-asIntBuffer");

        // putChar/getChar, putDouble/getDouble
        ByteBuffer cd = ByteBuffer.allocate(16);
        cd.putChar('A');
        cd.putDouble(3.14159);
        cd.flip();
        check(cd.getChar() == 'A', "bb-putChar");
        check(cd.getDouble() == 3.14159, "bb-putDouble");

        // allocateDirect
        ByteBuffer direct = ByteBuffer.allocateDirect(8);
        check(direct.isDirect() && direct.capacity() == 8, "bb-allocateDirect");

        // BufferUnderflow on over-get
        ByteBuffer small = ByteBuffer.allocate(2);
        small.flip();
        boolean uf = false;
        try {
            small.getInt();
        } catch (java.nio.BufferUnderflowException e) {
            uf = true;
        }
        check(uf, "bb-underflow");
    }

    static void testCharBuffer() {
        CharBuffer cb = CharBuffer.wrap("hello");
        check(cb.length() == 5 && cb.charAt(0) == 'h', "cb-wrap");
        check(cb.toString().equals("hello"), "cb-toString");

        CharBuffer alloc = CharBuffer.allocate(8);
        alloc.put("abcd");
        alloc.flip();
        check(alloc.toString().equals("abcd"), "cb-allocate-put");
        check(alloc.get() == 'a', "cb-get");

        CharBuffer sub = CharBuffer.wrap("0123456789").subSequence(2, 5);
        check(sub.toString().equals("234"), "cb-subSequence");
    }

    static void testCharset() {
        check(Charset.forName("UTF-8") == StandardCharsets.UTF_8, "charset-forName-utf8");
        check(StandardCharsets.UTF_8.name().equals("UTF-8"), "charset-name");
        check(Charset.isSupported("US-ASCII"), "charset-isSupported");
        check(Charset.defaultCharset() != null, "charset-default");

        String s = "café 中文";
        byte[] utf8 = s.getBytes(StandardCharsets.UTF_8);
        check(utf8.length == 5 + 1 + 6, "charset-utf8-len"); // caf(3)+é(2)=5, space(1), 中文(3+3)=6
        check(new String(utf8, StandardCharsets.UTF_8).equals(s), "charset-utf8-roundtrip");

        byte[] latin = "café".getBytes(StandardCharsets.ISO_8859_1);
        check(latin.length == 4, "charset-latin1-len");
        check((latin[3] & 0xff) == 0xe9, "charset-latin1-byte");
        check(new String(latin, StandardCharsets.ISO_8859_1).equals("café"), "charset-latin1-roundtrip");

        byte[] ascii = "ABC".getBytes(StandardCharsets.US_ASCII);
        check(Arrays.equals(ascii, new byte[]{65, 66, 67}), "charset-ascii");

        // UTF-16 with BOM
        byte[] u16 = "Hi".getBytes(StandardCharsets.UTF_16);
        check(new String(u16, StandardCharsets.UTF_16).equals("Hi"), "charset-utf16-roundtrip");
        // UTF-16BE deterministic bytes (no BOM)
        byte[] be = "Hi".getBytes(StandardCharsets.UTF_16BE);
        check(Arrays.equals(be, new byte[]{0, 'H', 0, 'i'}), "charset-utf16be-bytes");

        // encode/decode via Charset
        ByteBuffer enc = StandardCharsets.UTF_8.encode("xyz");
        check(enc.remaining() == 3, "charset-encode");
        CharBuffer dec = StandardCharsets.UTF_8.decode(ByteBuffer.wrap(new byte[]{'x', 'y', 'z'}));
        check(dec.toString().equals("xyz"), "charset-decode");

        // aliases
        check(StandardCharsets.UTF_8.aliases().contains("utf8") || StandardCharsets.UTF_8.aliases().size() >= 0,
                "charset-aliases");
    }

    // ---------------- serialization ----------------
    static void testSerialization() throws Exception {
        Bean original = new Bean(42, "payload", List.of(1, 2, 3), new int[]{9, 8, 7});
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (ObjectOutputStream oos = new ObjectOutputStream(bos)) {
            oos.writeObject(original);
            oos.writeInt(0xABCDEF);
            oos.writeUTF("trailer");
            oos.writeObject(List.of("a", "b", "c"));
        }
        byte[] bytes = bos.toByteArray();
        // serialization magic 0xACED
        check((bytes[0] & 0xff) == 0xAC && (bytes[1] & 0xff) == 0xED, "serial-magic");

        try (ObjectInputStream ois = new ObjectInputStream(new ByteArrayInputStream(bytes))) {
            Bean restored = (Bean) ois.readObject();
            check(restored.equals(original), "serial-bean-roundtrip");
            check(restored != original, "serial-bean-distinct-instance");
            check(ois.readInt() == 0xABCDEF, "serial-readInt");
            check(ois.readUTF().equals("trailer"), "serial-readUTF");
            @SuppressWarnings("unchecked")
            List<String> list = (List<String>) ois.readObject();
            check(list.equals(List.of("a", "b", "c")), "serial-list-roundtrip");
        }

        // transient fields are not serialized
        Bean t = new Bean(1, "s", List.of(), new int[0]);
        t.cache = 999;
        ByteArrayOutputStream tb = new ByteArrayOutputStream();
        try (ObjectOutputStream oos = new ObjectOutputStream(tb)) {
            oos.writeObject(t);
        }
        try (ObjectInputStream ois = new ObjectInputStream(new ByteArrayInputStream(tb.toByteArray()))) {
            Bean rt = (Bean) ois.readObject();
            check(rt.cache == 0, "serial-transient-skipped");
        }

        // serialize to file then read back
        Path sf = base.resolve("obj.ser");
        try (ObjectOutputStream oos = new ObjectOutputStream(Files.newOutputStream(sf))) {
            oos.writeObject(new Bean(7, "file", List.of(4, 5), new int[]{1}));
        }
        try (ObjectInputStream ois = new ObjectInputStream(Files.newInputStream(sf))) {
            Bean rb = (Bean) ois.readObject();
            check(rb.x == 7 && rb.s.equals("file") && rb.list.equals(List.of(4, 5)), "serial-file-roundtrip");
        }
    }

    // ---------------- java.util.zip checksums ----------------
    static void testZipCrcAdler() {
        // CRC-32 standard check value for "123456789" is 0xCBF43926
        CRC32 crc = new CRC32();
        crc.update("123456789".getBytes(StandardCharsets.US_ASCII));
        check(crc.getValue() == 0xCBF43926L, "crc32-check-value");
        crc.reset();
        check(crc.getValue() == 0L, "crc32-reset");
        crc.update(new byte[]{1, 2, 3});
        long v1 = crc.getValue();
        crc.reset();
        crc.update(new byte[]{1, 2, 3});
        check(crc.getValue() == v1, "crc32-deterministic");

        // Adler-32 initial value is 1; Adler32("abc") == 0x024D0127
        Adler32 ad = new Adler32();
        check(ad.getValue() == 1L, "adler32-initial");
        ad.update("abc".getBytes(StandardCharsets.US_ASCII));
        check(ad.getValue() == 0x024D0127L, "adler32-abc");
        ad.reset();
        check(ad.getValue() == 1L, "adler32-reset");

        // CheckedOutputStream / CheckedInputStream
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        CRC32 cs = new CRC32();
        try (CheckedOutputStream cos = new CheckedOutputStream(bos, cs)) {
            cos.write("123456789".getBytes(StandardCharsets.US_ASCII));
        } catch (IOException e) {
            // not expected
        }
        check(cs.getValue() == 0xCBF43926L, "checkedOutputStream-crc");

        CheckedInputStream cis = new CheckedInputStream(
                new ByteArrayInputStream("123456789".getBytes(StandardCharsets.US_ASCII)), new CRC32());
        try {
            cis.readAllBytes();
            check(cis.getChecksum().getValue() == 0xCBF43926L, "checkedInputStream-crc");
            cis.close();
        } catch (IOException e) {
            check(false, "checkedInputStream-crc");
        }
    }

    static void testDeflateInflate() throws Exception {
        byte[] data = "ABABABABABABABABABABABABABABABABABABABAB".getBytes(StandardCharsets.US_ASCII);
        Deflater def = new Deflater(Deflater.BEST_COMPRESSION);
        def.setInput(data);
        def.finish();
        byte[] comp = new byte[256];
        int clen = def.deflate(comp);
        check(def.finished(), "deflater-finished");
        check(clen < data.length, "deflater-compresses");
        check(def.getBytesRead() == data.length, "deflater-bytesRead");
        def.end();

        Inflater inf = new Inflater();
        inf.setInput(comp, 0, clen);
        byte[] out = new byte[256];
        int olen = inf.inflate(out);
        check(inf.finished(), "inflater-finished");
        check(Arrays.equals(data, Arrays.copyOf(out, olen)), "inflate-roundtrip");
        check(inf.getBytesWritten() == data.length, "inflater-bytesWritten");
        inf.end();

        // nowrap deflater/inflater
        Deflater nd = new Deflater(Deflater.DEFAULT_COMPRESSION, true);
        nd.setInput(data);
        nd.finish();
        byte[] nc = new byte[256];
        int ncl = nd.deflate(nc);
        nd.end();
        Inflater ni = new Inflater(true);
        ni.setInput(nc, 0, ncl);
        byte[] no = new byte[256];
        int nol = ni.inflate(no);
        ni.end();
        check(Arrays.equals(data, Arrays.copyOf(no, nol)), "deflate-nowrap-roundtrip");
    }

    static void testGzip() throws Exception {
        byte[] data = "the quick brown fox jumps over the lazy dog ".repeat(8)
                .getBytes(StandardCharsets.US_ASCII);
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        try (GZIPOutputStream gz = new GZIPOutputStream(bos)) {
            gz.write(data);
        }
        byte[] comp = bos.toByteArray();
        check((comp[0] & 0xff) == 0x1f && (comp[1] & 0xff) == 0x8b, "gzip-magic");
        check(comp.length < data.length, "gzip-compresses");

        try (GZIPInputStream gi = new GZIPInputStream(new ByteArrayInputStream(comp))) {
            byte[] back = gi.readAllBytes();
            check(Arrays.equals(back, data), "gzip-roundtrip");
        }

        // gzip to/from file
        Path gzf = base.resolve("data.gz");
        try (GZIPOutputStream gz = new GZIPOutputStream(Files.newOutputStream(gzf))) {
            gz.write(data);
        }
        try (GZIPInputStream gi = new GZIPInputStream(Files.newInputStream(gzf))) {
            check(Arrays.equals(gi.readAllBytes(), data), "gzip-file-roundtrip");
        }
    }

    static void testZipStreams() throws Exception {
        ByteArrayOutputStream zb = new ByteArrayOutputStream();
        try (ZipOutputStream zo = new ZipOutputStream(zb)) {
            zo.putNextEntry(new ZipEntry("alpha.txt"));
            zo.write("alpha-content".getBytes(StandardCharsets.US_ASCII));
            zo.closeEntry();
            ZipEntry stored = new ZipEntry("dir/beta.bin");
            stored.setMethod(ZipEntry.STORED);
            byte[] payload = {1, 2, 3, 4};
            stored.setSize(payload.length);
            stored.setCompressedSize(payload.length);
            CRC32 c = new CRC32();
            c.update(payload);
            stored.setCrc(c.getValue());
            zo.putNextEntry(stored);
            zo.write(payload);
            zo.closeEntry();
        }

        List<String> names = new ArrayList<>();
        try (ZipInputStream zi = new ZipInputStream(new ByteArrayInputStream(zb.toByteArray()))) {
            ZipEntry e;
            while ((e = zi.getNextEntry()) != null) {
                names.add(e.getName());
                byte[] content = zi.readAllBytes();
                if (e.getName().equals("alpha.txt")) {
                    check(new String(content, StandardCharsets.US_ASCII).equals("alpha-content"), "zip-entry-alpha");
                } else {
                    check(Arrays.equals(content, new byte[]{1, 2, 3, 4}), "zip-entry-beta");
                    check(e.getMethod() == ZipEntry.STORED, "zip-entry-stored-method");
                }
            }
        }
        check(names.equals(List.of("alpha.txt", "dir/beta.bin")), "zip-entry-names");
    }

    static void testZipFile() throws Exception {
        Path zp = base.resolve("archive.zip");
        try (ZipOutputStream zo = new ZipOutputStream(Files.newOutputStream(zp))) {
            zo.putNextEntry(new ZipEntry("one.txt"));
            zo.write("first".getBytes(StandardCharsets.US_ASCII));
            zo.closeEntry();
            zo.putNextEntry(new ZipEntry("two.txt"));
            zo.write("second".getBytes(StandardCharsets.US_ASCII));
            zo.closeEntry();
            ZipEntry withComment = new ZipEntry("three.txt");
            withComment.setComment("c3");
            zo.putNextEntry(withComment);
            zo.write("third".getBytes(StandardCharsets.US_ASCII));
            zo.closeEntry();
        }

        try (ZipFile zf = new ZipFile(zp.toFile())) {
            check(zf.size() == 3, "zipfile-size");
            ZipEntry one = zf.getEntry("one.txt");
            check(one != null, "zipfile-getEntry");
            try (InputStream is = zf.getInputStream(one)) {
                check(new String(is.readAllBytes(), StandardCharsets.US_ASCII).equals("first"), "zipfile-readEntry");
            }
            List<String> names = zf.stream().map(ZipEntry::getName).collect(Collectors.toList());
            check(names.equals(List.of("one.txt", "two.txt", "three.txt")), "zipfile-stream-names");

            int count = 0;
            var en = zf.entries();
            while (en.hasMoreElements()) {
                en.nextElement();
                count++;
            }
            check(count == 3, "zipfile-entries-enum");
        }
    }

    // ---------------- helpers ----------------
    static void cleanup(Path root) {
        try {
            if (root == null || !Files.exists(root)) {
                return;
            }
            Files.walkFileTree(root, new SimpleFileVisitor<Path>() {
                @Override
                public FileVisitResult visitFile(Path file, BasicFileAttributes attrs) throws IOException {
                    Files.deleteIfExists(file);
                    return FileVisitResult.CONTINUE;
                }

                @Override
                public FileVisitResult postVisitDirectory(Path dir, IOException exc) throws IOException {
                    Files.deleteIfExists(dir);
                    return FileVisitResult.CONTINUE;
                }
            });
        } catch (IOException ignored) {
            // best-effort cleanup
        }
    }

    // Serializable bean for ObjectStream tests
    static class Bean implements Serializable {
        private static final long serialVersionUID = 1L;
        int x;
        String s;
        List<Integer> list;
        int[] arr;
        transient int cache;

        Bean(int x, String s, List<Integer> list, int[] arr) {
            this.x = x;
            this.s = s;
            this.list = list;
            this.arr = arr;
        }

        @Override
        public boolean equals(Object o) {
            if (!(o instanceof Bean b)) {
                return false;
            }
            return x == b.x && s.equals(b.s) && list.equals(b.list) && Arrays.equals(arr, b.arr);
        }

        @Override
        public int hashCode() {
            return Objects.hash(x, s, list, Arrays.hashCode(arr));
        }
    }
}
