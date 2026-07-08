# Apache Debug Probes

Each debug script targets one Apache-observed issue. Keep probes small and run
the same probe in Linux Alpine before comparing with StarryOS.

Probe names:

- `mpm-prefork-wait`
- `phase20-restart`
- `mpm-thread-futex` (not implemented yet)
- `accept-mutex`
- `htaccess-pathwalk`
- `sendfile-mmap-range`
- `graceful-signal`
- `cgi-pipe-exec`
- `log-append-reopen`

Issue notes:

- `ISSUE-001-phase20-prefork-readiness.md` records the phase20 readiness
  overspecification finding and points to `apache-phase20-restart.sh`.
