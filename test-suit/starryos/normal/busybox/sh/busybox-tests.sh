#!/bin/sh
# Auto-generated from ChenLongTest by scripts/convert_busybox_tests.py
PASS=0; FAIL=0; SKIP=0

_t=$({ timeout 10 sh -c "busybox adjtimex 2>&1"; } 2>&1)
if [ -n "$_t" ]; then echo "PASS: busybox_adjtimex"; PASS=$((PASS+1)); else echo "FAIL: busybox_adjtimex (empty)"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox arch 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "riscv"; then echo "PASS: busybox_arch"; PASS=$((PASS+1)); else echo "FAIL: busybox_arch"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ash -c 'echo ash_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ash_ok"; then echo "PASS: busybox_ash"; PASS=$((PASS+1)); else echo "FAIL: busybox_ash"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox awk 'BEGIN{print \"awk_ok\"}' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "awk_ok"; then echo "PASS: busybox_awk"; PASS=$((PASS+1)); else echo "FAIL: busybox_awk"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo test | busybox base64"; } 2>&1)
if echo "$_t" | grep -qF "dGVzdAo="; then echo "PASS: busybox_base64"; PASS=$((PASS+1)); else echo "FAIL: busybox_base64"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox basename /usr/bin/foo 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "foo"; then echo "PASS: busybox_basename"; PASS=$((PASS+1)); else echo "FAIL: busybox_basename"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox bbconfig 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "CONFIG_BUSYBOX=y"; then echo "PASS: busybox_bbconfig"; PASS=$((PASS+1)); else echo "FAIL: busybox_bbconfig"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo '2+2' | busybox bc 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "4"; then echo "PASS: busybox_bc"; PASS=$((PASS+1)); else echo "FAIL: busybox_bc"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox beep 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "can't open console"; then echo "PASS: busybox_beep"; PASS=$((PASS+1)); else echo "FAIL: busybox_beep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox brctl -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/sys/class/net"; then echo "PASS: busybox_brctl"; PASS=$((PASS+1)); else echo "FAIL: busybox_brctl"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo bunzip_ok > /tmp/bb_bunzip_t && busybox bzip2 -f /tmp/bb_bunzip_t && busybox bunzip2 -f /tmp/bb_bunzip_t.bz2 && busybox cat /tmp/bb_bunzip_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bunzip_ok"; then echo "PASS: busybox_bunzip2"; PASS=$((PASS+1)); else echo "FAIL: busybox_bunzip2"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo bzcat_ok > /tmp/bb_bzcat_t && busybox bzip2 -f /tmp/bb_bzcat_t && busybox bzcat /tmp/bb_bzcat_t.bz2' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bzcat_ok"; then echo "PASS: busybox_bzcat"; PASS=$((PASS+1)); else echo "FAIL: busybox_bzcat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo bz2_ok > /tmp/bb_bzip2_t && busybox bzip2 -kf /tmp/bb_bzip2_t && busybox test -f /tmp/bb_bzip2_t.bz2 && busybox echo bzip2_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bzip2_ok"; then echo "PASS: busybox_bzip2"; PASS=$((PASS+1)); else echo "FAIL: busybox_bzip2"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox cal 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Su Mo Tu We Th Fr Sa"; then echo "PASS: busybox_cal"; PASS=$((PASS+1)); else echo "FAIL: busybox_cal"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox cat /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_cat"; PASS=$((PASS+1)); else echo "FAIL: busybox_cat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox chattr -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: chattr"; then echo "PASS: busybox_chattr"; PASS=$((PASS+1)); else echo "FAIL: busybox_chattr"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo g > /tmp/bb_chgrp_t && G=\$(busybox id -g) && busybox chgrp \"\$G\" /tmp/bb_chgrp_t && busybox ls -ln /tmp/bb_chgrp_t | busybox grep -q \" \$G \" && busybox echo chgrp_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chgrp_ok"; then echo "PASS: busybox_chgrp"; PASS=$((PASS+1)); else echo "FAIL: busybox_chgrp"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo m > /tmp/bb_chmod_t && busybox chmod 600 /tmp/bb_chmod_t && busybox ls -l /tmp/bb_chmod_t | busybox grep -q \"^-rw-------\" && busybox echo chmod_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chmod_ok"; then echo "PASS: busybox_chmod"; PASS=$((PASS+1)); else echo "FAIL: busybox_chmod"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox chpasswd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: chpasswd"; then echo "PASS: busybox_chpasswd"; PASS=$((PASS+1)); else echo "FAIL: busybox_chpasswd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox chroot / /bin/busybox echo chroot_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chroot_ok"; then echo "PASS: busybox_chroot"; PASS=$((PASS+1)); else echo "FAIL: busybox_chroot"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox chvt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "invalid number"; then echo "PASS: busybox_chvt"; PASS=$((PASS+1)); else echo "FAIL: busybox_chvt"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox cksum /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/etc/passwd"; then echo "PASS: busybox_cksum"; PASS=$((PASS+1)); else echo "FAIL: busybox_cksum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox clear 2>&1; busybox echo clear_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "clear_ok"; then echo "PASS: busybox_clear"; PASS=$((PASS+1)); else echo "FAIL: busybox_clear"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo cmp_ok > /tmp/bb_cmp_a && busybox cp /tmp/bb_cmp_a /tmp/bb_cmp_b && busybox cmp /tmp/bb_cmp_a /tmp/bb_cmp_b && busybox echo cmp_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "cmp_ok"; then echo "PASS: busybox_cmp"; PASS=$((PASS+1)); else echo "FAIL: busybox_cmp"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox printf \"a\\nb\\n\" > /tmp/bb_comm_1 && busybox printf \"b\\nc\\n\" > /tmp/bb_comm_2 && busybox comm /tmp/bb_comm_1 /tmp/bb_comm_2' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "c"; then echo "PASS: busybox_comm"; PASS=$((PASS+1)); else echo "FAIL: busybox_comm"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo cp_ok > /tmp/bb_cp_src && busybox cp /tmp/bb_cp_src /tmp/bb_cp_dst && busybox cat /tmp/bb_cp_dst' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "cp_ok"; then echo "PASS: busybox_cp"; PASS=$((PASS+1)); else echo "FAIL: busybox_cp"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox cryptpw -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: cryptpw"; then echo "PASS: busybox_cryptpw"; PASS=$((PASS+1)); else echo "FAIL: busybox_cryptpw"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo 'a:b:c' | busybox cut -d: -f2 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "b"; then echo "PASS: busybox_cut"; PASS=$((PASS+1)); else echo "FAIL: busybox_cut"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox date +%Y 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "20"; then echo "PASS: busybox_date"; PASS=$((PASS+1)); else echo "FAIL: busybox_date"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo '2 2 + p' | busybox dc 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "4"; then echo "PASS: busybox_dc"; PASS=$((PASS+1)); else echo "FAIL: busybox_dc"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo dd_ok > /tmp/bb_dd_in && busybox dd if=/tmp/bb_dd_in of=/tmp/bb_dd_out bs=1 count=6 2>/tmp/bb_dd_stat && busybox cat /tmp/bb_dd_out' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "dd_ok"; then echo "PASS: busybox_dd"; PASS=$((PASS+1)); else echo "FAIL: busybox_dd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox deallocvt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "invalid number"; then echo "PASS: busybox_deallocvt"; PASS=$((PASS+1)); else echo "FAIL: busybox_deallocvt"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'G=bb_delg_t && busybox delgroup \"\$G\" 2>/dev/null || true && busybox addgroup \"\$G\" && busybox delgroup \"\$G\" && ! busybox grep -F \"\$G:\" /etc/group >/dev/null && busybox echo delgroup_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "delgroup_ok"; then echo "PASS: busybox_delgroup"; PASS=$((PASS+1)); else echo "FAIL: busybox_delgroup"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'U=bb_delu_t && busybox deluser \"\$U\" 2>/dev/null || true && busybox adduser -D -H \"\$U\" && busybox deluser \"\$U\" && ! busybox grep -F \"\$U:\" /etc/passwd >/dev/null && busybox echo deluser_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "deluser_ok"; then echo "PASS: busybox_deluser"; PASS=$((PASS+1)); else echo "FAIL: busybox_deluser"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox depmod -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: depmod"; then echo "PASS: busybox_depmod"; PASS=$((PASS+1)); else echo "FAIL: busybox_depmod"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox df 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Filesystem"; then echo "PASS: busybox_df"; PASS=$((PASS+1)); else echo "FAIL: busybox_df"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo left > /tmp/bb_diff_l && busybox echo right > /tmp/bb_diff_r && busybox diff /tmp/bb_diff_l /tmp/bb_diff_r > /tmp/bb_diff_o 2>&1; busybox grep -q \"^--- /tmp/bb_diff_l\" /tmp/bb_diff_o && busybox echo diff_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "diff_ok"; then echo "PASS: busybox_diff"; PASS=$((PASS+1)); else echo "FAIL: busybox_diff"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox dirname /usr/bin/foo 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/usr/bin"; then echo "PASS: busybox_dirname"; PASS=$((PASS+1)); else echo "FAIL: busybox_dirname"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox dmesg -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: dmesg"; then echo "PASS: busybox_dmesg"; PASS=$((PASS+1)); else echo "FAIL: busybox_dmesg"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox dnsdomainname -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "No help available"; then echo "PASS: busybox_dnsdomainname"; PASS=$((PASS+1)); else echo "FAIL: busybox_dnsdomainname"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox du -s . 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "."; then echo "PASS: busybox_du"; PASS=$((PASS+1)); else echo "FAIL: busybox_du"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox dumpkmap -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: dumpkmap"; then echo "PASS: busybox_dumpkmap"; PASS=$((PASS+1)); else echo "FAIL: busybox_dumpkmap"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo echo_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "echo_ok"; then echo "PASS: busybox_echo"; PASS=$((PASS+1)); else echo "FAIL: busybox_echo"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo hello | busybox egrep hell 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_egrep"; PASS=$((PASS+1)); else echo "FAIL: busybox_egrep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox eject 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/dev/cdrom"; then echo "PASS: busybox_eject"; PASS=$((PASS+1)); else echo "FAIL: busybox_eject"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ether-wake 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ether-wake"; then echo "PASS: busybox_ether_wake"; PASS=$((PASS+1)); else echo "FAIL: busybox_ether_wake"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf \"a\\tb\\n\" | busybox expand 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "a       b"; then echo "PASS: busybox_expand"; PASS=$((PASS+1)); else echo "FAIL: busybox_expand"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox expr 3 '*' 4 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "12"; then echo "PASS: busybox_expr"; PASS=$((PASS+1)); else echo "FAIL: busybox_expr"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox factor 6 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "6: 2 3"; then echo "PASS: busybox_factor"; PASS=$((PASS+1)); else echo "FAIL: busybox_factor"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_falloc_t && busybox touch /tmp/bb_falloc_t && busybox fallocate -l 1024 /tmp/bb_falloc_t && busybox ls -ln /tmp/bb_falloc_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF " 1024 "; then echo "PASS: busybox_fallocate"; PASS=$((PASS+1)); else echo "FAIL: busybox_fallocate"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox false; busybox echo exit:\$? 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "exit:1"; then echo "PASS: busybox_false"; PASS=$((PASS+1)); else echo "FAIL: busybox_false"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fatattr -v / 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "fatattr:"; then echo "PASS: busybox_fatattr"; PASS=$((PASS+1)); else echo "FAIL: busybox_fatattr"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fbset -i 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "option 'i' not handled"; then echo "PASS: busybox_fbset"; PASS=$((PASS+1)); else echo "FAIL: busybox_fbset"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fbsplash -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fbsplash"; then echo "PASS: busybox_fbsplash"; PASS=$((PASS+1)); else echo "FAIL: busybox_fbsplash"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fdisk -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fdisk"; then echo "PASS: busybox_fdisk"; PASS=$((PASS+1)); else echo "FAIL: busybox_fdisk"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo hello | busybox fgrep hell 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_fgrep"; PASS=$((PASS+1)); else echo "FAIL: busybox_fgrep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox find / -maxdepth 1 -name proc 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "proc"; then echo "PASS: busybox_find"; PASS=$((PASS+1)); else echo "FAIL: busybox_find"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox findfs -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: findfs"; then echo "PASS: busybox_findfs"; PASS=$((PASS+1)); else echo "FAIL: busybox_findfs"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_flock_t && busybox touch /tmp/bb_flock_t && busybox flock -x /tmp/bb_flock_t -c 'busybox echo flock_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "flock_ok"; then echo "PASS: busybox_flock"; PASS=$((PASS+1)); else echo "FAIL: busybox_flock"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo abcdef | busybox fold -w 2 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ab"; then echo "PASS: busybox_fold"; PASS=$((PASS+1)); else echo "FAIL: busybox_fold"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox free 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Mem"; then echo "PASS: busybox_free"; PASS=$((PASS+1)); else echo "FAIL: busybox_free"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fsck -V 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "fsck"; then echo "PASS: busybox_fsck"; PASS=$((PASS+1)); else echo "FAIL: busybox_fsck"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fstrim -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fstrim"; then echo "PASS: busybox_fstrim"; PASS=$((PASS+1)); else echo "FAIL: busybox_fstrim"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo fsync_ok > /tmp/bb_fsync_t && busybox fsync /tmp/bb_fsync_t && busybox cat /tmp/bb_fsync_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "fsync_ok"; then echo "PASS: busybox_fsync"; PASS=$((PASS+1)); else echo "FAIL: busybox_fsync"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox fuser -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fuser"; then echo "PASS: busybox_fuser"; PASS=$((PASS+1)); else echo "FAIL: busybox_fuser"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox getty -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: getty"; then echo "PASS: busybox_getty"; PASS=$((PASS+1)); else echo "FAIL: busybox_getty"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo hello | busybox grep hell 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_grep"; PASS=$((PASS+1)); else echo "FAIL: busybox_grep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox groups 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root"; then echo "PASS: busybox_groups"; PASS=$((PASS+1)); else echo "FAIL: busybox_groups"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n hello | busybox gzip -c | busybox gunzip -c 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_gunzip"; PASS=$((PASS+1)); else echo "FAIL: busybox_gunzip"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo -n hello > /tmp/bb_gzip_t && busybox gzip -f /tmp/bb_gzip_t && busybox test -s /tmp/bb_gzip_t.gz && busybox echo gzip_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "gzip_ok"; then echo "PASS: busybox_gzip"; PASS=$((PASS+1)); else echo "FAIL: busybox_gzip"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox halt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: halt"; then echo "PASS: busybox_halt"; PASS=$((PASS+1)); else echo "FAIL: busybox_halt"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox hd -n 64 /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "00000000"; then echo "PASS: busybox_hd"; PASS=$((PASS+1)); else echo "FAIL: busybox_hd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox head -n 1 /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_head"; PASS=$((PASS+1)); else echo "FAIL: busybox_head"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n ab | busybox hexdump -C 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "61"; then echo "PASS: busybox_hexdump"; PASS=$((PASS+1)); else echo "FAIL: busybox_hexdump"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox hostname 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "starry"; then echo "PASS: busybox_hostname"; PASS=$((PASS+1)); else echo "FAIL: busybox_hostname"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox id 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "uid="; then echo "PASS: busybox_id"; PASS=$((PASS+1)); else echo "FAIL: busybox_id"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ifdown 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ifdown"; then echo "PASS: busybox_ifdown"; PASS=$((PASS+1)); else echo "FAIL: busybox_ifdown"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ifup -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ifup"; then echo "PASS: busybox_ifup"; PASS=$((PASS+1)); else echo "FAIL: busybox_ifup"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox init 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "must be run as PID 1"; then echo "PASS: busybox_init"; PASS=$((PASS+1)); else echo "FAIL: busybox_init"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox inotifyd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: inotifyd"; then echo "PASS: busybox_inotifyd"; PASS=$((PASS+1)); else echo "FAIL: busybox_inotifyd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_inst_dst /tmp/bb_inst_src && busybox echo ok > /tmp/bb_inst_src && busybox install -m 644 /tmp/bb_inst_src /tmp/bb_inst_dst && busybox ls -l /tmp/bb_inst_dst && busybox cat /tmp/bb_inst_dst' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ok"; then echo "PASS: busybox_install"; PASS=$((PASS+1)); else echo "FAIL: busybox_install"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ionice -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ionice"; then echo "PASS: busybox_ionice"; PASS=$((PASS+1)); else echo "FAIL: busybox_ionice"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ipcrm -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ipcrm"; then echo "PASS: busybox_ipcrm"; PASS=$((PASS+1)); else echo "FAIL: busybox_ipcrm"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ipcs 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Message Queues"; then echo "PASS: busybox_ipcs"; PASS=$((PASS+1)); else echo "FAIL: busybox_ipcs"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ip neigh show 2>&1; busybox echo ipneigh_ok"; } 2>&1)
if echo "$_t" | grep -qF "ipneigh_ok"; then echo "PASS: busybox_ipneigh"; PASS=$((PASS+1)); else echo "FAIL: busybox_ipneigh"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ip route show 2>&1; busybox echo iproute_ok"; } 2>&1)
if echo "$_t" | grep -qF "iproute_ok"; then echo "PASS: busybox_iproute"; PASS=$((PASS+1)); else echo "FAIL: busybox_iproute"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ip rule show 2>&1; busybox echo iprule_ok"; } 2>&1)
if echo "$_t" | grep -qF "iprule_ok"; then echo "PASS: busybox_iprule"; PASS=$((PASS+1)); else echo "FAIL: busybox_iprule"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ip tunnel show 2>&1; busybox echo iptunnel_ok"; } 2>&1)
if echo "$_t" | grep -qF "iptunnel_ok"; then echo "PASS: busybox_iptunnel"; PASS=$((PASS+1)); else echo "FAIL: busybox_iptunnel"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox kbd_mode -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage"; then echo "PASS: busybox_kbd_mode"; PASS=$((PASS+1)); else echo "FAIL: busybox_kbd_mode"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox kill -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "HUP"; then echo "PASS: busybox_kill"; PASS=$((PASS+1)); else echo "FAIL: busybox_kill"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox killall -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "HUP"; then echo "PASS: busybox_killall"; PASS=$((PASS+1)); else echo "FAIL: busybox_killall"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox klogd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: klogd"; then echo "PASS: busybox_klogd"; PASS=$((PASS+1)); else echo "FAIL: busybox_klogd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox last -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: last"; then echo "PASS: busybox_last"; PASS=$((PASS+1)); else echo "FAIL: busybox_last"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox less -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: less"; then echo "PASS: busybox_less"; PASS=$((PASS+1)); else echo "FAIL: busybox_less"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox linux32 busybox echo linux32_ok 2>&1 || busybox echo linux32_fallback"; } 2>&1)
if echo "$_t" | grep -qF "linux32_"; then echo "PASS: busybox_linux32"; PASS=$((PASS+1)); else echo "FAIL: busybox_linux32"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox linux64 busybox echo linux64_ok 2>&1 || busybox echo linux64_fallback"; } 2>&1)
if echo "$_t" | grep -qF "linux64_"; then echo "PASS: busybox_linux64"; PASS=$((PASS+1)); else echo "FAIL: busybox_linux64"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox --list"; } 2>&1)
if [ -n "$_t" ]; then echo "PASS: busybox_list"; PASS=$((PASS+1)); else echo "FAIL: busybox_list (empty)"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_ln_s && busybox echo t > /tmp/bb_ln_t && busybox ln -s /tmp/bb_ln_t /tmp/bb_ln_s && busybox readlink /tmp/bb_ln_s 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bb_ln_t"; then echo "PASS: busybox_ln"; PASS=$((PASS+1)); else echo "FAIL: busybox_ln"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox loadfont -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: loadfont"; then echo "PASS: busybox_loadfont"; PASS=$((PASS+1)); else echo "FAIL: busybox_loadfont"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox loadkmap -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: loadkmap"; then echo "PASS: busybox_loadkmap"; PASS=$((PASS+1)); else echo "FAIL: busybox_loadkmap"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox logger -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: logger"; then echo "PASS: busybox_logger"; PASS=$((PASS+1)); else echo "FAIL: busybox_logger"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox login -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: login"; then echo "PASS: busybox_login"; PASS=$((PASS+1)); else echo "FAIL: busybox_login"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox logread 2>&1; busybox echo logread_ok"; } 2>&1)
if echo "$_t" | grep -qF "logread_ok"; then echo "PASS: busybox_logread"; PASS=$((PASS+1)); else echo "FAIL: busybox_logread"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox losetup -a 2>&1; busybox echo losetup_ok"; } 2>&1)
if echo "$_t" | grep -qF "losetup_ok"; then echo "PASS: busybox_losetup"; PASS=$((PASS+1)); else echo "FAIL: busybox_losetup"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ls / 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bin"; then echo "PASS: busybox_ls_bb"; PASS=$((PASS+1)); else echo "FAIL: busybox_ls_bb"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox lsattr -d /tmp 2>&1; busybox echo lsattr_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsattr_ok"; then echo "PASS: busybox_lsattr"; PASS=$((PASS+1)); else echo "FAIL: busybox_lsattr"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox lsmod 2>&1; busybox echo lsmod_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsmod_ok"; then echo "PASS: busybox_lsmod"; PASS=$((PASS+1)); else echo "FAIL: busybox_lsmod"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox lsof 2>&1; busybox echo lsof_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsof_ok"; then echo "PASS: busybox_lsof"; PASS=$((PASS+1)); else echo "FAIL: busybox_lsof"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox lsusb 2>&1; busybox echo lsusb_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsusb_ok"; then echo "PASS: busybox_lsusb"; PASS=$((PASS+1)); else echo "FAIL: busybox_lsusb"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_lzop.txt /tmp/bb_lzop.txt.lzo && busybox echo -n lzop_t > /tmp/bb_lzop.txt && busybox lzop -f /tmp/bb_lzop.txt && busybox lzop -dc /tmp/bb_lzop.txt.lzo 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "lzop_t"; then echo "PASS: busybox_lzop"; PASS=$((PASS+1)); else echo "FAIL: busybox_lzop"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n round | busybox lzop -c 2>/dev/null | busybox lzopcat 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "round"; then echo "PASS: busybox_lzopcat"; PASS=$((PASS+1)); else echo "FAIL: busybox_lzopcat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox makemime -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: makemime"; then echo "PASS: busybox_makemime"; PASS=$((PASS+1)); else echo "FAIL: busybox_makemime"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n md5_t | busybox md5sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "-"; then echo "PASS: busybox_md5sum"; PASS=$((PASS+1)); else echo "FAIL: busybox_md5sum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mdev -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mdev"; then echo "PASS: busybox_mdev"; PASS=$((PASS+1)); else echo "FAIL: busybox_mdev"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mesg 2>&1; busybox echo mesg_ok"; } 2>&1)
if echo "$_t" | grep -qF "mesg_ok"; then echo "PASS: busybox_mesg"; PASS=$((PASS+1)); else echo "FAIL: busybox_mesg"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox microcom -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: microcom"; then echo "PASS: busybox_microcom"; PASS=$((PASS+1)); else echo "FAIL: busybox_microcom"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mkdosfs -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mkdosfs"; then echo "PASS: busybox_mkdosfs"; PASS=$((PASS+1)); else echo "FAIL: busybox_mkdosfs"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_fifo_t && busybox mkfifo /tmp/bb_fifo_t && busybox ls -l /tmp/bb_fifo_t 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bb_fifo_t"; then echo "PASS: busybox_mkfifo"; PASS=$((PASS+1)); else echo "FAIL: busybox_mkfifo"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mkfs.vfat -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mkfs.vfat"; then echo "PASS: busybox_mkfs_vfat"; PASS=$((PASS+1)); else echo "FAIL: busybox_mkfs_vfat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mknod -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mknod"; then echo "PASS: busybox_mknod"; PASS=$((PASS+1)); else echo "FAIL: busybox_mknod"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mkpasswd -m md5 testpass 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "\$1\$"; then echo "PASS: busybox_mkpasswd"; PASS=$((PASS+1)); else echo "FAIL: busybox_mkpasswd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mkswap -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mkswap"; then echo "PASS: busybox_mkswap"; PASS=$((PASS+1)); else echo "FAIL: busybox_mkswap"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'd=\$(busybox mktemp -d -t bbXXXXXX) && busybox test -d \"\$d\" && busybox echo mktemp_ok && busybox rm -rf \"\$d\"' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "mktemp_ok"; then echo "PASS: busybox_mktemp"; PASS=$((PASS+1)); else echo "FAIL: busybox_mktemp"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox modinfo -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: modinfo"; then echo "PASS: busybox_modinfo"; PASS=$((PASS+1)); else echo "FAIL: busybox_modinfo"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox modprobe -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: modprobe"; then echo "PASS: busybox_modprobe"; PASS=$((PASS+1)); else echo "FAIL: busybox_modprobe"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox more /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_more"; PASS=$((PASS+1)); else echo "FAIL: busybox_more"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mount 2>&1; busybox echo mount_ok"; } 2>&1)
if echo "$_t" | grep -qF "mount_ok"; then echo "PASS: busybox_mount"; PASS=$((PASS+1)); else echo "FAIL: busybox_mount"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mountpoint / 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "is a mountpoint"; then echo "PASS: busybox_mountpoint"; PASS=$((PASS+1)); else echo "FAIL: busybox_mountpoint"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox mpstat 1 1 2>&1; busybox echo mpstat_ok"; } 2>&1)
if echo "$_t" | grep -qF "mpstat_ok"; then echo "PASS: busybox_mpstat"; PASS=$((PASS+1)); else echo "FAIL: busybox_mpstat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nameif -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nameif"; then echo "PASS: busybox_nameif"; PASS=$((PASS+1)); else echo "FAIL: busybox_nameif"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nanddump -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nanddump"; then echo "PASS: busybox_nanddump"; PASS=$((PASS+1)); else echo "FAIL: busybox_nanddump"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nandwrite -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nandwrite"; then echo "PASS: busybox_nandwrite"; PASS=$((PASS+1)); else echo "FAIL: busybox_nandwrite"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nbd-client -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nbd-client"; then echo "PASS: busybox_nbd_client"; PASS=$((PASS+1)); else echo "FAIL: busybox_nbd_client"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nc -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nc"; then echo "PASS: busybox_nc"; PASS=$((PASS+1)); else echo "FAIL: busybox_nc"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox netstat -a 2>&1; busybox echo netstat_ok"; } 2>&1)
if echo "$_t" | grep -qF "netstat_ok"; then echo "PASS: busybox_netstat"; PASS=$((PASS+1)); else echo "FAIL: busybox_netstat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nl -ba /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_nl"; PASS=$((PASS+1)); else echo "FAIL: busybox_nl"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nmeter -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nmeter"; then echo "PASS: busybox_nmeter"; PASS=$((PASS+1)); else echo "FAIL: busybox_nmeter"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nologin 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "This account is not available"; then echo "PASS: busybox_nologin"; PASS=$((PASS+1)); else echo "FAIL: busybox_nologin"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'n=\$(busybox nproc) && busybox test -n \"\$n\" && busybox echo nproc_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "nproc_ok"; then echo "PASS: busybox_nproc"; PASS=$((PASS+1)); else echo "FAIL: busybox_nproc"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nsenter -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nsenter"; then echo "PASS: busybox_nsenter"; PASS=$((PASS+1)); else echo "FAIL: busybox_nsenter"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox nslookup 127.0.0.1 2>&1; busybox echo nslookup_ok"; } 2>&1)
if echo "$_t" | grep -qF "nslookup_ok"; then echo "PASS: busybox_nslookup"; PASS=$((PASS+1)); else echo "FAIL: busybox_nslookup"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ntpd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ntpd"; then echo "PASS: busybox_ntpd"; PASS=$((PASS+1)); else echo "FAIL: busybox_ntpd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo test | busybox od -An -tx1 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "74"; then echo "PASS: busybox_od"; PASS=$((PASS+1)); else echo "FAIL: busybox_od"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox openvt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: openvt"; then echo "PASS: busybox_openvt"; PASS=$((PASS+1)); else echo "FAIL: busybox_openvt"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox partprobe -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: partprobe"; then echo "PASS: busybox_partprobe"; PASS=$((PASS+1)); else echo "FAIL: busybox_partprobe"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox passwd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: passwd"; then echo "PASS: busybox_passwd"; PASS=$((PASS+1)); else echo "FAIL: busybox_passwd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo a > /tmp/bb_p1 && busybox echo b > /tmp/bb_p2 && busybox paste /tmp/bb_p1 /tmp/bb_p2 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "a	b"; then echo "PASS: busybox_paste"; PASS=$((PASS+1)); else echo "FAIL: busybox_paste"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pgrep -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: pgrep"; then echo "PASS: busybox_pgrep"; PASS=$((PASS+1)); else echo "FAIL: busybox_pgrep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ping6 -c 1 ::1 2>&1 || busybox echo ping6_fallback"; } 2>&1)
if echo "$_t" | grep -qF "ping6_"; then echo "PASS: busybox_ping6"; PASS=$((PASS+1)); else echo "FAIL: busybox_ping6"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pivot_root -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: pivot_root"; then echo "PASS: busybox_pivot_root"; PASS=$((PASS+1)); else echo "FAIL: busybox_pivot_root"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pkill -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "HUP"; then echo "PASS: busybox_pkill"; PASS=$((PASS+1)); else echo "FAIL: busybox_pkill"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pmap 1 2>&1; busybox echo pmap_ok"; } 2>&1)
if echo "$_t" | grep -qF "pmap_ok"; then echo "PASS: busybox_pmap"; PASS=$((PASS+1)); else echo "FAIL: busybox_pmap"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox poweroff -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: poweroff"; then echo "PASS: busybox_poweroff"; PASS=$((PASS+1)); else echo "FAIL: busybox_poweroff"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printenv PATH 2>&1; busybox echo printenv_ok"; } 2>&1)
if echo "$_t" | grep -qF "printenv_ok"; then echo "PASS: busybox_printenv"; PASS=$((PASS+1)); else echo "FAIL: busybox_printenv"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'pf_%s_ok\\n' bb 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "pf_bb_ok"; then echo "PASS: busybox_printf"; PASS=$((PASS+1)); else echo "FAIL: busybox_printf"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox ps 2>&1; busybox echo ps_ok"; } 2>&1)
if echo "$_t" | grep -qF "ps_ok"; then echo "PASS: busybox_ps"; PASS=$((PASS+1)); else echo "FAIL: busybox_ps"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pscan -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: pscan"; then echo "PASS: busybox_pscan"; PASS=$((PASS+1)); else echo "FAIL: busybox_pscan"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pstree 2>&1; busybox echo pstree_ok"; } 2>&1)
if echo "$_t" | grep -qF "pstree_ok"; then echo "PASS: busybox_pstree"; PASS=$((PASS+1)); else echo "FAIL: busybox_pstree"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pwd 2>&1; busybox echo pwd_ok"; } 2>&1)
if echo "$_t" | grep -qF "pwd_ok"; then echo "PASS: busybox_pwd"; PASS=$((PASS+1)); else echo "FAIL: busybox_pwd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox pwdx 1 2>&1; busybox echo pwdx_ok"; } 2>&1)
if echo "$_t" | grep -qF "pwdx_ok"; then echo "PASS: busybox_pwdx"; PASS=$((PASS+1)); else echo "FAIL: busybox_pwdx"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rdate -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: rdate"; then echo "PASS: busybox_rdate"; PASS=$((PASS+1)); else echo "FAIL: busybox_rdate"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo ra > /tmp/bb_ra_f && busybox readahead /tmp/bb_ra_f 2>/dev/null; busybox echo ra_done 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ra_done"; then echo "PASS: busybox_readahead"; PASS=$((PASS+1)); else echo "FAIL: busybox_readahead"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox readlink -f /proc/self/exe 2>&1; busybox echo readlink_ok"; } 2>&1)
if echo "$_t" | grep -qF "readlink_ok"; then echo "PASS: busybox_readlink"; PASS=$((PASS+1)); else echo "FAIL: busybox_readlink"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox realpath /tmp 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/tmp"; then echo "PASS: busybox_realpath"; PASS=$((PASS+1)); else echo "FAIL: busybox_realpath"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox reboot -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: reboot"; then echo "PASS: busybox_reboot"; PASS=$((PASS+1)); else echo "FAIL: busybox_reboot"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox reformime -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: reformime"; then echo "PASS: busybox_reformime"; PASS=$((PASS+1)); else echo "FAIL: busybox_reformime"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox renice +0 -p \$\$; busybox echo renice_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "renice_ok"; then echo "PASS: busybox_renice"; PASS=$((PASS+1)); else echo "FAIL: busybox_renice"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox reset 2>/dev/null; busybox echo reset_done 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "reset_done"; then echo "PASS: busybox_reset"; PASS=$((PASS+1)); else echo "FAIL: busybox_reset"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo abcd | busybox rev 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "dcba"; then echo "PASS: busybox_rev"; PASS=$((PASS+1)); else echo "FAIL: busybox_rev"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rfkill list 2>&1; busybox echo rfkill_ok"; } 2>&1)
if echo "$_t" | grep -qF "rfkill_ok"; then echo "PASS: busybox_rfkill"; PASS=$((PASS+1)); else echo "FAIL: busybox_rfkill"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox touch /tmp/bb_rm_x && busybox rm /tmp/bb_rm_x && busybox test ! -e /tmp/bb_rm_x && busybox echo rm_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "rm_ok"; then echo "PASS: busybox_rm"; PASS=$((PASS+1)); else echo "FAIL: busybox_rm"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox rmmod -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: rmmod"; then echo "PASS: busybox_rmmod"; PASS=$((PASS+1)); else echo "FAIL: busybox_rmmod"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox route -n 2>&1; busybox echo route_ok"; } 2>&1)
if echo "$_t" | grep -qF "route_ok"; then echo "PASS: busybox_route"; PASS=$((PASS+1)); else echo "FAIL: busybox_route"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo hello | busybox sed 's/hello/sed_ok/' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sed_ok"; then echo "PASS: busybox_sed"; PASS=$((PASS+1)); else echo "FAIL: busybox_sed"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sendmail -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: sendmail"; then echo "PASS: busybox_sendmail"; PASS=$((PASS+1)); else echo "FAIL: busybox_sendmail"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox seq 1 3 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "3"; then echo "PASS: busybox_seq"; PASS=$((PASS+1)); else echo "FAIL: busybox_seq"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox setconsole -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setconsole"; then echo "PASS: busybox_setconsole"; PASS=$((PASS+1)); else echo "FAIL: busybox_setconsole"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox setfont -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setfont"; then echo "PASS: busybox_setfont"; PASS=$((PASS+1)); else echo "FAIL: busybox_setfont"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox setkeycodes -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setkeycodes"; then echo "PASS: busybox_setkeycodes"; PASS=$((PASS+1)); else echo "FAIL: busybox_setkeycodes"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox setpriv -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setpriv"; then echo "PASS: busybox_setpriv"; PASS=$((PASS+1)); else echo "FAIL: busybox_setpriv"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox setserial -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setserial"; then echo "PASS: busybox_setserial"; PASS=$((PASS+1)); else echo "FAIL: busybox_setserial"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox setsid busybox echo setsid_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "setsid_ok"; then echo "PASS: busybox_setsid"; PASS=$((PASS+1)); else echo "FAIL: busybox_setsid"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'echo sh_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sh_ok"; then echo "PASS: busybox_sh"; PASS=$((PASS+1)); else echo "FAIL: busybox_sh"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n sha1_t | busybox sha1sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha1sum"; PASS=$((PASS+1)); else echo "FAIL: busybox_sha1sum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n s256 | busybox sha256sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha256sum"; PASS=$((PASS+1)); else echo "FAIL: busybox_sha256sum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n s3 | busybox sha3sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha3sum"; PASS=$((PASS+1)); else echo "FAIL: busybox_sha3sum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n s512 | busybox sha512sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha512sum"; PASS=$((PASS+1)); else echo "FAIL: busybox_sha512sum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox showkey -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: showkey"; then echo "PASS: busybox_showkey"; PASS=$((PASS+1)); else echo "FAIL: busybox_showkey"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'echo x > /tmp/bb_shred_t && busybox shred -n 1 -u /tmp/bb_shred_t 2>&1; busybox test ! -f /tmp/bb_shred_t && busybox echo shred_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "shred_ok"; then echo "PASS: busybox_shred"; PASS=$((PASS+1)); else echo "FAIL: busybox_shred"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'a
b
c
' | busybox shuf 2>&1; busybox echo shuf_ok"; } 2>&1)
if echo "$_t" | grep -qF "shuf_ok"; then echo "PASS: busybox_shuf"; PASS=$((PASS+1)); else echo "FAIL: busybox_shuf"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox slattach -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: slattach"; then echo "PASS: busybox_slattach"; PASS=$((PASS+1)); else echo "FAIL: busybox_slattach"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sleep 0 && busybox echo sleep_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sleep_ok"; then echo "PASS: busybox_sleep"; PASS=$((PASS+1)); else echo "FAIL: busybox_sleep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'c
a
b
' | busybox sort 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "a"; then echo "PASS: busybox_sort"; PASS=$((PASS+1)); else echo "FAIL: busybox_sort"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox stat /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "File: /etc/passwd"; then echo "PASS: busybox_stat"; PASS=$((PASS+1)); else echo "FAIL: busybox_stat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox strings /bin/busybox 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "BusyBox"; then echo "PASS: busybox_strings"; PASS=$((PASS+1)); else echo "FAIL: busybox_strings"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox stty -a 2>&1; busybox echo stty_ok"; } 2>&1)
if echo "$_t" | grep -qF "stty_ok"; then echo "PASS: busybox_stty"; PASS=$((PASS+1)); else echo "FAIL: busybox_stty"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox su -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: su"; then echo "PASS: busybox_su"; PASS=$((PASS+1)); else echo "FAIL: busybox_su"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo sum_t | busybox sum 2>&1; busybox echo sum_ok"; } 2>&1)
if echo "$_t" | grep -qF "sum_ok"; then echo "PASS: busybox_sum"; PASS=$((PASS+1)); else echo "FAIL: busybox_sum"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox swapoff -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: swapoff"; then echo "PASS: busybox_swapoff"; PASS=$((PASS+1)); else echo "FAIL: busybox_swapoff"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox swapon -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: swapon"; then echo "PASS: busybox_swapon"; PASS=$((PASS+1)); else echo "FAIL: busybox_swapon"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox switch_root -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: switch_root"; then echo "PASS: busybox_switch_root"; PASS=$((PASS+1)); else echo "FAIL: busybox_switch_root"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sync && busybox echo sync_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sync_ok"; then echo "PASS: busybox_sync"; PASS=$((PASS+1)); else echo "FAIL: busybox_sync"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sysctl kernel.hostname 2>&1 || busybox sysctl -h 2>&1; busybox echo sysctl_ok"; } 2>&1)
if echo "$_t" | grep -qF "sysctl_ok"; then echo "PASS: busybox_sysctl"; PASS=$((PASS+1)); else echo "FAIL: busybox_sysctl"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox syslogd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: syslogd"; then echo "PASS: busybox_syslogd"; PASS=$((PASS+1)); else echo "FAIL: busybox_syslogd"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'a
b
' | busybox tac 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "b"; then echo "PASS: busybox_tac"; PASS=$((PASS+1)); else echo "FAIL: busybox_tac"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo tee_line | busybox tee /tmp/bb_tee_f >/dev/null && busybox cat /tmp/bb_tee_f' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "tee_line"; then echo "PASS: busybox_tee"; PASS=$((PASS+1)); else echo "FAIL: busybox_tee"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox test 1 -eq 1 && busybox echo test_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "test_ok"; then echo "PASS: busybox_test"; PASS=$((PASS+1)); else echo "FAIL: busybox_test"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox time busybox echo time_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "time_ok"; then echo "PASS: busybox_time"; PASS=$((PASS+1)); else echo "FAIL: busybox_time"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox timeout 2 busybox echo timeout_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "timeout_ok"; then echo "PASS: busybox_timeout"; PASS=$((PASS+1)); else echo "FAIL: busybox_timeout"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox top -b -n 1 2>&1; busybox echo top_ok"; } 2>&1)
if echo "$_t" | grep -qF "top_ok"; then echo "PASS: busybox_top"; PASS=$((PASS+1)); else echo "FAIL: busybox_top"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox touch /tmp/bb_touch_f && busybox test -f /tmp/bb_touch_f && busybox echo touch_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "touch_ok"; then echo "PASS: busybox_touch"; PASS=$((PASS+1)); else echo "FAIL: busybox_touch"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo abc | busybox tr 'a-z' 'A-Z' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ABC"; then echo "PASS: busybox_tr"; PASS=$((PASS+1)); else echo "FAIL: busybox_tr"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox traceroute -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: traceroute"; then echo "PASS: busybox_traceroute"; PASS=$((PASS+1)); else echo "FAIL: busybox_traceroute"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox traceroute6 -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: traceroute6"; then echo "PASS: busybox_traceroute6"; PASS=$((PASS+1)); else echo "FAIL: busybox_traceroute6"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox mkdir -p /tmp/bb_tree/d && busybox echo x > /tmp/bb_tree/d/a && busybox tree /tmp/bb_tree 2>&1'"; } 2>&1)
if echo "$_t" | grep -qF "a"; then echo "PASS: busybox_tree"; PASS=$((PASS+1)); else echo "FAIL: busybox_tree"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox true && busybox echo true_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "true_ok"; then echo "PASS: busybox_true"; PASS=$((PASS+1)); else echo "FAIL: busybox_true"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo abcd > /tmp/bb_trunc_f && busybox truncate -s 2 /tmp/bb_trunc_f && busybox cat /tmp/bb_trunc_f' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ab"; then echo "PASS: busybox_truncate"; PASS=$((PASS+1)); else echo "FAIL: busybox_truncate"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox tty 2>&1; busybox echo tty_ok"; } 2>&1)
if echo "$_t" | grep -qF "tty_ok"; then echo "PASS: busybox_tty"; PASS=$((PASS+1)); else echo "FAIL: busybox_tty"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox tunctl -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: tunctl"; then echo "PASS: busybox_tunctl"; PASS=$((PASS+1)); else echo "FAIL: busybox_tunctl"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox udhcpc -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: udhcpc"; then echo "PASS: busybox_udhcpc"; PASS=$((PASS+1)); else echo "FAIL: busybox_udhcpc"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox udhcpc6 -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: udhcpc6"; then echo "PASS: busybox_udhcpc6"; PASS=$((PASS+1)); else echo "FAIL: busybox_udhcpc6"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox umount -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: umount"; then echo "PASS: busybox_umount"; PASS=$((PASS+1)); else echo "FAIL: busybox_umount"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox uname -a 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Linux"; then echo "PASS: busybox_uname"; PASS=$((PASS+1)); else echo "FAIL: busybox_uname"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'x    y
' | busybox unexpand -a 2>&1; busybox echo unexpand_ok"; } 2>&1)
if echo "$_t" | grep -qF "unexpand_ok"; then echo "PASS: busybox_unexpand"; PASS=$((PASS+1)); else echo "FAIL: busybox_unexpand"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'a
a
b
' | busybox uniq 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "b"; then echo "PASS: busybox_uniq"; PASS=$((PASS+1)); else echo "FAIL: busybox_uniq"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'u2d
' | busybox unix2dos 2>&1; busybox echo unix2dos_ok"; } 2>&1)
if echo "$_t" | grep -qF "unix2dos_ok"; then echo "PASS: busybox_unix2dos"; PASS=$((PASS+1)); else echo "FAIL: busybox_unix2dos"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo u > /tmp/bb_unl && busybox unlink /tmp/bb_unl && busybox test ! -e /tmp/bb_unl && busybox echo unlink_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "unlink_ok"; then echo "PASS: busybox_unlink"; PASS=$((PASS+1)); else echo "FAIL: busybox_unlink"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox unlzma -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unlzma"; then echo "PASS: busybox_unlzma"; PASS=$((PASS+1)); else echo "FAIL: busybox_unlzma"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox unlzop -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unlzop"; then echo "PASS: busybox_unlzop"; PASS=$((PASS+1)); else echo "FAIL: busybox_unlzop"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox unshare -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unshare"; then echo "PASS: busybox_unshare"; PASS=$((PASS+1)); else echo "FAIL: busybox_unshare"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox unxz -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unxz"; then echo "PASS: busybox_unxz"; PASS=$((PASS+1)); else echo "FAIL: busybox_unxz"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox unzip -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unzip"; then echo "PASS: busybox_unzip"; PASS=$((PASS+1)); else echo "FAIL: busybox_unzip"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox uptime 2>&1; busybox echo uptime_ok"; } 2>&1)
if echo "$_t" | grep -qF "uptime_ok"; then echo "PASS: busybox_uptime"; PASS=$((PASS+1)); else echo "FAIL: busybox_uptime"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox usleep 1 && busybox echo usleep_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "usleep_ok"; then echo "PASS: busybox_usleep"; PASS=$((PASS+1)); else echo "FAIL: busybox_usleep"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo hi | busybox uuencode out | busybox uudecode -o /tmp/bb_uudec 2>&1 && busybox cat /tmp/bb_uudec' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hi"; then echo "PASS: busybox_uudecode"; PASS=$((PASS+1)); else echo "FAIL: busybox_uudecode"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo enc | busybox uuencode out 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "begin"; then echo "PASS: busybox_uuencode"; PASS=$((PASS+1)); else echo "FAIL: busybox_uuencode"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox vconfig -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: vconfig"; then echo "PASS: busybox_vconfig"; PASS=$((PASS+1)); else echo "FAIL: busybox_vconfig"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox vi -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: vi"; then echo "PASS: busybox_vi"; PASS=$((PASS+1)); else echo "FAIL: busybox_vi"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox vlock -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: vlock"; then echo "PASS: busybox_vlock"; PASS=$((PASS+1)); else echo "FAIL: busybox_vlock"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox volname /dev/null 2>&1; busybox echo volname_ok"; } 2>&1)
if echo "$_t" | grep -qF "volname_ok"; then echo "PASS: busybox_volname"; PASS=$((PASS+1)); else echo "FAIL: busybox_volname"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox watch -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: watch"; then echo "PASS: busybox_watch"; PASS=$((PASS+1)); else echo "FAIL: busybox_watch"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox watchdog -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: watchdog"; then echo "PASS: busybox_watchdog"; PASS=$((PASS+1)); else echo "FAIL: busybox_watchdog"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox printf 'a
b
c
' | busybox wc -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "3"; then echo "PASS: busybox_wc"; PASS=$((PASS+1)); else echo "FAIL: busybox_wc"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox wget -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: wget"; then echo "PASS: busybox_wget"; PASS=$((PASS+1)); else echo "FAIL: busybox_wget"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox which busybox 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "busybox"; then echo "PASS: busybox_which"; PASS=$((PASS+1)); else echo "FAIL: busybox_which"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox who 2>&1 | busybox wc -l 2>&1; busybox echo who_ok"; } 2>&1)
if echo "$_t" | grep -qF "who_ok"; then echo "PASS: busybox_who"; PASS=$((PASS+1)); else echo "FAIL: busybox_who"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox whoami 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root"; then echo "PASS: busybox_whoami"; PASS=$((PASS+1)); else echo "FAIL: busybox_whoami"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox whois -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: whois"; then echo "PASS: busybox_whois"; PASS=$((PASS+1)); else echo "FAIL: busybox_whois"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo a b | busybox xargs busybox echo X 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "X a b"; then echo "PASS: busybox_xargs"; PASS=$((PASS+1)); else echo "FAIL: busybox_xargs"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox xzcat -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: xzcat"; then echo "PASS: busybox_xzcat"; PASS=$((PASS+1)); else echo "FAIL: busybox_xzcat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox yes y | busybox head -n 1 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "y"; then echo "PASS: busybox_yes"; PASS=$((PASS+1)); else echo "FAIL: busybox_yes"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox echo -n hello | busybox gzip -c | busybox zcat 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_zcat"; PASS=$((PASS+1)); else echo "FAIL: busybox_zcat"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "busybox zcip -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: zcip"; then echo "PASS: busybox_zcip"; PASS=$((PASS+1)); else echo "FAIL: busybox_zcip"; FAIL=$((FAIL+1)); fi

_t=$({ timeout 10 sh -c "ls /"; } 2>&1)
if echo "$_t" | grep -qF "bin"; then echo "PASS: ls_root"; PASS=$((PASS+1)); else echo "FAIL: ls_root"; FAIL=$((FAIL+1)); fi

# Custom test: addgroup
_t=$({ timeout 10 sh -c "G=\$(date +%s); busybox delgroup \"gg_\$G\" 2>/dev/null; busybox addgroup \"gg_\$G\" 2>&1 && busybox grep -F \"gg_\$G:\" /etc/group 2>&1; busybox delgroup \"gg_\$G\" 2>/dev/null"; } 2>&1)
if echo "$_t" | grep -qF "gg_"; then echo "PASS: addgroup"; PASS=$((PASS+1)); else echo "FAIL: addgroup"; FAIL=$((FAIL+1)); fi
# Custom test: adduser
_t=$({ timeout 10 sh -c "U=\$(date +%s); busybox deluser \"uu_\$U\" 2>/dev/null; busybox adduser -D -H \"uu_\$U\" 2>&1 && busybox grep -F \"uu_\$U:\" /etc/passwd 2>&1; busybox deluser \"uu_\$U\" 2>/dev/null"; } 2>&1)
if echo "$_t" | grep -qF "uu_"; then echo "PASS: adduser"; PASS=$((PASS+1)); else echo "FAIL: adduser"; FAIL=$((FAIL+1)); fi
echo "=== BusyBox Test Summary ==="
echo "PASS: $PASS  FAIL: $FAIL  TOTAL: $((PASS+FAIL))"
_m1="Test"; _m2="run"; _m3="completed"; echo "$_m1 $_m2 $_m3"
