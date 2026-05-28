# Nginx Debug Tests

This directory stores flexible debug scripts for single issue reproduction and diagnosis.

Current scripts:

- `nginx-http-basic-tests.sh`: early HTTP basic script kept for issue-level debugging.

Rule:

- Debug scripts are free-form and can focus on one syscall or one behavior path.
- Debug scripts are not connected to tgoskits nginx CI entry.
