# Nginx Stress Tests

This directory is reserved for stress and pressure tests.

Current status:

- No stress script is integrated yet.
- Planned items moved out from phase tracking:
  - concurrent 2, 100 requests
  - concurrent 8, 1000 requests
  - concurrent 32, 5000 requests
  - keep-alive concurrent connections
  - large file concurrent download
  - mixed 200/404/range/large traffic

Management rule:

- Stress tests are managed separately from phase tests.
- Stress tests are not connected to tgoskits CI test entry for nginx.
