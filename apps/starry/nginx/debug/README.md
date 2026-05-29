# Nginx Debug Tests

This directory stores flexible debug scripts for single issue reproduction and diagnosis.

Current scripts:

- `nginx-http-basic-tests.sh`: early HTTP basic script kept for issue-level debugging.
- `nginx-2-0-bad-method-debug.sh`: focused probe for stage 2.0 BAD method (`BAD / HTTP/1.1`) instability.

Rule:

- Debug scripts are free-form and can focus on one syscall or one behavior path.
- Debug scripts are not connected to tgoskits nginx CI entry.
