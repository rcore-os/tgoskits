# ICPC — Industrial Control Plane Communication

`no_std` application-layer protocol for cross-guest control traffic (Task 2).

Runs over UDP. Wire header is 24 bytes plus variable payload; CRC32 covers the
header (with the CRC field zeroed) and the payload.
