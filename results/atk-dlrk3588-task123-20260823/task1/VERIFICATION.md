# Verification record

## Automated tests

Executed from `/home/huhu/tgoskits-starry` on 2026-08-23:

```text
python3 -m unittest -v scripts.test.net-dual-guest.test_verify_starry_task1_ab
Ran 6 tests ... OK

python3 -m unittest discover -v -s scripts/test/net-dual-guest -p 'test_*.py'
Ran 28 tests ... OK
```

Full outputs are `task1-verifier-tests.log` and
`net-dual-guest-tests.log`. The two expected `error: missing gate/stalled`
strings are fixture output from tests that verify failure-forensics behavior;
the corresponding test cases themselves are `ok`.

## Physical-log invariants

The uniform parser rejects incomplete blocks, missing rows, duplicate or
non-contiguous sequence numbers, and declared sample-count mismatches. It
accepted all six YOLO runs and the idle, stress, and stability RT-Thread runs.
The cyclictest parser accepted 20,000, 20,000, and 600,000 Linux samples with
zero overflow.

Formal console logs were scanned for `panic`, `ESR_EL2`, IRQ 26 fatal, and
generic fatal markers; none were present. Diagnostic pre-runs are intentionally
not mixed into the formal directories.

## Integrity

The full external archive's `MANIFEST.sha256` covers every file including the
exact raw/FIT images. The repository snapshot omits generated images, records
their identities in the top-level `artifact-hashes.txt`, and covers all
committed evidence with `SHA256SUMS.txt`. Validate from the snapshot root with:

```text
sha256sum -c SHA256SUMS.txt
```
