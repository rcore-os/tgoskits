// Link the production runtime's synchronization capability implementations
// into each integration-test binary. `ax-sync` deliberately owns no host
// fallback provider.
use ax_runtime as _;
