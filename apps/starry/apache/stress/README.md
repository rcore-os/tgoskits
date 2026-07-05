# Apache Stress Tests

Stress tests are separate from smoke and phase correctness tests. Do not use
stress results to mark phase tracker items as passed.

`apache-runner.sh stress` exits successfully without running stress cases, and
the `all` flow does not include stress.
