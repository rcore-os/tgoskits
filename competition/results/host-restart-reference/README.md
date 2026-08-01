# Host controller-restart reference

This retained IPv4/UDP loopback run verifies application-session recovery, not
AxVisor or cross-guest performance. One endpoint accepted sequence 1 from
session 101, then accepted sequence 1 from a restarted controller using session
202. Both commands completed once, the endpoint recorded one session reset,
and it reported no rejected session, duplicate, protocol error, or timeout.

Reproduce it with:

```sh
bash competition/ivc/run-restart-recovery.sh \
  tmp/competition/ivc/restart-recovery-reproduction
```

See [`metadata.json`](metadata.json) for hashes and exact invariants.
