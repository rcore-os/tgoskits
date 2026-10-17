#!/bin/sh
echo "SYSFS_INFO_BEGIN"
/usr/bin/sysfs-carpet
RC=$?
echo "SYSFS_INFO_DONE RC=$RC"
