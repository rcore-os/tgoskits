import org.junit.runner.JUnitCore;
import org.junit.runner.Result;
import org.junit.runner.notification.Failure;

/**
 * Comprehensive Java-8 backward-compat suite runner.
 *
 * Compiled with --release 8 (bytecode major version 52) so that the produced
 * jar runs forward-compatibly on newer JREs (17, 21). This main() drives all
 * per-library *BackCompatTest classes through JUnitCore and reports per-class
 * and total results.
 *
 * On zero failures it prints exactly:  BACKCOMPAT_REAL_OK <total>
 * On any failure it prints:            BACKCOMPAT_REAL_FAIL  + details
 */
public class BackCompatReal {

    private static final Class<?>[] SUITE = new Class<?>[] {
        CommonsBackCompatTest.class,
        LoggingBackCompatTest.class,
        SqlBackCompatTest.class,
        JsonBackCompatTest.class,
        ScriptBackCompatTest.class
    };

    public static void main(String[] args) {
        int totalRun = 0;
        int totalFailures = 0;
        int totalIgnored = 0;
        boolean anyFailure = false;
        StringBuilder failureDetails = new StringBuilder();

        for (Class<?> testClass : SUITE) {
            Result result = JUnitCore.runClasses(testClass);
            int run = result.getRunCount();
            int fail = result.getFailureCount();
            int ignored = result.getIgnoreCount();
            totalRun += run;
            totalFailures += fail;
            totalIgnored += ignored;

            System.out.println(testClass.getSimpleName()
                    + " -> Tests run: " + run
                    + ", Failures: " + fail
                    + ", Ignored: " + ignored);

            if (fail > 0) {
                anyFailure = true;
                for (Failure f : result.getFailures()) {
                    failureDetails.append("  [")
                            .append(testClass.getSimpleName())
                            .append("] ")
                            .append(f.getTestHeader())
                            .append(" : ")
                            .append(f.getMessage())
                            .append(System.lineSeparator());
                    Throwable ex = f.getException();
                    if (ex != null) {
                        failureDetails.append("      ")
                                .append(ex.getClass().getName())
                                .append(System.lineSeparator());
                    }
                }
            }
        }

        System.out.println("------------------------------------------------------------");
        System.out.println("TOTAL -> Tests run: " + totalRun
                + ", Failures: " + totalFailures
                + ", Ignored: " + totalIgnored);

        if (!anyFailure) {
            System.out.println("BACKCOMPAT_REAL_OK " + totalRun);
        } else {
            System.out.println("BACKCOMPAT_REAL_FAIL");
            System.out.print(failureDetails.toString());
            System.exit(1);
        }
    }
}
