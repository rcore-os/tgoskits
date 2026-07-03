import java.io.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import java.lang.ProcessBuilder.Redirect;
import java.lang.ProcessBuilder.Redirect.Type;

/*
 * Carpet coverage for the JDK process-management package surface:
 *   java.lang.ProcessBuilder (construction, command/directory/environment,
 *     redirect{Input,Output,Error}, redirectErrorStream, inheritIO, startPipeline)
 *   java.lang.ProcessBuilder.Redirect / Redirect.Type (PIPE/INHERIT/READ/WRITE/APPEND,
 *     to/from/appendTo/DISCARD, equality)
 *   java.lang.Process (start, getInput/Output/ErrorStream, waitFor, waitFor(timeout),
 *     exitValue, isAlive, destroy, destroyForcibly, pid, toHandle, info, onExit,
 *     supportsNormalTermination)
 *   java.lang.ProcessHandle + ProcessHandle.Info (current/of/pid/isAlive/parent/info/
 *     compareTo/equals/hashCode/supportsNormalTermination/allProcesses)
 *   java.lang.Runtime (getRuntime, availableProcessors, *Memory, gc, version, exec*)
 * Pure JDK17. Deterministic, offline, self-built data. Spawns only busybox-equivalent
 * commands present in the rootfs. Exercises fork/execve/waitpid/pipe/dup syscalls.
 */
public class ProcessTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String n) { if (c) ok++; else { fail++; System.out.println("FAIL " + n); } }

    static final String SH    = "/bin/sh";
    static final String ECHO  = "/bin/echo";
    static final String CAT   = "/bin/cat";
    static final String TRUE_ = "/bin/true";
    static final String FALSE_= "/bin/false";
    static final String SLEEP = "/bin/sleep";
    static final String SORT  = "/bin/sort";

    static String drain(InputStream is) throws IOException {
        return new String(is.readAllBytes()).trim();
    }
    static String run(ProcessBuilder pb) throws Exception {
        pb.redirectErrorStream(true);
        Process p = pb.start();
        String out = drain(p.getInputStream());
        p.waitFor(20, TimeUnit.SECONDS);
        return out;
    }

    interface Sec { void run() throws Exception; }
    static void sec(String name, Sec s) {
        try { s.run(); }
        catch (Throwable t) { fail++; System.out.println("FAIL section:" + name + " : " + t); }
    }

    // ---- A. ProcessBuilder construction / command / directory / environment (no spawn) ----
    static void sectionBuilderConfig() {
        // varargs constructor + command() live view
        ProcessBuilder pb = new ProcessBuilder("/bin/echo", "a", "b");
        check(pb.command().equals(Arrays.asList("/bin/echo", "a", "b")), "pb-varargs-command");
        check(pb.command().size() == 3, "pb-command-size");

        // List constructor
        ProcessBuilder pb2 = new ProcessBuilder(new ArrayList<>(List.of("x", "y")));
        check(pb2.command().equals(List.of("x", "y")), "pb-list-ctor");

        // command(String...) setter replaces and returns this
        ProcessBuilder ret = pb2.command("p", "q", "r");
        check(ret == pb2, "pb-command-returns-this");
        check(pb2.command().equals(List.of("p", "q", "r")), "pb-command-varargs-set");

        // command(List) setter (mutable backing list)
        pb2.command(new ArrayList<>(List.of("only")));
        check(pb2.command().equals(List.of("only")), "pb-command-list-set");

        // command() returns the live backing list (mutations visible)
        pb2.command().add("more");
        check(pb2.command().equals(List.of("only", "more")), "pb-command-live");

        // directory(): default null, then settable
        check(pb.directory() == null, "pb-directory-default-null");
        File d = new File("/tmp");
        ProcessBuilder dr = pb.directory(d);
        check(dr == pb, "pb-directory-returns-this");
        check(d.equals(pb.directory()), "pb-directory-set");
        pb.directory(null);
        check(pb.directory() == null, "pb-directory-reset-null");

        // environment(): mutable, cached (same instance), supports map ops
        Map<String,String> env = pb.environment();
        check(env != null, "pb-env-nonnull");
        check(pb.environment() == env, "pb-env-cached-same-instance");
        env.clear();
        check(env.isEmpty() && env.size() == 0, "pb-env-clear");
        env.put("K1", "V1");
        check("V1".equals(env.get("K1")), "pb-env-put-get");
        int before = env.size();
        env.put("K2", "V2");
        check(env.size() == before + 1, "pb-env-size-grow");
        check(env.containsKey("K2"), "pb-env-containsKey");
        check(env.containsValue("V2"), "pb-env-containsValue");
        check(env.keySet().contains("K1"), "pb-env-keySet");
        int entries = 0; for (Map.Entry<String,String> e : env.entrySet()) entries++;
        check(entries == env.size(), "pb-env-entrySet-iter");
        env.remove("K2");
        check(!env.containsKey("K2"), "pb-env-remove");
        check(env.size() == 1, "pb-env-size-after-remove");

        // redirect defaults
        ProcessBuilder fresh = new ProcessBuilder("x");
        check(fresh.redirectInput()  == Redirect.PIPE, "pb-default-in-pipe");
        check(fresh.redirectOutput() == Redirect.PIPE, "pb-default-out-pipe");
        check(fresh.redirectError()  == Redirect.PIPE, "pb-default-err-pipe");
        check(fresh.redirectErrorStream() == false, "pb-default-merge-false");

        // redirectErrorStream setter + getter
        ProcessBuilder mret = fresh.redirectErrorStream(true);
        check(mret == fresh, "pb-merge-returns-this");
        check(fresh.redirectErrorStream(), "pb-merge-set-true");

        // inheritIO sets all three to INHERIT
        ProcessBuilder ih = new ProcessBuilder("x").inheritIO();
        check(ih.redirectInput()  == Redirect.INHERIT, "pb-inheritIO-in");
        check(ih.redirectOutput() == Redirect.INHERIT, "pb-inheritIO-out");
        check(ih.redirectError()  == Redirect.INHERIT, "pb-inheritIO-err");
    }

    // ---- B. Redirect / Redirect.Type value matrix (no spawn) ----
    static void sectionRedirect() {
        // Type enum completeness
        check(Type.values().length == 5, "redirect-type-count");
        check(Type.valueOf("PIPE")    == Type.PIPE,    "redirect-type-PIPE");
        check(Type.valueOf("INHERIT") == Type.INHERIT, "redirect-type-INHERIT");
        check(Type.valueOf("READ")    == Type.READ,    "redirect-type-READ");
        check(Type.valueOf("WRITE")   == Type.WRITE,   "redirect-type-WRITE");
        check(Type.valueOf("APPEND")  == Type.APPEND,  "redirect-type-APPEND");

        // Singleton redirects
        check(Redirect.PIPE.type()    == Type.PIPE,    "redirect-PIPE-type");
        check(Redirect.PIPE.file()    == null,         "redirect-PIPE-file-null");
        check(Redirect.INHERIT.type() == Type.INHERIT, "redirect-INHERIT-type");
        check(Redirect.INHERIT.file() == null,         "redirect-INHERIT-file-null");
        check(Redirect.DISCARD.type() == Type.WRITE,   "redirect-DISCARD-type-WRITE");
        check(Redirect.DISCARD.file() != null,         "redirect-DISCARD-file-nonnull");
        check(Redirect.PIPE == Redirect.PIPE,          "redirect-PIPE-singleton");

        // File-based factories
        File f = new File("/tmp/proc-redirect-marker");
        Redirect to = Redirect.to(f);
        check(to.type() == Type.WRITE, "redirect-to-type");
        check(f.equals(to.file()),     "redirect-to-file");
        Redirect from = Redirect.from(f);
        check(from.type() == Type.READ, "redirect-from-type");
        check(f.equals(from.file()),    "redirect-from-file");
        Redirect app = Redirect.appendTo(f);
        check(app.type() == Type.APPEND, "redirect-appendTo-type");
        check(f.equals(app.file()),      "redirect-appendTo-file");

        // equality / inequality
        check(Redirect.to(f).equals(Redirect.to(f)),     "redirect-to-equals");
        check(!Redirect.to(f).equals(Redirect.from(f)),  "redirect-to-ne-from");
        check(!Redirect.to(f).equals(Redirect.appendTo(f)), "redirect-to-ne-append");
        check(Redirect.to(f).hashCode() == Redirect.to(f).hashCode(), "redirect-to-hashcode");

        // applying redirects to a builder and reading them back
        ProcessBuilder pb = new ProcessBuilder("x");
        pb.redirectOutput(to);
        check(pb.redirectOutput() == to, "pb-redirectOutput-getset");
        pb.redirectInput(from);
        check(pb.redirectInput() == from, "pb-redirectInput-getset");
        pb.redirectError(Redirect.DISCARD);
        check(pb.redirectError() == Redirect.DISCARD, "pb-redirectError-getset");

        // File convenience overloads produce the corresponding Redirect type
        ProcessBuilder pb2 = new ProcessBuilder("x");
        pb2.redirectOutput(f);
        check(pb2.redirectOutput().type() == Type.WRITE && f.equals(pb2.redirectOutput().file()),
              "pb-redirectOutput-file");
        pb2.redirectInput(f);
        check(pb2.redirectInput().type() == Type.READ && f.equals(pb2.redirectInput().file()),
              "pb-redirectInput-file");
        pb2.redirectError(f);
        check(pb2.redirectError().type() == Type.WRITE && f.equals(pb2.redirectError().file()),
              "pb-redirectError-file");

        // illegal-argument contracts: WRITE redirect for input, READ redirect for output
        try { new ProcessBuilder("x").redirectInput(Redirect.to(f)); check(false, "redirectInput-WRITE-illegal"); }
        catch (IllegalArgumentException e) { check(true, "redirectInput-WRITE-illegal"); }
        try { new ProcessBuilder("x").redirectOutput(Redirect.from(f)); check(false, "redirectOutput-READ-illegal"); }
        catch (IllegalArgumentException e) { check(true, "redirectOutput-READ-illegal"); }
        try { new ProcessBuilder("x").redirectError(Redirect.from(f)); check(false, "redirectError-READ-illegal"); }
        catch (IllegalArgumentException e) { check(true, "redirectError-READ-illegal"); }
    }

    // ---- C. Runtime metadata (no spawn) ----
    static void sectionRuntime() {
        Runtime rt = Runtime.getRuntime();
        check(rt != null, "runtime-nonnull");
        check(Runtime.getRuntime() == rt, "runtime-singleton");

        check(rt.availableProcessors() >= 1, "runtime-availableProcessors");

        long total = rt.totalMemory();
        long free  = rt.freeMemory();
        long max   = rt.maxMemory();
        check(total > 0, "runtime-totalMemory-pos");
        check(free  >= 0, "runtime-freeMemory-nonneg");
        check(free  <= total, "runtime-free-le-total");
        check(max   > 0, "runtime-maxMemory-pos");
        check(max   >= total, "runtime-max-ge-total");

        rt.gc();
        check(rt.totalMemory() > 0 && rt.freeMemory() <= rt.totalMemory(), "runtime-after-gc");

        // shutdown hook add/remove round-trip
        Thread hook = new Thread(() -> {});
        rt.addShutdownHook(hook);
        check(rt.removeShutdownHook(hook), "runtime-shutdown-hook-roundtrip");
        check(!rt.removeShutdownHook(hook), "runtime-shutdown-hook-removed-once");

        // Runtime.version() -> JDK17 feature
        Runtime.Version v = Runtime.version();
        check(v != null, "runtime-version-nonnull");
        check(v.feature() == 17, "runtime-version-feature-17");
        check(v.compareTo(Runtime.version()) == 0, "runtime-version-compare-self");
    }

    // ---- D. ProcessHandle.current + Info (no spawn) ----
    static void sectionProcessHandle() {
        ProcessHandle cur = ProcessHandle.current();
        check(cur != null, "ph-current-nonnull");
        long pid = cur.pid();
        check(pid > 0, "ph-current-pid-pos");
        check(cur.isAlive(), "ph-current-alive");

        ProcessHandle cur2 = ProcessHandle.current();
        check(cur.equals(cur2), "ph-current-equals");
        check(cur.hashCode() == cur2.hashCode(), "ph-current-hashcode");
        check(cur.compareTo(cur2) == 0, "ph-current-compareTo-self");
        check(cur.pid() == cur2.pid(), "ph-current-pid-stable");

        // ProcessHandle.of(currentPid)
        Optional<ProcessHandle> of = ProcessHandle.of(pid);
        check(of.isPresent(), "ph-of-present");
        check(of.get().pid() == pid, "ph-of-pid");
        check(of.get().equals(cur), "ph-of-equals-current");

        // parent() returns a (possibly empty) Optional, never null
        check(cur.parent() != null, "ph-parent-optional-nonnull");

        // Info contract: all accessors return non-null Optionals (contents are platform dependent)
        ProcessHandle.Info info = cur.info();
        check(info != null, "ph-info-nonnull");
        check(info.command() != null, "ph-info-command-optional");
        check(info.commandLine() != null, "ph-info-commandLine-optional");
        check(info.arguments() != null, "ph-info-arguments-optional");
        check(info.startInstant() != null, "ph-info-startInstant-optional");
        check(info.totalCpuDuration() != null, "ph-info-cpu-optional");
        check(info.user() != null, "ph-info-user-optional");

        // supportsNormalTermination is a boolean predicate that must not throw
        boolean snt = cur.supportsNormalTermination();
        check(snt || !snt, "ph-current-supportsNormalTermination-callable");

        // allProcesses returns a non-null stream (do not enumerate /proc contents)
        check(ProcessHandle.allProcesses() != null, "ph-allProcesses-nonnull");

        // children/descendants return non-null streams
        check(cur.children() != null, "ph-children-stream");
        check(cur.descendants() != null, "ph-descendants-stream");
    }

    // ---- E. Basic spawn: echo, exit codes, streams ----
    static void sectionSpawnBasic() throws Exception {
        Process p = new ProcessBuilder(ECHO, "hello", "world").start();
        check(p.getInputStream() != null, "spawn-getInputStream-nonnull");
        check(p.getOutputStream() != null, "spawn-getOutputStream-nonnull");
        check(p.getErrorStream() != null, "spawn-getErrorStream-nonnull");
        String out = drain(p.getInputStream());
        int code = p.waitFor();
        check(out.equals("hello world"), "spawn-echo-stdout");
        check(code == 0, "spawn-echo-exit0");
        check(p.exitValue() == 0, "spawn-echo-exitValue");
        check(!p.isAlive(), "spawn-echo-not-alive");
        check(p.pid() > 0, "spawn-echo-pid-pos");
        check(p.toHandle().pid() == p.pid(), "spawn-toHandle-pid");
        check(p.info() != null, "spawn-info-nonnull");
        // supportsNormalTermination on a finished process must not throw
        boolean snt = p.supportsNormalTermination();
        check(snt || !snt, "spawn-supportsNormalTermination-callable");

        // exit-code propagation
        check(new ProcessBuilder(TRUE_).start().waitFor() == 0, "spawn-true-exit0");
        check(new ProcessBuilder(FALSE_).start().waitFor() == 1, "spawn-false-exit1");
        check(new ProcessBuilder(SH, "-c", "exit 7").start().waitFor() == 7, "spawn-exit7");
        check(new ProcessBuilder(SH, "-c", "exit 42").start().waitFor() == 42, "spawn-exit42");

        // idempotent waitFor on a finished process
        Process q = new ProcessBuilder(TRUE_).start();
        check(q.waitFor() == 0, "spawn-waitFor-first");
        check(q.waitFor() == 0, "spawn-waitFor-idempotent");

        // waitFor(timeout) true when already finished
        Process r = new ProcessBuilder(TRUE_).start();
        check(r.waitFor(20, TimeUnit.SECONDS), "spawn-waitFor-timeout-true");
        check(r.exitValue() == 0, "spawn-waitFor-timeout-exit0");
    }

    // ---- F. Pipes: stdin -> stdout ----
    static void sectionPipes() throws Exception {
        Process cat = new ProcessBuilder(CAT).start();
        try (OutputStream os = cat.getOutputStream()) { os.write("piped-data\n".getBytes()); }
        String catOut = drain(cat.getInputStream());
        cat.waitFor(20, TimeUnit.SECONDS);
        check(catOut.equals("piped-data"), "pipe-stdin-stdout");

        // larger deterministic payload through cat (still small)
        Process cat2 = new ProcessBuilder(CAT).start();
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 100; i++) sb.append("L").append(i).append("\n");
        byte[] payload = sb.toString().getBytes();
        try (OutputStream os = cat2.getOutputStream()) { os.write(payload); }
        byte[] back = cat2.getInputStream().readAllBytes();
        cat2.waitFor(20, TimeUnit.SECONDS);
        check(Arrays.equals(back, payload), "pipe-roundtrip-100lines");
    }

    // ---- G. Environment passing + working directory ----
    static void sectionEnvAndDir() throws Exception {
        ProcessBuilder pb = new ProcessBuilder(SH, "-c", "echo $DOD_VAR");
        pb.environment().put("DOD_VAR", "starry42");
        check(run(pb).equals("starry42"), "env-var-pass");

        ProcessBuilder pb2 = new ProcessBuilder(SH, "-c", "echo $A-$B");
        pb2.environment().put("A", "foo");
        pb2.environment().put("B", "bar");
        check(run(pb2).equals("foo-bar"), "env-two-vars");

        // removing an inherited/added var
        ProcessBuilder pb3 = new ProcessBuilder(SH, "-c", "echo [$GONE]");
        pb3.environment().put("GONE", "x");
        pb3.environment().remove("GONE");
        check(run(pb3).equals("[]"), "env-var-removed");

        // working directory
        ProcessBuilder pb4 = new ProcessBuilder(SH, "-c", "pwd");
        pb4.directory(new File("/tmp"));
        check(run(pb4).equals("/tmp"), "working-directory");
    }

    // ---- H. Redirect to/from/append files, merge, discard, separate stderr ----
    static void sectionRedirectIO() throws Exception {
        File outF = File.createTempFile("proc-out", ".txt");
        outF.deleteOnExit();
        ProcessBuilder pbw = new ProcessBuilder(ECHO, "file-redirect-data");
        pbw.redirectOutput(outF);
        check(pbw.redirectOutput().type() == Type.WRITE, "fileredir-out-type");
        check(outF.equals(pbw.redirectOutput().file()), "fileredir-out-file");
        Process pw = pbw.start();
        // when output is to a file, the captured input stream is empty (null stream)
        check(pw.getInputStream().readAllBytes().length == 0, "fileredir-out-null-inputstream");
        pw.waitFor(20, TimeUnit.SECONDS);
        check(Files.readString(outF.toPath()).trim().equals("file-redirect-data"), "fileredir-out-content");

        // APPEND: second write keeps the first line
        ProcessBuilder pba = new ProcessBuilder(ECHO, "second-line");
        pba.redirectOutput(Redirect.appendTo(outF));
        check(pba.redirectOutput().type() == Type.APPEND, "fileredir-append-type");
        pba.start().waitFor(20, TimeUnit.SECONDS);
        String appended = Files.readString(outF.toPath());
        check(appended.equals("file-redirect-data\nsecond-line\n"), "fileredir-append-content");

        // WRITE (truncate) overwrites
        ProcessBuilder pbt = new ProcessBuilder(ECHO, "truncated");
        pbt.redirectOutput(outF);
        pbt.start().waitFor(20, TimeUnit.SECONDS);
        check(Files.readString(outF.toPath()).trim().equals("truncated"), "fileredir-truncate");

        // input from file
        File inF = File.createTempFile("proc-in", ".txt");
        inF.deleteOnExit();
        Files.writeString(inF.toPath(), "input-from-file\n");
        ProcessBuilder pbr = new ProcessBuilder(CAT);
        pbr.redirectInput(inF);
        check(pbr.redirectInput().type() == Type.READ, "fileredir-in-type");
        Process pr = pbr.start();
        String inOut = drain(pr.getInputStream());
        pr.waitFor(20, TimeUnit.SECONDS);
        check(inOut.equals("input-from-file"), "fileredir-in-content");

        // redirectErrorStream(true): stderr merged into stdout, sequential order preserved
        ProcessBuilder pm = new ProcessBuilder(SH, "-c", "echo out; echo err 1>&2");
        pm.redirectErrorStream(true);
        Process pmp = pm.start();
        String merged = drain(pmp.getInputStream());
        pmp.waitFor(20, TimeUnit.SECONDS);
        check(merged.equals("out\nerr"), "merge-stderr-into-stdout");

        // separate stderr (no merge)
        ProcessBuilder ps = new ProcessBuilder(SH, "-c", "echo onout; echo onerr 1>&2");
        Process psp = ps.start();
        String so = drain(psp.getInputStream());
        String se = drain(psp.getErrorStream());
        psp.waitFor(20, TimeUnit.SECONDS);
        check(so.equals("onout"), "separate-stdout");
        check(se.equals("onerr"), "separate-stderr");

        // DISCARD output -> captured stream empty, process still exits 0
        ProcessBuilder pd = new ProcessBuilder(ECHO, "discarded");
        pd.redirectOutput(Redirect.DISCARD);
        check(pd.redirectOutput() == Redirect.DISCARD, "discard-getset");
        Process pdp = pd.start();
        check(pdp.getInputStream().readAllBytes().length == 0, "discard-null-stream");
        check(pdp.waitFor() == 0, "discard-exit0");
    }

    // ---- I. Lifecycle: destroy, destroyForcibly, isAlive, waitFor(timeout)=false, onExit, exitValue exception ----
    static void sectionLifecycle() throws Exception {
        // exitValue() on a running process throws IllegalThreadStateException
        Process s0 = new ProcessBuilder(SLEEP, "30").start();
        check(s0.isAlive(), "lifecycle-sleep-alive");
        try { s0.exitValue(); check(false, "lifecycle-exitValue-running-throws"); }
        catch (IllegalThreadStateException e) { check(true, "lifecycle-exitValue-running-throws"); }
        // waitFor(timeout) returns false while still running
        check(!s0.waitFor(50, TimeUnit.MILLISECONDS), "lifecycle-waitFor-timeout-false");
        check(s0.isAlive(), "lifecycle-still-alive-after-short-wait");
        s0.destroyForcibly();
        s0.waitFor(20, TimeUnit.SECONDS);
        check(!s0.isAlive(), "lifecycle-forcibly-not-alive");

        // destroy() (SIGTERM) -> terminates, nonzero exit
        Process s1 = new ProcessBuilder(SLEEP, "30").start();
        check(s1.isAlive(), "destroy-alive-before");
        s1.destroy(); // void
        s1.waitFor(20, TimeUnit.SECONDS);
        check(!s1.isAlive(), "destroy-not-alive");
        check(s1.exitValue() != 0, "destroy-nonzero-exit");

        // destroyForcibly() returns the same Process and kills it
        Process s2 = new ProcessBuilder(SLEEP, "30").start();
        Process s2ret = s2.destroyForcibly();
        check(s2ret == s2, "destroyForcibly-returns-this");
        s2.waitFor(20, TimeUnit.SECONDS);
        check(!s2.isAlive(), "destroyForcibly-not-alive");
        check(s2.exitValue() != 0, "destroyForcibly-nonzero-exit");

        // onExit() CompletableFuture completes with the same Process
        Process e0 = new ProcessBuilder(TRUE_).start();
        CompletableFuture<Process> f = e0.onExit();
        check(f != null, "onExit-future-nonnull");
        Process done = f.get(20, TimeUnit.SECONDS);
        check(done == e0, "onExit-completes-same-process");
        check(!e0.isAlive(), "onExit-not-alive");
        check(e0.exitValue() == 0, "onExit-exit0");

        // ProcessHandle.onExit() on a spawned child
        Process e1 = new ProcessBuilder(TRUE_).start();
        ProcessHandle h1 = e1.toHandle();
        CompletableFuture<ProcessHandle> hf = h1.onExit();
        ProcessHandle hdone = hf.get(20, TimeUnit.SECONDS);
        check(hdone.pid() == e1.pid(), "handle-onExit-pid");
        e1.waitFor(20, TimeUnit.SECONDS);
    }

    // ---- J. Runtime.exec variants ----
    static void sectionRuntimeExec() throws Exception {
        Runtime rt = Runtime.getRuntime();

        // exec(String[])
        Process p1 = rt.exec(new String[]{ECHO, "rt-array"});
        String o1 = drain(p1.getInputStream());
        p1.waitFor(20, TimeUnit.SECONDS);
        check(o1.equals("rt-array"), "rt-exec-array");

        // exec(String) tokenizes on whitespace
        Process p2 = rt.exec(ECHO + " rt-string");
        String o2 = drain(p2.getInputStream());
        p2.waitFor(20, TimeUnit.SECONDS);
        check(o2.equals("rt-string"), "rt-exec-string");

        // exec(String[], envp) replaces environment
        Process p3 = rt.exec(new String[]{SH, "-c", "echo $RTENV"}, new String[]{"RTENV=rtval"});
        String o3 = drain(p3.getInputStream());
        p3.waitFor(20, TimeUnit.SECONDS);
        check(o3.equals("rtval"), "rt-exec-array-envp");

        // exec(String[], envp, dir)
        Process p4 = rt.exec(new String[]{SH, "-c", "pwd"}, null, new File("/tmp"));
        String o4 = drain(p4.getInputStream());
        p4.waitFor(20, TimeUnit.SECONDS);
        check(o4.equals("/tmp"), "rt-exec-array-envp-dir");
    }

    // ---- K. Error / exception paths ----
    static void sectionErrors() throws Exception {
        // empty command list -> IndexOutOfBoundsException
        try { new ProcessBuilder(new ArrayList<String>()).start(); check(false, "err-empty-command"); }
        catch (IndexOutOfBoundsException e) { check(true, "err-empty-command"); }

        // null command element -> NullPointerException
        try { new ProcessBuilder(Arrays.asList(ECHO, (String) null)).start(); check(false, "err-null-element"); }
        catch (NullPointerException e) { check(true, "err-null-element"); }

        // non-existent executable -> IOException
        try { new ProcessBuilder("/no/such/binary-xyz-123").start(); check(false, "err-nonexistent-binary"); }
        catch (IOException e) { check(true, "err-nonexistent-binary"); }

        // Runtime.exec empty string -> IllegalArgumentException
        try { Runtime.getRuntime().exec(""); check(false, "err-exec-empty-string"); }
        catch (IllegalArgumentException e) { check(true, "err-exec-empty-string"); }

        // ProcessHandle.of(negative pid) is not present (and must not throw)
        // Use a large pid that is essentially never live; tolerate either empty or (improbably) present-but-dead.
        Optional<ProcessHandle> bogus = ProcessHandle.of(0x3FFFFFFFL);
        check(bogus != null, "err-handle-of-large-pid-nonnull-optional");
    }

    // ---- L. startPipeline (Java 9+) ----
    static void sectionPipeline() throws Exception {
        // Second stage uses only /bin/sh (guaranteed present on a minimal busybox rootfs)
        // as a read-loop filter, so the pipeline does not depend on coreutils such as sort.
        List<ProcessBuilder> builders = Arrays.asList(
            new ProcessBuilder(SH, "-c", "echo charlie; echo bravo; echo alpha"),
            new ProcessBuilder(SH, "-c", "while IFS= read -r l; do echo \"got:$l\"; done"));
        List<Process> procs = ProcessBuilder.startPipeline(builders);
        check(procs.size() == 2, "pipeline-process-count");
        Process last = procs.get(procs.size() - 1);
        String piped = drain(last.getInputStream());
        for (Process pr : procs) pr.waitFor(20, TimeUnit.SECONDS);
        check(piped.equals("got:charlie\ngot:bravo\ngot:alpha"), "pipeline-filtered-output");
        check(procs.get(0).exitValue() == 0 && last.exitValue() == 0, "pipeline-all-exit0");
    }

    // ---- M. Sequential spawn stress (fork/exec/wait) ----
    static void sectionSequentialSpawns() throws Exception {
        int sum = 0;
        for (int i = 1; i <= 10; i++) {
            String r = run(new ProcessBuilder(SH, "-c", "echo " + i));
            sum += Integer.parseInt(r);
        }
        check(sum == 55, "sequential-spawns-sum");
    }

    public static void main(String[] args) throws Exception {
        sec("builder-config",   ProcessTest::sectionBuilderConfig);
        sec("redirect",         ProcessTest::sectionRedirect);
        sec("runtime",          ProcessTest::sectionRuntime);
        sec("process-handle",   ProcessTest::sectionProcessHandle);
        sec("spawn-basic",      ProcessTest::sectionSpawnBasic);
        sec("pipes",            ProcessTest::sectionPipes);
        sec("env-and-dir",      ProcessTest::sectionEnvAndDir);
        sec("redirect-io",      ProcessTest::sectionRedirectIO);
        sec("lifecycle",        ProcessTest::sectionLifecycle);
        sec("runtime-exec",     ProcessTest::sectionRuntimeExec);
        sec("errors",           ProcessTest::sectionErrors);
        sec("pipeline",         ProcessTest::sectionPipeline);
        sec("sequential-spawns",ProcessTest::sectionSequentialSpawns);

        System.out.println("PROCESS_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("PROCESS_DONE");
    }
}
