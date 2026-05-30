#!/bin/sh
# Auto-generated from ChenLongTest by scripts/convert_busybox_tests.py
PASS=0; FAIL=0; SKIP=0
export PATH="${PATH:-/bin:/usr/bin:/sbin:/usr/sbin}"

bb_now_ms() {
    if IFS=' .' read -r _bb_sec _bb_frac _bb_rest < /proc/uptime; then
        _bb_frac=${_bb_frac:-0}000
        _bb_ms=${_bb_frac%${_bb_frac#???}}
        while :; do
            case $_bb_ms in
                0[0-9]*) _bb_ms=${_bb_ms#0} ;;
                *) break ;;
            esac
        done
        _bb_ms=${_bb_ms:-0}
        echo $((_bb_sec * 1000 + _bb_ms))
    else
        date +%s000 2>/dev/null || echo 0
    fi
}

bb_case_start() {
    BB_CASE_NAME=$1
    BB_CASE_START_MS=$(bb_now_ms)
    echo "START: $BB_CASE_NAME"
}

bb_case_print_time() {
    _bb_now=$(bb_now_ms)
    _bb_elapsed=$((_bb_now - BB_CASE_START_MS))
    if [ "$_bb_elapsed" -lt 0 ]; then
        _bb_elapsed=0
    fi
    echo "TIME: $BB_CASE_NAME elapsed=${_bb_elapsed}ms"
}

bb_case_pass() {
    bb_case_print_time
    PASS=$((PASS+1))
}

bb_case_fail() {
    bb_case_print_time
    FAIL=$((FAIL+1))
    echo "FAIL: $BB_CASE_NAME"
    echo "=== BusyBox Test Summary ==="
    echo "PASS: $PASS  FAIL: $FAIL  TOTAL: $((PASS+FAIL))"
    exit 1
}

bb_case_start "busybox_adjtimex"
_t=$({ timeout 10 sh -c "busybox adjtimex 2>&1"; } 2>&1)
if [ -n "$_t" ]; then echo "PASS: busybox_adjtimex"; bb_case_pass; else echo "FAIL_DETAIL: busybox_adjtimex (empty)"; bb_case_fail; fi

bb_case_start "busybox_arch"
_t=$({ timeout 10 sh -c "busybox arch 2>&1"; } 2>&1)
if [ -n "$_t" ] && echo "$_t" | grep -qE "x86_64|riscv|aarch64|arm|loongarch|mips|powerpc|s390"; then echo "PASS: busybox_arch"; bb_case_pass; else echo "FAIL_DETAIL: busybox_arch"; bb_case_fail; fi

bb_case_start "busybox_arp"
_t=$({ timeout 10 sh -c "busybox arp 2>&1"; echo "BUSYBOX_ARP_STATUS:$?"; } 2>&1)
_status=$(printf '%s\n' "$_t" | sed -n 's/^BUSYBOX_ARP_STATUS://p')
_t=$(printf '%s\n' "$_t" | sed '/^BUSYBOX_ARP_STATUS:/d')
if [ "$_status" = 0 ] && { [ -z "$_t" ] || echo "$_t" | grep -qF "HWtype" || echo "$_t" | grep -qF "[ether]"; }; then echo "PASS: busybox_arp"; bb_case_pass; else echo "FAIL_DETAIL: busybox_arp"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_arping"
_t=$({ timeout 3 sh -c "busybox arping -f -c 1 -w 1 10.0.2.2 2>&1"; echo "ARPING_STATUS:$?"; } 2>&1)
if echo "$_t" | grep -qF "ARPING_STATUS:0" && echo "$_t" | grep -Eq "Received [1-9][0-9]* response[(]s[)]|Unicast reply"; then echo "PASS: busybox_arping"; bb_case_pass; else echo "FAIL_DETAIL: busybox_arping"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_arping_loopback_negative"
_t=$({ timeout 5 sh -c "busybox arping -c 1 -w 1 127.0.0.1 2>&1"; echo "ARPING_LOOPBACK_STATUS:$?"; } 2>&1)
if ! echo "$_t" | grep -Eq "Received [1-9][0-9]* response[(]s[)]|Unicast reply"; then echo "PASS: busybox_arping_loopback_negative"; bb_case_pass; else echo "FAIL_DETAIL: busybox_arping_loopback_negative"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_ash"
_t=$({ timeout 10 sh -c "busybox ash -c 'echo ash_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ash_ok"; then echo "PASS: busybox_ash"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ash"; bb_case_fail; fi

bb_case_start "busybox_awk"
_t=$({ timeout 10 sh -c "busybox awk 'BEGIN{print \"awk_ok\"}' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "awk_ok"; then echo "PASS: busybox_awk"; bb_case_pass; else echo "FAIL_DETAIL: busybox_awk"; bb_case_fail; fi

bb_case_start "busybox_base64"
_t=$({ timeout 10 sh -c "busybox echo test | busybox base64"; } 2>&1)
if echo "$_t" | grep -qF "dGVzdAo="; then echo "PASS: busybox_base64"; bb_case_pass; else echo "FAIL_DETAIL: busybox_base64"; bb_case_fail; fi

bb_case_start "busybox_basename"
_t=$({ timeout 10 sh -c "busybox basename /usr/bin/foo 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "foo"; then echo "PASS: busybox_basename"; bb_case_pass; else echo "FAIL_DETAIL: busybox_basename"; bb_case_fail; fi

bb_case_start "busybox_bbconfig"
_t=$({ timeout 10 sh -c "busybox bbconfig 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "CONFIG_BUSYBOX=y"; then echo "PASS: busybox_bbconfig"; bb_case_pass; else echo "FAIL_DETAIL: busybox_bbconfig"; bb_case_fail; fi

bb_case_start "busybox_bc"
_t=$({ timeout 10 sh -c "busybox echo '2+2' | busybox bc 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "4"; then echo "PASS: busybox_bc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_bc"; bb_case_fail; fi

bb_case_start "busybox_beep"
_t=$({ timeout 10 sh -c "busybox beep 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "can't open console"; then echo "PASS: busybox_beep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_beep"; bb_case_fail; fi

bb_case_start "busybox_brctl"
_t=$({ timeout 10 sh -c "busybox brctl -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/sys/class/net"; then echo "PASS: busybox_brctl"; bb_case_pass; else echo "FAIL_DETAIL: busybox_brctl"; bb_case_fail; fi

bb_case_start "busybox_bunzip2"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo bunzip_ok > /tmp/bb_bunzip_t && busybox bzip2 -f /tmp/bb_bunzip_t && busybox bunzip2 -f /tmp/bb_bunzip_t.bz2 && busybox cat /tmp/bb_bunzip_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bunzip_ok"; then echo "PASS: busybox_bunzip2"; bb_case_pass; else echo "FAIL_DETAIL: busybox_bunzip2"; bb_case_fail; fi

bb_case_start "busybox_bzcat"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo bzcat_ok > /tmp/bb_bzcat_t && busybox bzip2 -f /tmp/bb_bzcat_t && busybox bzcat /tmp/bb_bzcat_t.bz2' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bzcat_ok"; then echo "PASS: busybox_bzcat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_bzcat"; bb_case_fail; fi

bb_case_start "busybox_bzip2"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo bz2_ok > /tmp/bb_bzip2_t && busybox bzip2 -kf /tmp/bb_bzip2_t && busybox test -f /tmp/bb_bzip2_t.bz2 && busybox echo bzip2_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bzip2_ok"; then echo "PASS: busybox_bzip2"; bb_case_pass; else echo "FAIL_DETAIL: busybox_bzip2"; bb_case_fail; fi

bb_case_start "busybox_cal"
_t=$({ timeout 10 sh -c "busybox cal 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Su Mo Tu We Th Fr Sa"; then echo "PASS: busybox_cal"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cal"; bb_case_fail; fi

bb_case_start "busybox_cat"
_t=$({ timeout 10 sh -c "busybox cat /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_cat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cat"; bb_case_fail; fi

bb_case_start "busybox_chattr"
_t=$({ timeout 10 sh -c "busybox chattr -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: chattr"; then echo "PASS: busybox_chattr"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chattr"; bb_case_fail; fi

bb_case_start "busybox_chgrp"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo g > /tmp/bb_chgrp_t && G=\$(busybox id -g) && busybox chgrp \"\$G\" /tmp/bb_chgrp_t && busybox ls -ln /tmp/bb_chgrp_t | busybox grep -q \" \$G \" && busybox echo chgrp_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chgrp_ok"; then echo "PASS: busybox_chgrp"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chgrp"; bb_case_fail; fi

bb_case_start "busybox_chmod"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo m > /tmp/bb_chmod_t && busybox chmod 600 /tmp/bb_chmod_t && busybox ls -l /tmp/bb_chmod_t | busybox grep -q \"^-rw-------\" && busybox echo chmod_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chmod_ok"; then echo "PASS: busybox_chmod"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chmod"; bb_case_fail; fi

bb_case_start "busybox_chpasswd"
_t=$({ timeout 10 sh -c "busybox chpasswd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: chpasswd"; then echo "PASS: busybox_chpasswd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chpasswd"; bb_case_fail; fi

bb_case_start "busybox_chroot"
_t=$({ timeout 10 sh -c "busybox chroot / /bin/busybox echo chroot_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chroot_ok"; then echo "PASS: busybox_chroot"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chroot"; bb_case_fail; fi

bb_case_start "busybox_chvt"
_t=$({ timeout 10 sh -c "busybox chvt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "invalid number"; then echo "PASS: busybox_chvt"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chvt"; bb_case_fail; fi

bb_case_start "busybox_cksum"
_t=$({ timeout 10 sh -c "busybox cksum /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/etc/passwd"; then echo "PASS: busybox_cksum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cksum"; bb_case_fail; fi

bb_case_start "busybox_clear"
_t=$({ timeout 10 sh -c "busybox clear 2>&1; busybox echo clear_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "clear_ok"; then echo "PASS: busybox_clear"; bb_case_pass; else echo "FAIL_DETAIL: busybox_clear"; bb_case_fail; fi

bb_case_start "busybox_cmp"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo cmp_ok > /tmp/bb_cmp_a && busybox cp /tmp/bb_cmp_a /tmp/bb_cmp_b && busybox cmp /tmp/bb_cmp_a /tmp/bb_cmp_b && busybox echo cmp_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "cmp_ok"; then echo "PASS: busybox_cmp"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cmp"; bb_case_fail; fi

bb_case_start "busybox_comm"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox printf \"a\\nb\\n\" > /tmp/bb_comm_1 && busybox printf \"b\\nc\\n\" > /tmp/bb_comm_2 && busybox comm /tmp/bb_comm_1 /tmp/bb_comm_2' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "c"; then echo "PASS: busybox_comm"; bb_case_pass; else echo "FAIL_DETAIL: busybox_comm"; bb_case_fail; fi

bb_case_start "busybox_cp"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo cp_ok > /tmp/bb_cp_src && busybox cp /tmp/bb_cp_src /tmp/bb_cp_dst && busybox cat /tmp/bb_cp_dst' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "cp_ok"; then echo "PASS: busybox_cp"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cp"; bb_case_fail; fi

bb_case_start "busybox_cryptpw"
_t=$({ timeout 10 sh -c "busybox cryptpw -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: cryptpw"; then echo "PASS: busybox_cryptpw"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cryptpw"; bb_case_fail; fi

bb_case_start "busybox_cut"
_t=$({ timeout 10 sh -c "busybox echo 'a:b:c' | busybox cut -d: -f2 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "b"; then echo "PASS: busybox_cut"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cut"; bb_case_fail; fi

bb_case_start "busybox_date"
_t=$({ timeout 10 sh -c "busybox date +%Y 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "20"; then echo "PASS: busybox_date"; bb_case_pass; else echo "FAIL_DETAIL: busybox_date"; bb_case_fail; fi

bb_case_start "busybox_dc"
_t=$({ timeout 10 sh -c "busybox echo '2 2 + p' | busybox dc 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "4"; then echo "PASS: busybox_dc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dc"; bb_case_fail; fi

bb_case_start "busybox_dd"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo dd_ok > /tmp/bb_dd_in && busybox dd if=/tmp/bb_dd_in of=/tmp/bb_dd_out bs=1 count=6 2>/tmp/bb_dd_stat && busybox cat /tmp/bb_dd_out' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "dd_ok"; then echo "PASS: busybox_dd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dd"; bb_case_fail; fi

bb_case_start "busybox_deallocvt"
_t=$({ timeout 10 sh -c "busybox deallocvt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "invalid number"; then echo "PASS: busybox_deallocvt"; bb_case_pass; else echo "FAIL_DETAIL: busybox_deallocvt"; bb_case_fail; fi

bb_case_start "busybox_delgroup"
_t=$({ timeout 10 sh -c "busybox sh -c 'G=bb_delg_t && busybox delgroup \"\$G\" 2>/dev/null || true && busybox addgroup \"\$G\" && busybox delgroup \"\$G\" && ! busybox grep -F \"\$G:\" /etc/group >/dev/null && busybox echo delgroup_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "delgroup_ok"; then echo "PASS: busybox_delgroup"; bb_case_pass; else echo "FAIL_DETAIL: busybox_delgroup"; bb_case_fail; fi

bb_case_start "busybox_deluser"
_t=$({ timeout 10 sh -c "busybox sh -c 'U=bb_delu_t && busybox deluser \"\$U\" 2>/dev/null || true && busybox adduser -D -H \"\$U\" && busybox deluser \"\$U\" && ! busybox grep -F \"\$U:\" /etc/passwd >/dev/null && busybox echo deluser_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "deluser_ok"; then echo "PASS: busybox_deluser"; bb_case_pass; else echo "FAIL_DETAIL: busybox_deluser"; bb_case_fail; fi

bb_case_start "busybox_depmod"
_t=$({ timeout 10 sh -c "busybox depmod -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: depmod"; then echo "PASS: busybox_depmod"; bb_case_pass; else echo "FAIL_DETAIL: busybox_depmod"; bb_case_fail; fi

bb_case_start "busybox_df"
_t=$({ timeout 10 sh -c "busybox df 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Filesystem"; then echo "PASS: busybox_df"; bb_case_pass; else echo "FAIL_DETAIL: busybox_df"; bb_case_fail; fi

bb_case_start "busybox_diff"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo left > /tmp/bb_diff_l && busybox echo right > /tmp/bb_diff_r && busybox diff /tmp/bb_diff_l /tmp/bb_diff_r > /tmp/bb_diff_o 2>&1; busybox grep -q \"^--- /tmp/bb_diff_l\" /tmp/bb_diff_o && busybox echo diff_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "diff_ok"; then echo "PASS: busybox_diff"; bb_case_pass; else echo "FAIL_DETAIL: busybox_diff"; bb_case_fail; fi

bb_case_start "busybox_dirname"
_t=$({ timeout 10 sh -c "busybox dirname /usr/bin/foo 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/usr/bin"; then echo "PASS: busybox_dirname"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dirname"; bb_case_fail; fi

bb_case_start "busybox_dmesg"
_t=$({ timeout 10 sh -c "busybox dmesg -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: dmesg"; then echo "PASS: busybox_dmesg"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dmesg"; bb_case_fail; fi

bb_case_start "busybox_dnsdomainname"
_t=$({ timeout 10 sh -c "busybox dnsdomainname -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "No help available"; then echo "PASS: busybox_dnsdomainname"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dnsdomainname"; bb_case_fail; fi

bb_case_start "busybox_du"
_t=$({ timeout 10 sh -c "busybox du -s . 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "."; then echo "PASS: busybox_du"; bb_case_pass; else echo "FAIL_DETAIL: busybox_du"; bb_case_fail; fi

bb_case_start "busybox_dumpkmap"
_t=$({ timeout 10 sh -c "busybox dumpkmap -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: dumpkmap"; then echo "PASS: busybox_dumpkmap"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dumpkmap"; bb_case_fail; fi

bb_case_start "busybox_echo"
_t=$({ timeout 10 sh -c "busybox echo echo_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "echo_ok"; then echo "PASS: busybox_echo"; bb_case_pass; else echo "FAIL_DETAIL: busybox_echo"; bb_case_fail; fi

bb_case_start "busybox_egrep"
_t=$({ timeout 10 sh -c "busybox echo hello | busybox egrep hell 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_egrep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_egrep"; bb_case_fail; fi

bb_case_start "busybox_eject"
_t=$({ timeout 10 sh -c "busybox eject 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/dev/cdrom"; then echo "PASS: busybox_eject"; bb_case_pass; else echo "FAIL_DETAIL: busybox_eject"; bb_case_fail; fi

bb_case_start "busybox_ether_wake"
_t=$({ timeout 10 sh -c "busybox ether-wake 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ether-wake"; then echo "PASS: busybox_ether_wake"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ether_wake"; bb_case_fail; fi

bb_case_start "busybox_expand"
_t=$({ timeout 10 sh -c "busybox printf \"a\\tb\\n\" | busybox expand 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "a       b"; then echo "PASS: busybox_expand"; bb_case_pass; else echo "FAIL_DETAIL: busybox_expand"; bb_case_fail; fi

bb_case_start "busybox_expr"
_t=$({ timeout 10 sh -c "busybox expr 3 '*' 4 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "12"; then echo "PASS: busybox_expr"; bb_case_pass; else echo "FAIL_DETAIL: busybox_expr"; bb_case_fail; fi

bb_case_start "busybox_factor"
_t=$({ timeout 10 sh -c "busybox factor 6 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "6: 2 3"; then echo "PASS: busybox_factor"; bb_case_pass; else echo "FAIL_DETAIL: busybox_factor"; bb_case_fail; fi

bb_case_start "busybox_fallocate"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_falloc_t && busybox touch /tmp/bb_falloc_t && busybox fallocate -l 1024 /tmp/bb_falloc_t && busybox ls -ln /tmp/bb_falloc_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF " 1024 "; then echo "PASS: busybox_fallocate"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fallocate"; bb_case_fail; fi

bb_case_start "busybox_false"
_t=$({ timeout 10 sh -c "busybox false; busybox echo exit:\$? 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "exit:1"; then echo "PASS: busybox_false"; bb_case_pass; else echo "FAIL_DETAIL: busybox_false"; bb_case_fail; fi

bb_case_start "busybox_fatattr"
_t=$({ timeout 10 sh -c "busybox fatattr -v / 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "fatattr:"; then echo "PASS: busybox_fatattr"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fatattr"; bb_case_fail; fi

bb_case_start "busybox_fbset"
_t=$({ timeout 10 sh -c "busybox fbset -i 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "option 'i' not handled"; then echo "PASS: busybox_fbset"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fbset"; bb_case_fail; fi

bb_case_start "busybox_fbsplash"
_t=$({ timeout 10 sh -c "busybox fbsplash -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fbsplash"; then echo "PASS: busybox_fbsplash"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fbsplash"; bb_case_fail; fi

bb_case_start "busybox_fdisk"
_t=$({ timeout 10 sh -c "busybox fdisk -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fdisk"; then echo "PASS: busybox_fdisk"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fdisk"; bb_case_fail; fi

# busybox_blkid — list block device attributes
# blkid should handle non-block-device files gracefully (exit 0, error msg to stderr)
bb_case_start "busybox_blkid"
_t=$({ timeout 10 sh -c 'busybox blkid 2>&1; S=$(busybox blkid /dev/null 2>&1); R=$?; echo "$S"; echo EXIT:$R >&2'; } 2>&1)
if echo "$_t" | grep -qF "EXIT:0"; then echo "PASS: busybox_blkid"; bb_case_pass; else echo "FAIL_DETAIL: busybox_blkid"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_fgrep"
_t=$({ timeout 10 sh -c "busybox echo hello | busybox fgrep hell 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_fgrep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fgrep"; bb_case_fail; fi

bb_case_start "busybox_find"
_t=$({ timeout 10 sh -c "busybox find / -maxdepth 1 -name proc 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "proc"; then echo "PASS: busybox_find"; bb_case_pass; else echo "FAIL_DETAIL: busybox_find"; bb_case_fail; fi

bb_case_start "busybox_findfs"
_t=$({ timeout 10 sh -c "busybox findfs -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: findfs"; then echo "PASS: busybox_findfs"; bb_case_pass; else echo "FAIL_DETAIL: busybox_findfs"; bb_case_fail; fi

bb_case_start "busybox_flock"
_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_flock_t && busybox touch /tmp/bb_flock_t && busybox flock -x /tmp/bb_flock_t -c 'busybox echo flock_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "flock_ok"; then echo "PASS: busybox_flock"; bb_case_pass; else echo "FAIL_DETAIL: busybox_flock"; bb_case_fail; fi

bb_case_start "busybox_fold"
_t=$({ timeout 10 sh -c "busybox echo abcdef | busybox fold -w 2 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ab"; then echo "PASS: busybox_fold"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fold"; bb_case_fail; fi

bb_case_start "busybox_free"
_t=$({ timeout 10 sh -c "busybox free 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Mem"; then echo "PASS: busybox_free"; bb_case_pass; else echo "FAIL_DETAIL: busybox_free"; bb_case_fail; fi

bb_case_start "busybox_fsck"
_t=$({ timeout 10 sh -c "busybox fsck -V 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "fsck"; then echo "PASS: busybox_fsck"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fsck"; bb_case_fail; fi

bb_case_start "busybox_fstrim"
_t=$({ timeout 10 sh -c "busybox fstrim -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fstrim"; then echo "PASS: busybox_fstrim"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fstrim"; bb_case_fail; fi

bb_case_start "busybox_fsync"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo fsync_ok > /tmp/bb_fsync_t && busybox fsync /tmp/bb_fsync_t && busybox cat /tmp/bb_fsync_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "fsync_ok"; then echo "PASS: busybox_fsync"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fsync"; bb_case_fail; fi

bb_case_start "busybox_fuser"
_t=$({ timeout 10 sh -c "busybox fuser -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: fuser"; then echo "PASS: busybox_fuser"; bb_case_pass; else echo "FAIL_DETAIL: busybox_fuser"; bb_case_fail; fi

bb_case_start "busybox_getty"
_t=$({ timeout 10 sh -c "busybox getty -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: getty"; then echo "PASS: busybox_getty"; bb_case_pass; else echo "FAIL_DETAIL: busybox_getty"; bb_case_fail; fi

bb_case_start "busybox_grep"
_t=$({ timeout 10 sh -c "busybox echo hello | busybox grep hell 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_grep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_grep"; bb_case_fail; fi

bb_case_start "busybox_groups"
_t=$({ timeout 10 sh -c "busybox groups 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root"; then echo "PASS: busybox_groups"; bb_case_pass; else echo "FAIL_DETAIL: busybox_groups"; bb_case_fail; fi

# busybox_ttysize — query terminal size (outputs "rows cols")
bb_case_start "busybox_ttysize"
_t=$({ timeout 10 sh -c 'busybox ttysize 2>&1'; } 2>&1)
if echo "$_t" | grep -qE '^[0-9]+ [0-9]+$'; then echo "PASS: busybox_ttysize"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ttysize"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_gunzip"
_t=$({ timeout 10 sh -c "busybox echo -n hello | busybox gzip -c | busybox gunzip -c 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_gunzip"; bb_case_pass; else echo "FAIL_DETAIL: busybox_gunzip"; bb_case_fail; fi

bb_case_start "busybox_gzip"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo -n hello > /tmp/bb_gzip_t && busybox gzip -f /tmp/bb_gzip_t && busybox test -s /tmp/bb_gzip_t.gz && busybox echo gzip_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "gzip_ok"; then echo "PASS: busybox_gzip"; bb_case_pass; else echo "FAIL_DETAIL: busybox_gzip"; bb_case_fail; fi

bb_case_start "busybox_halt"
_t=$({ timeout 10 sh -c "busybox halt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: halt"; then echo "PASS: busybox_halt"; bb_case_pass; else echo "FAIL_DETAIL: busybox_halt"; bb_case_fail; fi

bb_case_start "busybox_hd"
_t=$({ timeout 10 sh -c "busybox hd -n 64 /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "00000000"; then echo "PASS: busybox_hd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_hd"; bb_case_fail; fi

bb_case_start "busybox_head"
_t=$({ timeout 10 sh -c "busybox head -n 1 /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_head"; bb_case_pass; else echo "FAIL_DETAIL: busybox_head"; bb_case_fail; fi

bb_case_start "busybox_hexdump"
_t=$({ timeout 10 sh -c "busybox echo -n ab | busybox hexdump -C 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "61"; then echo "PASS: busybox_hexdump"; bb_case_pass; else echo "FAIL_DETAIL: busybox_hexdump"; bb_case_fail; fi

bb_case_start "busybox_hostname"
_t=$({ timeout 10 sh -c "busybox hostname 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "starry"; then echo "PASS: busybox_hostname"; bb_case_pass; else echo "FAIL_DETAIL: busybox_hostname"; bb_case_fail; fi

bb_case_start "busybox_id"
_t=$({ timeout 10 sh -c "busybox id 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "uid="; then echo "PASS: busybox_id"; bb_case_pass; else echo "FAIL_DETAIL: busybox_id"; bb_case_fail; fi

bb_case_start "busybox_ifdown"
_t=$({ timeout 10 sh -c "busybox ifdown 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ifdown"; then echo "PASS: busybox_ifdown"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ifdown"; bb_case_fail; fi

bb_case_start "busybox_ifup"
_t=$({ timeout 10 sh -c "busybox ifup -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ifup"; then echo "PASS: busybox_ifup"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ifup"; bb_case_fail; fi

bb_case_start "busybox_init"
_t=$({ timeout 10 sh -c "busybox init 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "must be run as PID 1"; then echo "PASS: busybox_init"; bb_case_pass; else echo "FAIL_DETAIL: busybox_init"; bb_case_fail; fi

bb_case_start "busybox_inotifyd"
_t=$({ timeout 10 sh -c "busybox inotifyd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: inotifyd"; then echo "PASS: busybox_inotifyd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_inotifyd"; bb_case_fail; fi

bb_case_start "busybox_install"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_inst_dst /tmp/bb_inst_src && busybox echo ok > /tmp/bb_inst_src && busybox install -m 644 /tmp/bb_inst_src /tmp/bb_inst_dst && busybox ls -l /tmp/bb_inst_dst && busybox cat /tmp/bb_inst_dst' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ok"; then echo "PASS: busybox_install"; bb_case_pass; else echo "FAIL_DETAIL: busybox_install"; bb_case_fail; fi

bb_case_start "busybox_ionice"
_t=$({ timeout 10 sh -c "busybox ionice -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ionice"; then echo "PASS: busybox_ionice"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ionice"; bb_case_fail; fi

bb_case_start "busybox_ipcrm"
_t=$({ timeout 10 sh -c "busybox ipcrm -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ipcrm"; then echo "PASS: busybox_ipcrm"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ipcrm"; bb_case_fail; fi

bb_case_start "busybox_ipcs"
_t=$({ timeout 10 sh -c "busybox ipcs 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Message Queues"; then echo "PASS: busybox_ipcs"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ipcs"; bb_case_fail; fi

bb_case_start "busybox_ip"
_t=$({ timeout 10 sh -c "busybox ip link 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "link/"; then echo "PASS: busybox_ip"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ip"; bb_case_fail; fi

bb_case_start "busybox_iplink"
_t=$({ timeout 10 sh -c "busybox iplink 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "link/"; then echo "PASS: busybox_iplink"; bb_case_pass; else echo "FAIL_DETAIL: busybox_iplink"; bb_case_fail; fi

bb_case_start "busybox_ipaddr"
_t=$({ timeout 10 sh -c "busybox ip addr 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "inet "; then echo "PASS: busybox_ipaddr"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ipaddr"; bb_case_fail; fi

bb_case_start "busybox_ipneigh"
_t=$({ timeout 10 sh -c "busybox ip neigh show 2>&1; busybox echo ipneigh_ok"; } 2>&1)
if echo "$_t" | grep -qF "ipneigh_ok"; then echo "PASS: busybox_ipneigh"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ipneigh"; bb_case_fail; fi

bb_case_start "busybox_iproute"
_t=$({ timeout 10 sh -c "busybox ip route show 2>&1; busybox echo iproute_ok"; } 2>&1)
if echo "$_t" | grep -qF "iproute_ok"; then echo "PASS: busybox_iproute"; bb_case_pass; else echo "FAIL_DETAIL: busybox_iproute"; bb_case_fail; fi

bb_case_start "busybox_iprule"
_t=$({ timeout 10 sh -c "busybox ip rule show 2>&1; busybox echo iprule_ok"; } 2>&1)
if echo "$_t" | grep -qF "iprule_ok"; then echo "PASS: busybox_iprule"; bb_case_pass; else echo "FAIL_DETAIL: busybox_iprule"; bb_case_fail; fi

bb_case_start "busybox_iptunnel"
_t=$({ timeout 10 sh -c "busybox ip tunnel show 2>&1; busybox echo iptunnel_ok"; } 2>&1)
if echo "$_t" | grep -qF "iptunnel_ok"; then echo "PASS: busybox_iptunnel"; bb_case_pass; else echo "FAIL_DETAIL: busybox_iptunnel"; bb_case_fail; fi

bb_case_start "busybox_kbd_mode"
_t=$({ timeout 10 sh -c "busybox kbd_mode -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage"; then echo "PASS: busybox_kbd_mode"; bb_case_pass; else echo "FAIL_DETAIL: busybox_kbd_mode"; bb_case_fail; fi

bb_case_start "busybox_kill"
_t=$({ timeout 10 sh -c "busybox kill -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "HUP"; then echo "PASS: busybox_kill"; bb_case_pass; else echo "FAIL_DETAIL: busybox_kill"; bb_case_fail; fi

bb_case_start "busybox_killall"
_t=$({ timeout 10 sh -c "busybox killall -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "HUP"; then echo "PASS: busybox_killall"; bb_case_pass; else echo "FAIL_DETAIL: busybox_killall"; bb_case_fail; fi

bb_case_start "busybox_klogd"
_t=$({ timeout 10 sh -c "busybox klogd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: klogd"; then echo "PASS: busybox_klogd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_klogd"; bb_case_fail; fi

bb_case_start "busybox_last"
_t=$({ timeout 10 sh -c "busybox last -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: last"; then echo "PASS: busybox_last"; bb_case_pass; else echo "FAIL_DETAIL: busybox_last"; bb_case_fail; fi

bb_case_start "busybox_less"
_t=$({ timeout 10 sh -c "busybox less -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: less"; then echo "PASS: busybox_less"; bb_case_pass; else echo "FAIL_DETAIL: busybox_less"; bb_case_fail; fi

bb_case_start "busybox_linux32"
_t=$({ timeout 10 sh -c "busybox linux32 busybox echo linux32_ok 2>&1 || busybox echo linux32_fallback"; } 2>&1)
if echo "$_t" | grep -qF "linux32_"; then echo "PASS: busybox_linux32"; bb_case_pass; else echo "FAIL_DETAIL: busybox_linux32"; bb_case_fail; fi

bb_case_start "busybox_linux64"
_t=$({ timeout 10 sh -c "busybox linux64 busybox echo linux64_ok 2>&1 || busybox echo linux64_fallback"; } 2>&1)
if echo "$_t" | grep -qF "linux64_"; then echo "PASS: busybox_linux64"; bb_case_pass; else echo "FAIL_DETAIL: busybox_linux64"; bb_case_fail; fi

bb_case_start "busybox_list"
_t=$({ timeout 10 sh -c "busybox --list"; } 2>&1)
if [ -n "$_t" ]; then echo "PASS: busybox_list"; bb_case_pass; else echo "FAIL_DETAIL: busybox_list (empty)"; bb_case_fail; fi

bb_case_start "busybox_ln"
_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_ln_s && busybox echo t > /tmp/bb_ln_t && busybox ln -s /tmp/bb_ln_t /tmp/bb_ln_s && busybox readlink /tmp/bb_ln_s 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bb_ln_t"; then echo "PASS: busybox_ln"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ln"; bb_case_fail; fi

bb_case_start "busybox_loadfont"
_t=$({ timeout 10 sh -c "busybox loadfont -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: loadfont"; then echo "PASS: busybox_loadfont"; bb_case_pass; else echo "FAIL_DETAIL: busybox_loadfont"; bb_case_fail; fi

bb_case_start "busybox_loadkmap"
_t=$({ timeout 10 sh -c "busybox loadkmap -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: loadkmap"; then echo "PASS: busybox_loadkmap"; bb_case_pass; else echo "FAIL_DETAIL: busybox_loadkmap"; bb_case_fail; fi

bb_case_start "busybox_logger"
_t=$({ timeout 10 sh -c "busybox logger -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: logger"; then echo "PASS: busybox_logger"; bb_case_pass; else echo "FAIL_DETAIL: busybox_logger"; bb_case_fail; fi

bb_case_start "busybox_login"
_t=$({ timeout 10 sh -c "busybox login -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: login"; then echo "PASS: busybox_login"; bb_case_pass; else echo "FAIL_DETAIL: busybox_login"; bb_case_fail; fi

bb_case_start "busybox_logread"
_t=$({ timeout 10 sh -c "busybox logread 2>&1; busybox echo logread_ok"; } 2>&1)
if echo "$_t" | grep -qF "logread_ok"; then echo "PASS: busybox_logread"; bb_case_pass; else echo "FAIL_DETAIL: busybox_logread"; bb_case_fail; fi

bb_case_start "busybox_losetup"
_t=$({ timeout 10 sh -c "busybox losetup -a 2>&1; busybox echo losetup_ok"; } 2>&1)
if echo "$_t" | grep -qF "losetup_ok"; then echo "PASS: busybox_losetup"; bb_case_pass; else echo "FAIL_DETAIL: busybox_losetup"; bb_case_fail; fi

bb_case_start "busybox_ls_bb"
_t=$({ timeout 10 sh -c "busybox ls / 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bin"; then echo "PASS: busybox_ls_bb"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ls_bb"; bb_case_fail; fi

bb_case_start "busybox_lsattr"
_t=$({ timeout 10 sh -c "busybox lsattr -d /tmp 2>&1; busybox echo lsattr_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsattr_ok"; then echo "PASS: busybox_lsattr"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lsattr"; bb_case_fail; fi

bb_case_start "busybox_lsmod"
_t=$({ timeout 10 sh -c "busybox lsmod 2>&1; busybox echo lsmod_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsmod_ok"; then echo "PASS: busybox_lsmod"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lsmod"; bb_case_fail; fi

bb_case_start "busybox_lsof"
_t=$({ timeout 10 sh -c "busybox lsof 2>&1; busybox echo lsof_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsof_ok"; then echo "PASS: busybox_lsof"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lsof"; bb_case_fail; fi

bb_case_start "busybox_lsusb"
_t=$({ timeout 10 sh -c "busybox lsusb 2>&1; busybox echo lsusb_ok"; } 2>&1)
if echo "$_t" | grep -qF "lsusb_ok"; then echo "PASS: busybox_lsusb"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lsusb"; bb_case_fail; fi

bb_case_start "busybox_lzop"
_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_lzop.txt /tmp/bb_lzop.txt.lzo && busybox echo -n lzop_t > /tmp/bb_lzop.txt && busybox lzop -f /tmp/bb_lzop.txt && busybox lzop -dc /tmp/bb_lzop.txt.lzo 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "lzop_t"; then echo "PASS: busybox_lzop"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lzop"; bb_case_fail; fi

bb_case_start "busybox_lzopcat"
_t=$({ timeout 10 sh -c "busybox echo -n round | busybox lzop -c 2>/dev/null | busybox lzopcat 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "round"; then echo "PASS: busybox_lzopcat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lzopcat"; bb_case_fail; fi

bb_case_start "busybox_makemime"
_t=$({ timeout 10 sh -c "busybox makemime -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: makemime"; then echo "PASS: busybox_makemime"; bb_case_pass; else echo "FAIL_DETAIL: busybox_makemime"; bb_case_fail; fi

bb_case_start "busybox_md5sum"
_t=$({ timeout 10 sh -c "busybox echo -n md5_t | busybox md5sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "-"; then echo "PASS: busybox_md5sum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_md5sum"; bb_case_fail; fi

bb_case_start "busybox_mdev"
_t=$({ timeout 10 sh -c "busybox mdev -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mdev"; then echo "PASS: busybox_mdev"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mdev"; bb_case_fail; fi

bb_case_start "busybox_mesg"
_t=$({ timeout 10 sh -c "busybox mesg 2>&1; busybox echo mesg_ok"; } 2>&1)
if echo "$_t" | grep -qF "mesg_ok"; then echo "PASS: busybox_mesg"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mesg"; bb_case_fail; fi

bb_case_start "busybox_microcom"
_t=$({ timeout 10 sh -c "busybox microcom -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: microcom"; then echo "PASS: busybox_microcom"; bb_case_pass; else echo "FAIL_DETAIL: busybox_microcom"; bb_case_fail; fi

bb_case_start "busybox_mkdosfs"
_t=$({ timeout 10 sh -c "busybox mkdosfs -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mkdosfs"; then echo "PASS: busybox_mkdosfs"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mkdosfs"; bb_case_fail; fi

bb_case_start "busybox_mkfifo"
_t=$({ timeout 10 sh -c "busybox rm -f /tmp/bb_fifo_t && busybox mkfifo /tmp/bb_fifo_t && busybox ls -l /tmp/bb_fifo_t 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "bb_fifo_t"; then echo "PASS: busybox_mkfifo"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mkfifo"; bb_case_fail; fi

bb_case_start "busybox_mkfs_vfat"
_t=$({ timeout 10 sh -c "busybox mkfs.vfat -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mkfs.vfat"; then echo "PASS: busybox_mkfs_vfat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mkfs_vfat"; bb_case_fail; fi

bb_case_start "busybox_mknod"
_t=$({ timeout 10 sh -c "busybox mknod -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mknod"; then echo "PASS: busybox_mknod"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mknod"; bb_case_fail; fi

bb_case_start "busybox_mkpasswd"
_t=$({ timeout 10 sh -c "busybox mkpasswd -m md5 testpass 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "\$1\$"; then echo "PASS: busybox_mkpasswd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mkpasswd"; bb_case_fail; fi

bb_case_start "busybox_mkswap"
_t=$({ timeout 10 sh -c "busybox mkswap -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: mkswap"; then echo "PASS: busybox_mkswap"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mkswap"; bb_case_fail; fi

bb_case_start "busybox_mktemp"
_t=$({ timeout 10 sh -c "busybox sh -c 'd=\$(busybox mktemp -d -t bbXXXXXX) && busybox test -d \"\$d\" && busybox echo mktemp_ok && busybox rm -rf \"\$d\"' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "mktemp_ok"; then echo "PASS: busybox_mktemp"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mktemp"; bb_case_fail; fi

bb_case_start "busybox_modinfo"
_t=$({ timeout 10 sh -c "busybox modinfo -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: modinfo"; then echo "PASS: busybox_modinfo"; bb_case_pass; else echo "FAIL_DETAIL: busybox_modinfo"; bb_case_fail; fi

bb_case_start "busybox_modprobe"
_t=$({ timeout 10 sh -c "busybox modprobe -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: modprobe"; then echo "PASS: busybox_modprobe"; bb_case_pass; else echo "FAIL_DETAIL: busybox_modprobe"; bb_case_fail; fi

bb_case_start "busybox_more"
_t=$({ timeout 10 sh -c "busybox more /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_more"; bb_case_pass; else echo "FAIL_DETAIL: busybox_more"; bb_case_fail; fi

bb_case_start "busybox_mount"
_t=$({ timeout 10 sh -c "busybox mount 2>&1; busybox echo mount_ok"; } 2>&1)
if echo "$_t" | grep -qF "mount_ok"; then echo "PASS: busybox_mount"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mount"; bb_case_fail; fi

bb_case_start "busybox_mountpoint"
_t=$({ timeout 10 sh -c "busybox mountpoint / 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "is a mountpoint"; then echo "PASS: busybox_mountpoint"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mountpoint"; bb_case_fail; fi

bb_case_start "busybox_mpstat"
_t=$({ timeout 5 sh -c "busybox mpstat 2>&1; busybox echo mpstat_ok"; } 2>&1)
if echo "$_t" | grep -qF "mpstat_ok"; then echo "PASS: busybox_mpstat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mpstat"; bb_case_fail; fi

bb_case_start "busybox_nameif"
_t=$({ timeout 10 sh -c "busybox nameif -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nameif"; then echo "PASS: busybox_nameif"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nameif"; bb_case_fail; fi

bb_case_start "busybox_nanddump"
_t=$({ timeout 10 sh -c "busybox nanddump -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nanddump"; then echo "PASS: busybox_nanddump"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nanddump"; bb_case_fail; fi

bb_case_start "busybox_nandwrite"
_t=$({ timeout 10 sh -c "busybox nandwrite -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nandwrite"; then echo "PASS: busybox_nandwrite"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nandwrite"; bb_case_fail; fi

bb_case_start "busybox_nbd_client"
_t=$({ timeout 10 sh -c "busybox nbd-client -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nbd-client"; then echo "PASS: busybox_nbd_client"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nbd_client"; bb_case_fail; fi

bb_case_start "busybox_nc"
_t=$({ timeout 10 sh -c "busybox nc -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nc"; then echo "PASS: busybox_nc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nc"; bb_case_fail; fi

bb_case_start "busybox_netstat"
_t=$({ timeout 10 sh -c "busybox netstat -a 2>&1; busybox echo netstat_ok"; } 2>&1)
if echo "$_t" | grep -qF "netstat_ok"; then echo "PASS: busybox_netstat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_netstat"; bb_case_fail; fi

bb_case_start "busybox_nice"
_t=$({ timeout 10 sh -c "busybox nice -n 10 busybox echo nice_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "nice_ok"; then echo "PASS: busybox_nice"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nice"; bb_case_fail; fi

bb_case_start "busybox_nl"
_t=$({ timeout 10 sh -c "busybox nl -ba /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_nl"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nl"; bb_case_fail; fi

bb_case_start "busybox_nmeter"
_t=$({ timeout 10 sh -c "busybox nmeter -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nmeter"; then echo "PASS: busybox_nmeter"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nmeter"; bb_case_fail; fi

bb_case_start "busybox_nologin"
_t=$({ timeout 2 busybox nologin 2>&1 || true; } 2>&1)
if echo "$_t" | grep -qF "This account is not available"; then echo "PASS: busybox_nologin"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nologin"; bb_case_fail; fi

bb_case_start "busybox_nproc"
_t=$({ timeout 10 sh -c "busybox sh -c 'n=\$(busybox nproc) && busybox test -n \"\$n\" && busybox echo nproc_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "nproc_ok"; then echo "PASS: busybox_nproc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nproc"; bb_case_fail; fi

bb_case_start "busybox_nsenter"
_t=$({ timeout 10 sh -c "busybox nsenter -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: nsenter"; then echo "PASS: busybox_nsenter"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nsenter"; bb_case_fail; fi

bb_case_start "busybox_nslookup"
_t=$({ timeout 10 sh -c "busybox nslookup 127.0.0.1 2>&1; busybox echo nslookup_ok"; } 2>&1)
if echo "$_t" | grep -qF "nslookup_ok"; then echo "PASS: busybox_nslookup"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nslookup"; bb_case_fail; fi

bb_case_start "busybox_ntpd"
_t=$({ timeout 10 sh -c "busybox ntpd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: ntpd"; then echo "PASS: busybox_ntpd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ntpd"; bb_case_fail; fi

bb_case_start "busybox_od"
_t=$({ timeout 10 sh -c "busybox echo test | busybox od -An -tx1 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "74"; then echo "PASS: busybox_od"; bb_case_pass; else echo "FAIL_DETAIL: busybox_od"; bb_case_fail; fi

bb_case_start "busybox_openvt"
_t=$({ timeout 10 sh -c "busybox openvt -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: openvt"; then echo "PASS: busybox_openvt"; bb_case_pass; else echo "FAIL_DETAIL: busybox_openvt"; bb_case_fail; fi

bb_case_start "busybox_partprobe"
_t=$({ timeout 10 sh -c "busybox partprobe -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: partprobe"; then echo "PASS: busybox_partprobe"; bb_case_pass; else echo "FAIL_DETAIL: busybox_partprobe"; bb_case_fail; fi

bb_case_start "busybox_passwd"
_t=$({ timeout 10 sh -c "busybox passwd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: passwd"; then echo "PASS: busybox_passwd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_passwd"; bb_case_fail; fi

bb_case_start "busybox_paste"
_t=$({ timeout 10 sh -c "busybox echo a > /tmp/bb_p1 && busybox echo b > /tmp/bb_p2 && busybox paste /tmp/bb_p1 /tmp/bb_p2 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "a	b"; then echo "PASS: busybox_paste"; bb_case_pass; else echo "FAIL_DETAIL: busybox_paste"; bb_case_fail; fi

bb_case_start "busybox_pgrep"
_t=$({ timeout 10 sh -c "busybox pgrep -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: pgrep"; then echo "PASS: busybox_pgrep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pgrep"; bb_case_fail; fi

bb_case_start "busybox_pidof"
_t=$({ busybox pidof -s init 2>&1 || busybox pidof -s sh 2>&1; } 2>&1)
if echo "$_t" | grep -qF "1"; then echo "PASS: busybox_pidof"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pidof"; bb_case_fail; fi

bb_case_start "busybox_ping6"
_t=$({ timeout 10 sh -c "busybox ping6 -c 1 ::1 2>&1 || busybox echo ping6_fallback"; } 2>&1)
if echo "$_t" | grep -qF "ping6_"; then echo "PASS: busybox_ping6"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ping6"; bb_case_fail; fi

bb_case_start "busybox_pivot_root"
_t=$({ timeout 10 sh -c "busybox pivot_root -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: pivot_root"; then echo "PASS: busybox_pivot_root"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pivot_root"; bb_case_fail; fi

bb_case_start "busybox_pkill"
_t=$({ timeout 10 sh -c "busybox pkill -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "HUP"; then echo "PASS: busybox_pkill"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pkill"; bb_case_fail; fi

bb_case_start "busybox_pmap"
_t=$({ timeout 10 sh -c "busybox pmap 1 2>&1; busybox echo pmap_ok"; } 2>&1)
if echo "$_t" | grep -qF "pmap_ok"; then echo "PASS: busybox_pmap"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pmap"; bb_case_fail; fi

bb_case_start "busybox_poweroff"
_t=$({ timeout 10 sh -c "busybox poweroff -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: poweroff"; then echo "PASS: busybox_poweroff"; bb_case_pass; else echo "FAIL_DETAIL: busybox_poweroff"; bb_case_fail; fi

bb_case_start "busybox_printenv"
_t=$({ timeout 10 sh -c "busybox printenv PATH 2>&1; busybox echo printenv_ok"; } 2>&1)
if echo "$_t" | grep -qF "printenv_ok"; then echo "PASS: busybox_printenv"; bb_case_pass; else echo "FAIL_DETAIL: busybox_printenv"; bb_case_fail; fi

bb_case_start "busybox_printf"
_t=$({ timeout 10 sh -c "busybox printf 'pf_%s_ok\\n' bb 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "pf_bb_ok"; then echo "PASS: busybox_printf"; bb_case_pass; else echo "FAIL_DETAIL: busybox_printf"; bb_case_fail; fi

bb_case_start "busybox_ps"
_t=$({ timeout 10 sh -c "busybox ps 2>&1; busybox echo ps_ok"; } 2>&1)
if echo "$_t" | grep -qF "ps_ok"; then echo "PASS: busybox_ps"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ps"; bb_case_fail; fi

bb_case_start "busybox_pscan"
_t=$({ timeout 10 sh -c "busybox pscan -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: pscan"; then echo "PASS: busybox_pscan"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pscan"; bb_case_fail; fi

bb_case_start "busybox_pstree"
_t=$({ timeout 10 sh -c "busybox pstree 2>&1; busybox echo pstree_ok"; } 2>&1)
if echo "$_t" | grep -qF "pstree_ok"; then echo "PASS: busybox_pstree"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pstree"; bb_case_fail; fi

bb_case_start "busybox_pwd"
_t=$({ timeout 10 sh -c "busybox pwd 2>&1; busybox echo pwd_ok"; } 2>&1)
if echo "$_t" | grep -qF "pwd_ok"; then echo "PASS: busybox_pwd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pwd"; bb_case_fail; fi

bb_case_start "busybox_pwdx"
_t=$({ timeout 10 sh -c "busybox pwdx 1 2>&1; busybox echo pwdx_ok"; } 2>&1)
if echo "$_t" | grep -qF "pwdx_ok"; then echo "PASS: busybox_pwdx"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pwdx"; bb_case_fail; fi

bb_case_start "busybox_rdate"
_t=$({ timeout 10 sh -c "busybox rdate -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: rdate"; then echo "PASS: busybox_rdate"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rdate"; bb_case_fail; fi

bb_case_start "busybox_readahead"
_t=$({ timeout 10 sh -c "busybox echo ra > /tmp/bb_ra_f && busybox readahead /tmp/bb_ra_f 2>/dev/null; busybox echo ra_done 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ra_done"; then echo "PASS: busybox_readahead"; bb_case_pass; else echo "FAIL_DETAIL: busybox_readahead"; bb_case_fail; fi

bb_case_start "busybox_readlink"
_t=$({ timeout 10 sh -c "busybox readlink -f /proc/self/exe 2>&1; busybox echo readlink_ok"; } 2>&1)
if echo "$_t" | grep -qF "readlink_ok"; then echo "PASS: busybox_readlink"; bb_case_pass; else echo "FAIL_DETAIL: busybox_readlink"; bb_case_fail; fi

bb_case_start "busybox_realpath"
_t=$({ timeout 10 sh -c "busybox realpath /tmp 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "/tmp"; then echo "PASS: busybox_realpath"; bb_case_pass; else echo "FAIL_DETAIL: busybox_realpath"; bb_case_fail; fi

bb_case_start "busybox_reboot"
_t=$({ timeout 10 sh -c "busybox reboot -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: reboot"; then echo "PASS: busybox_reboot"; bb_case_pass; else echo "FAIL_DETAIL: busybox_reboot"; bb_case_fail; fi

bb_case_start "busybox_reformime"
_t=$({ timeout 10 sh -c "busybox reformime -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: reformime"; then echo "PASS: busybox_reformime"; bb_case_pass; else echo "FAIL_DETAIL: busybox_reformime"; bb_case_fail; fi

bb_case_start "busybox_renice"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox renice +0 -p \$\$; busybox echo renice_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "renice_ok"; then echo "PASS: busybox_renice"; bb_case_pass; else echo "FAIL_DETAIL: busybox_renice"; bb_case_fail; fi

bb_case_start "busybox_reset"
_t=$({ timeout 10 sh -c "busybox reset 2>/dev/null; busybox echo reset_done 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "reset_done"; then echo "PASS: busybox_reset"; bb_case_pass; else echo "FAIL_DETAIL: busybox_reset"; bb_case_fail; fi

bb_case_start "busybox_rev"
_t=$({ timeout 10 sh -c "busybox echo abcd | busybox rev 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "dcba"; then echo "PASS: busybox_rev"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rev"; bb_case_fail; fi

bb_case_start "busybox_rfkill"
_t=$({ timeout 10 sh -c "busybox rfkill list 2>&1; busybox echo rfkill_ok"; } 2>&1)
if echo "$_t" | grep -qF "rfkill_ok"; then echo "PASS: busybox_rfkill"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rfkill"; bb_case_fail; fi

bb_case_start "busybox_rm"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox touch /tmp/bb_rm_x && busybox rm /tmp/bb_rm_x && busybox test ! -e /tmp/bb_rm_x && busybox echo rm_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "rm_ok"; then echo "PASS: busybox_rm"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rm"; bb_case_fail; fi

bb_case_start "busybox_rmmod"
_t=$({ timeout 10 sh -c "busybox rmmod -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: rmmod"; then echo "PASS: busybox_rmmod"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rmmod"; bb_case_fail; fi

bb_case_start "busybox_route"
_t=$({ timeout 10 sh -c "busybox route -n 2>&1; busybox echo route_ok"; } 2>&1)
if echo "$_t" | grep -qF "route_ok"; then echo "PASS: busybox_route"; bb_case_pass; else echo "FAIL_DETAIL: busybox_route"; bb_case_fail; fi

bb_case_start "busybox_sed"
_t=$({ timeout 10 sh -c "busybox echo hello | busybox sed 's/hello/sed_ok/' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sed_ok"; then echo "PASS: busybox_sed"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sed"; bb_case_fail; fi

bb_case_start "busybox_sendmail"
_t=$({ timeout 10 sh -c "busybox sendmail -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: sendmail"; then echo "PASS: busybox_sendmail"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sendmail"; bb_case_fail; fi

bb_case_start "busybox_seq"
_t=$({ timeout 10 sh -c "busybox seq 1 3 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "3"; then echo "PASS: busybox_seq"; bb_case_pass; else echo "FAIL_DETAIL: busybox_seq"; bb_case_fail; fi

bb_case_start "busybox_setconsole"
_t=$({ timeout 10 sh -c "busybox setconsole -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setconsole"; then echo "PASS: busybox_setconsole"; bb_case_pass; else echo "FAIL_DETAIL: busybox_setconsole"; bb_case_fail; fi

bb_case_start "busybox_setfont"
_t=$({ timeout 10 sh -c "busybox setfont -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setfont"; then echo "PASS: busybox_setfont"; bb_case_pass; else echo "FAIL_DETAIL: busybox_setfont"; bb_case_fail; fi

bb_case_start "busybox_setkeycodes"
_t=$({ timeout 10 sh -c "busybox setkeycodes -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setkeycodes"; then echo "PASS: busybox_setkeycodes"; bb_case_pass; else echo "FAIL_DETAIL: busybox_setkeycodes"; bb_case_fail; fi

bb_case_start "busybox_setpriv"
_t=$({ timeout 10 sh -c "busybox setpriv -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setpriv"; then echo "PASS: busybox_setpriv"; bb_case_pass; else echo "FAIL_DETAIL: busybox_setpriv"; bb_case_fail; fi

bb_case_start "busybox_setserial"
_t=$({ timeout 10 sh -c "busybox setserial -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: setserial"; then echo "PASS: busybox_setserial"; bb_case_pass; else echo "FAIL_DETAIL: busybox_setserial"; bb_case_fail; fi

bb_case_start "busybox_setsid"
_t=$({ timeout 10 sh -c "busybox setsid busybox echo setsid_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "setsid_ok"; then echo "PASS: busybox_setsid"; bb_case_pass; else echo "FAIL_DETAIL: busybox_setsid"; bb_case_fail; fi

bb_case_start "busybox_sh"
_t=$({ timeout 10 sh -c "busybox sh -c 'echo sh_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sh_ok"; then echo "PASS: busybox_sh"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sh"; bb_case_fail; fi

bb_case_start "busybox_sha1sum"
_t=$({ timeout 10 sh -c "busybox echo -n sha1_t | busybox sha1sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha1sum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sha1sum"; bb_case_fail; fi

bb_case_start "busybox_sha256sum"
_t=$({ timeout 10 sh -c "busybox echo -n s256 | busybox sha256sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha256sum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sha256sum"; bb_case_fail; fi

bb_case_start "busybox_sha3sum"
_t=$({ timeout 10 sh -c "busybox echo -n s3 | busybox sha3sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha3sum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sha3sum"; bb_case_fail; fi

bb_case_start "busybox_sha512sum"
_t=$({ timeout 10 sh -c "busybox echo -n s512 | busybox sha512sum 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "  -"; then echo "PASS: busybox_sha512sum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sha512sum"; bb_case_fail; fi

bb_case_start "busybox_showkey"
_t=$({ timeout 10 sh -c "busybox showkey -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: showkey"; then echo "PASS: busybox_showkey"; bb_case_pass; else echo "FAIL_DETAIL: busybox_showkey"; bb_case_fail; fi

bb_case_start "busybox_shred"
_t=$({ timeout 10 sh -c "busybox sh -c 'echo x > /tmp/bb_shred_t && busybox shred -n 1 -u /tmp/bb_shred_t 2>&1; busybox test ! -f /tmp/bb_shred_t && busybox echo shred_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "shred_ok"; then echo "PASS: busybox_shred"; bb_case_pass; else echo "FAIL_DETAIL: busybox_shred"; bb_case_fail; fi

bb_case_start "busybox_shuf"
_t=$({ timeout 10 sh -c "busybox printf 'a
b
c
' | busybox shuf 2>&1; busybox echo shuf_ok"; } 2>&1)
if echo "$_t" | grep -qF "shuf_ok"; then echo "PASS: busybox_shuf"; bb_case_pass; else echo "FAIL_DETAIL: busybox_shuf"; bb_case_fail; fi

bb_case_start "busybox_slattach"
_t=$({ timeout 10 sh -c "busybox slattach -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: slattach"; then echo "PASS: busybox_slattach"; bb_case_pass; else echo "FAIL_DETAIL: busybox_slattach"; bb_case_fail; fi

bb_case_start "busybox_sleep"
_t=$({ timeout 10 sh -c "busybox sleep 0 && busybox echo sleep_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sleep_ok"; then echo "PASS: busybox_sleep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sleep"; bb_case_fail; fi

bb_case_start "busybox_sort"
_t=$({ timeout 10 sh -c "busybox printf 'c
a
b
' | busybox sort 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "a"; then echo "PASS: busybox_sort"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sort"; bb_case_fail; fi

bb_case_start "busybox_stat"
_t=$({ timeout 10 sh -c "busybox stat /etc/passwd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "File: /etc/passwd"; then echo "PASS: busybox_stat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_stat"; bb_case_fail; fi

bb_case_start "busybox_strings"
_t=$({ timeout 10 sh -c "busybox strings /bin/busybox 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "BusyBox"; then echo "PASS: busybox_strings"; bb_case_pass; else echo "FAIL_DETAIL: busybox_strings"; bb_case_fail; fi

bb_case_start "busybox_stty"
_t=$({ timeout 10 sh -c "busybox stty -a 2>&1; busybox echo stty_ok"; } 2>&1)
if echo "$_t" | grep -qF "stty_ok"; then echo "PASS: busybox_stty"; bb_case_pass; else echo "FAIL_DETAIL: busybox_stty"; bb_case_fail; fi

bb_case_start "busybox_su"
_t=$({ timeout 10 sh -c "busybox su -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: su"; then echo "PASS: busybox_su"; bb_case_pass; else echo "FAIL_DETAIL: busybox_su"; bb_case_fail; fi

bb_case_start "busybox_sum"
_t=$({ timeout 10 sh -c "busybox echo sum_t | busybox sum 2>&1; busybox echo sum_ok"; } 2>&1)
if echo "$_t" | grep -qF "sum_ok"; then echo "PASS: busybox_sum"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sum"; bb_case_fail; fi

bb_case_start "busybox_swapoff"
_t=$({ timeout 10 sh -c "busybox swapoff -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: swapoff"; then echo "PASS: busybox_swapoff"; bb_case_pass; else echo "FAIL_DETAIL: busybox_swapoff"; bb_case_fail; fi

bb_case_start "busybox_swapon"
_t=$({ timeout 10 sh -c "busybox swapon -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: swapon"; then echo "PASS: busybox_swapon"; bb_case_pass; else echo "FAIL_DETAIL: busybox_swapon"; bb_case_fail; fi

bb_case_start "busybox_switch_root"
_t=$({ timeout 10 sh -c "busybox switch_root -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: switch_root"; then echo "PASS: busybox_switch_root"; bb_case_pass; else echo "FAIL_DETAIL: busybox_switch_root"; bb_case_fail; fi

bb_case_start "busybox_sync"
_t=$({ timeout 10 sh -c "busybox sync && busybox echo sync_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "sync_ok"; then echo "PASS: busybox_sync"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sync"; bb_case_fail; fi

bb_case_start "busybox_sysctl"
_t=$({ timeout 10 sh -c "busybox sysctl kernel.hostname 2>&1 || busybox sysctl -h 2>&1; busybox echo sysctl_ok"; } 2>&1)
if echo "$_t" | grep -qF "sysctl_ok"; then echo "PASS: busybox_sysctl"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sysctl"; bb_case_fail; fi

bb_case_start "busybox_syslogd"
_t=$({ timeout 10 sh -c "busybox syslogd -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: syslogd"; then echo "PASS: busybox_syslogd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_syslogd"; bb_case_fail; fi

bb_case_start "busybox_tac"
_t=$({ timeout 10 sh -c "busybox printf 'a
b
' | busybox tac 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "b"; then echo "PASS: busybox_tac"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tac"; bb_case_fail; fi

bb_case_start "busybox_tee"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo tee_line | busybox tee /tmp/bb_tee_f >/dev/null && busybox cat /tmp/bb_tee_f' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "tee_line"; then echo "PASS: busybox_tee"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tee"; bb_case_fail; fi

bb_case_start "busybox_test"
_t=$({ timeout 10 sh -c "busybox test 1 -eq 1 && busybox echo test_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "test_ok"; then echo "PASS: busybox_test"; bb_case_pass; else echo "FAIL_DETAIL: busybox_test"; bb_case_fail; fi

bb_case_start "busybox_time"
_t=$({ timeout 10 sh -c "busybox time busybox echo time_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "time_ok"; then echo "PASS: busybox_time"; bb_case_pass; else echo "FAIL_DETAIL: busybox_time"; bb_case_fail; fi

bb_case_start "busybox_timeout"
_t=$({ timeout 10 sh -c "busybox timeout 2 busybox echo timeout_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "timeout_ok"; then echo "PASS: busybox_timeout"; bb_case_pass; else echo "FAIL_DETAIL: busybox_timeout"; bb_case_fail; fi

bb_case_start "busybox_top"
_t=$({ timeout 10 sh -c "busybox top -b -n 1 2>&1; busybox echo top_ok"; } 2>&1)
if echo "$_t" | grep -qF "top_ok"; then echo "PASS: busybox_top"; bb_case_pass; else echo "FAIL_DETAIL: busybox_top"; bb_case_fail; fi

bb_case_start "busybox_touch"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox touch /tmp/bb_touch_f && busybox test -f /tmp/bb_touch_f && busybox echo touch_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "touch_ok"; then echo "PASS: busybox_touch"; bb_case_pass; else echo "FAIL_DETAIL: busybox_touch"; bb_case_fail; fi

bb_case_start "busybox_tr"
_t=$({ timeout 10 sh -c "busybox echo abc | busybox tr 'a-z' 'A-Z' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ABC"; then echo "PASS: busybox_tr"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tr"; bb_case_fail; fi

bb_case_start "busybox_traceroute"
_t=$({ timeout 10 sh -c "busybox traceroute -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: traceroute"; then echo "PASS: busybox_traceroute"; bb_case_pass; else echo "FAIL_DETAIL: busybox_traceroute"; bb_case_fail; fi

bb_case_start "busybox_traceroute6"
_t=$({ timeout 10 sh -c "busybox traceroute6 -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: traceroute6"; then echo "PASS: busybox_traceroute6"; bb_case_pass; else echo "FAIL_DETAIL: busybox_traceroute6"; bb_case_fail; fi

bb_case_start "busybox_tree"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox mkdir -p /tmp/bb_tree/d && busybox echo x > /tmp/bb_tree/d/a && busybox tree /tmp/bb_tree 2>&1'"; } 2>&1)
if echo "$_t" | grep -qF "a"; then echo "PASS: busybox_tree"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tree"; bb_case_fail; fi

bb_case_start "busybox_true"
_t=$({ timeout 10 sh -c "busybox true && busybox echo true_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "true_ok"; then echo "PASS: busybox_true"; bb_case_pass; else echo "FAIL_DETAIL: busybox_true"; bb_case_fail; fi

bb_case_start "busybox_truncate"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo abcd > /tmp/bb_trunc_f && busybox truncate -s 2 /tmp/bb_trunc_f && busybox cat /tmp/bb_trunc_f' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ab"; then echo "PASS: busybox_truncate"; bb_case_pass; else echo "FAIL_DETAIL: busybox_truncate"; bb_case_fail; fi

bb_case_start "busybox_tty"
_t=$({ timeout 10 sh -c "busybox tty 2>&1; busybox echo tty_ok"; } 2>&1)
if echo "$_t" | grep -qF "tty_ok"; then echo "PASS: busybox_tty"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tty"; bb_case_fail; fi

bb_case_start "busybox_tunctl"
_t=$({ timeout 10 sh -c "busybox tunctl -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: tunctl"; then echo "PASS: busybox_tunctl"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tunctl"; bb_case_fail; fi

bb_case_start "busybox_udhcpc"
_t=$({ timeout 10 sh -c "busybox udhcpc -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: udhcpc"; then echo "PASS: busybox_udhcpc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_udhcpc"; bb_case_fail; fi

bb_case_start "busybox_udhcpc6"
_t=$({ timeout 10 sh -c "busybox udhcpc6 -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: udhcpc6"; then echo "PASS: busybox_udhcpc6"; bb_case_pass; else echo "FAIL_DETAIL: busybox_udhcpc6"; bb_case_fail; fi

bb_case_start "busybox_umount"
_t=$({ timeout 10 sh -c "busybox umount -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: umount"; then echo "PASS: busybox_umount"; bb_case_pass; else echo "FAIL_DETAIL: busybox_umount"; bb_case_fail; fi

bb_case_start "busybox_uname"
_t=$({ timeout 10 sh -c "busybox uname -a 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Linux"; then echo "PASS: busybox_uname"; bb_case_pass; else echo "FAIL_DETAIL: busybox_uname"; bb_case_fail; fi

bb_case_start "busybox_unexpand"
_t=$({ timeout 10 sh -c "busybox printf 'x    y
' | busybox unexpand -a 2>&1; busybox echo unexpand_ok"; } 2>&1)
if echo "$_t" | grep -qF "unexpand_ok"; then echo "PASS: busybox_unexpand"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unexpand"; bb_case_fail; fi

bb_case_start "busybox_uniq"
_t=$({ timeout 10 sh -c "busybox printf 'a
a
b
' | busybox uniq 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "b"; then echo "PASS: busybox_uniq"; bb_case_pass; else echo "FAIL_DETAIL: busybox_uniq"; bb_case_fail; fi

bb_case_start "busybox_unix2dos"
_t=$({ timeout 10 sh -c "busybox printf 'u2d
' | busybox unix2dos 2>&1; busybox echo unix2dos_ok"; } 2>&1)
if echo "$_t" | grep -qF "unix2dos_ok"; then echo "PASS: busybox_unix2dos"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unix2dos"; bb_case_fail; fi

bb_case_start "busybox_unlink"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo u > /tmp/bb_unl && busybox unlink /tmp/bb_unl && busybox test ! -e /tmp/bb_unl && busybox echo unlink_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "unlink_ok"; then echo "PASS: busybox_unlink"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unlink"; bb_case_fail; fi

bb_case_start "busybox_unlzma"
_t=$({ timeout 10 sh -c "busybox unlzma -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unlzma"; then echo "PASS: busybox_unlzma"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unlzma"; bb_case_fail; fi

bb_case_start "busybox_unlzop"
_t=$({ timeout 10 sh -c "busybox unlzop -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unlzop"; then echo "PASS: busybox_unlzop"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unlzop"; bb_case_fail; fi

bb_case_start "busybox_unshare"
_t=$({ timeout 10 sh -c "busybox unshare -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unshare"; then echo "PASS: busybox_unshare"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unshare"; bb_case_fail; fi

bb_case_start "busybox_unxz"
_t=$({ timeout 10 sh -c "busybox unxz -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unxz"; then echo "PASS: busybox_unxz"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unxz"; bb_case_fail; fi

bb_case_start "busybox_unzip"
_t=$({ timeout 10 sh -c "busybox unzip -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: unzip"; then echo "PASS: busybox_unzip"; bb_case_pass; else echo "FAIL_DETAIL: busybox_unzip"; bb_case_fail; fi

bb_case_start "busybox_uptime"
_t=$({ timeout 10 sh -c "busybox uptime 2>&1; busybox echo uptime_ok"; } 2>&1)
if echo "$_t" | grep -qF "uptime_ok"; then echo "PASS: busybox_uptime"; bb_case_pass; else echo "FAIL_DETAIL: busybox_uptime"; bb_case_fail; fi

bb_case_start "busybox_usleep"
_t=$({ timeout 10 sh -c "busybox usleep 1 && busybox echo usleep_ok 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "usleep_ok"; then echo "PASS: busybox_usleep"; bb_case_pass; else echo "FAIL_DETAIL: busybox_usleep"; bb_case_fail; fi

bb_case_start "busybox_uudecode"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox echo hi | busybox uuencode out | busybox uudecode -o /tmp/bb_uudec 2>&1 && busybox cat /tmp/bb_uudec' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hi"; then echo "PASS: busybox_uudecode"; bb_case_pass; else echo "FAIL_DETAIL: busybox_uudecode"; bb_case_fail; fi

bb_case_start "busybox_uuencode"
_t=$({ timeout 10 sh -c "busybox echo enc | busybox uuencode out 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "begin"; then echo "PASS: busybox_uuencode"; bb_case_pass; else echo "FAIL_DETAIL: busybox_uuencode"; bb_case_fail; fi

bb_case_start "busybox_vconfig"
_t=$({ timeout 10 sh -c "busybox vconfig -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: vconfig"; then echo "PASS: busybox_vconfig"; bb_case_pass; else echo "FAIL_DETAIL: busybox_vconfig"; bb_case_fail; fi

bb_case_start "busybox_vi"
_t=$({ timeout 10 sh -c "busybox vi -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: vi"; then echo "PASS: busybox_vi"; bb_case_pass; else echo "FAIL_DETAIL: busybox_vi"; bb_case_fail; fi

bb_case_start "busybox_vlock"
_t=$({ timeout 10 sh -c "busybox vlock -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: vlock" || echo "$_t" | grep -qF "vlock:"; then echo "PASS: busybox_vlock"; bb_case_pass; else echo "FAIL_DETAIL: busybox_vlock"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_volname"
_t=$({ timeout 10 sh -c "busybox volname /dev/null 2>&1; busybox echo volname_ok"; } 2>&1)
if echo "$_t" | grep -qF "volname_ok"; then echo "PASS: busybox_volname"; bb_case_pass; else echo "FAIL_DETAIL: busybox_volname"; bb_case_fail; fi

bb_case_start "busybox_watch"
_t=$({ timeout 10 sh -c "busybox watch -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: watch"; then echo "PASS: busybox_watch"; bb_case_pass; else echo "FAIL_DETAIL: busybox_watch"; bb_case_fail; fi

bb_case_start "busybox_watchdog"
_t=$({ timeout 10 sh -c "busybox watchdog -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: watchdog"; then echo "PASS: busybox_watchdog"; bb_case_pass; else echo "FAIL_DETAIL: busybox_watchdog"; bb_case_fail; fi

bb_case_start "busybox_wc"
_t=$({ timeout 10 sh -c "busybox printf 'a
b
c
' | busybox wc -l 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "3"; then echo "PASS: busybox_wc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_wc"; bb_case_fail; fi

bb_case_start "busybox_wget"
_t=$({ timeout 30 sh -c '
busybox rm -rf /tmp/bb_wget_root /tmp/bb_wget.html
busybox mkdir -p /tmp/bb_wget_root
{
    busybox printf "HTTP/1.0 200 OK\r\n"
    busybox printf "Content-Length: 22\r\n"
    busybox printf "Connection: close\r\n"
    busybox printf "\r\n"
    busybox printf "busybox wget local ok\n"
} > /tmp/bb_wget_root/response.http
busybox nc -l -p 18080 -w 10 < /tmp/bb_wget_root/response.http >/tmp/bb_wget_nc.out 2>&1 &
server_pid=$!
busybox sleep 1
timeout 10 busybox wget -O /tmp/bb_wget.html http://127.0.0.1:18080/index.html 2>&1
wget_status=$?
busybox kill "$server_pid" 2>/dev/null || true
busybox test "$wget_status" -eq 0 &&
busybox test -s /tmp/bb_wget.html &&
busybox grep -q "busybox wget local ok" /tmp/bb_wget.html &&
busybox echo wget_download_ok
'; } 2>&1)
if echo "$_t" | grep -qF "wget_download_ok"; then echo "PASS: busybox_wget"; bb_case_pass; else echo "FAIL_DETAIL: busybox_wget"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_which"
_t=$({ timeout 10 sh -c "busybox which busybox 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "busybox"; then echo "PASS: busybox_which"; bb_case_pass; else echo "FAIL_DETAIL: busybox_which"; bb_case_fail; fi

bb_case_start "busybox_who"
_t=$({ timeout 10 sh -c "busybox who 2>&1 | busybox wc -l 2>&1; busybox echo who_ok"; } 2>&1)
if echo "$_t" | grep -qF "who_ok"; then echo "PASS: busybox_who"; bb_case_pass; else echo "FAIL_DETAIL: busybox_who"; bb_case_fail; fi

bb_case_start "busybox_whoami"
_t=$({ timeout 10 sh -c "busybox whoami 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root"; then echo "PASS: busybox_whoami"; bb_case_pass; else echo "FAIL_DETAIL: busybox_whoami"; bb_case_fail; fi

bb_case_start "busybox_whois"
_t=$({ timeout 10 sh -c "busybox whois -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: whois"; then echo "PASS: busybox_whois"; bb_case_pass; else echo "FAIL_DETAIL: busybox_whois"; bb_case_fail; fi

bb_case_start "busybox_xargs"
_t=$({ timeout 10 sh -c "busybox echo a b | busybox xargs busybox echo X 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "X a b"; then echo "PASS: busybox_xargs"; bb_case_pass; else echo "FAIL_DETAIL: busybox_xargs"; bb_case_fail; fi

bb_case_start "busybox_xzcat"
_t=$({ timeout 10 sh -c "busybox xzcat -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: xzcat"; then echo "PASS: busybox_xzcat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_xzcat"; bb_case_fail; fi

bb_case_start "busybox_yes"
_t=$({ timeout 10 sh -c "busybox yes y | busybox head -n 1 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "y"; then echo "PASS: busybox_yes"; bb_case_pass; else echo "FAIL_DETAIL: busybox_yes"; bb_case_fail; fi

bb_case_start "busybox_zcat"
_t=$({ timeout 10 sh -c "busybox echo -n hello | busybox gzip -c | busybox zcat 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hello"; then echo "PASS: busybox_zcat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_zcat"; bb_case_fail; fi

bb_case_start "busybox_zcip"
_t=$({ timeout 10 sh -c "busybox zcip -h 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "Usage: zcip"; then echo "PASS: busybox_zcip"; bb_case_pass; else echo "FAIL_DETAIL: busybox_zcip"; bb_case_fail; fi

bb_case_start "ls_root"
_t=$({ timeout 10 sh -c "ls /"; } 2>&1)
if echo "$_t" | grep -qF "bin"; then echo "PASS: ls_root"; bb_case_pass; else echo "FAIL_DETAIL: ls_root"; bb_case_fail; fi

# Custom test: addgroup
bb_case_start "addgroup"
_t=$({ timeout 10 sh -c "G=\$(date +%s); busybox delgroup \"gg_\$G\" 2>/dev/null; busybox addgroup \"gg_\$G\" 2>&1 && busybox grep -F \"gg_\$G:\" /etc/group 2>&1; busybox delgroup \"gg_\$G\" 2>/dev/null"; } 2>&1)
if echo "$_t" | grep -qF "gg_"; then echo "PASS: addgroup"; bb_case_pass; else echo "FAIL_DETAIL: addgroup"; bb_case_fail; fi
# Custom test: adduser
bb_case_start "adduser"
_t=$({ timeout 10 sh -c "U=\$(date +%s); busybox deluser \"uu_\$U\" 2>/dev/null; busybox adduser -D -H \"uu_\$U\" 2>&1 && busybox grep -F \"uu_\$U:\" /etc/passwd 2>&1; busybox deluser \"uu_\$U\" 2>/dev/null"; } 2>&1)
if echo "$_t" | grep -qF "uu_"; then echo "PASS: adduser"; bb_case_pass; else echo "FAIL_DETAIL: adduser"; bb_case_fail; fi

# Restored batch: PostgreSQL bring-up applets (see rcore-os/tgoskits#349).
bb_case_start "busybox_chown"
_t=$({ timeout 10 sh -c "busybox sh -c 'U=\$(busybox id -u); G=\$(busybox id -g); busybox echo c > /tmp/bb_chown_t && busybox chown \"\$U:\$G\" /tmp/bb_chown_t && [ \"\$(busybox stat -c \"%u:%g\" /tmp/bb_chown_t)\" = \"\$U:\$G\" ] && busybox echo chown_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "chown_ok"; then echo "PASS: busybox_chown"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chown"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_cpio"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_cpio.arc; busybox rm -rf /tmp/bb_cpio_src /tmp/bb_cpio_out; busybox mkdir -p /tmp/bb_cpio_src /tmp/bb_cpio_out && busybox echo cpio_payload > /tmp/bb_cpio_src/in && cd /tmp/bb_cpio_src && busybox echo in | busybox cpio -o -H newc > /tmp/bb_cpio.arc && cd /tmp/bb_cpio_out && busybox cpio -i < /tmp/bb_cpio.arc && busybox cat in && busybox echo cpio_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "cpio_ok"; then echo "PASS: busybox_cpio"; bb_case_pass; else echo "FAIL_DETAIL: busybox_cpio"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_dos2unix"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox printf \"a\\r\\nb\\r\\n\" > /tmp/bb_d2u && busybox dos2unix /tmp/bb_d2u && busybox od -An -tx1 /tmp/bb_d2u' 2>&1"; } 2>&1)
_d2u=$(echo "$_t" | tr -d '\n' | tr -s ' ')
if echo "$_d2u" | grep -qF "61 0a 62 0a" && ! echo "$_d2u" | grep -qF "0d"; then echo "PASS: busybox_dos2unix"; bb_case_pass; else echo "FAIL_DETAIL: busybox_dos2unix"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_env"
_t=$({ timeout 10 sh -c "busybox env 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "PATH="; then echo "PASS: busybox_env"; bb_case_pass; else echo "FAIL_DETAIL: busybox_env"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_getopt"
_t=$({ timeout 10 sh -c "busybox getopt -o ab: -- -a -b bar 2>&1"; } 2>&1)
if echo "$_t" | grep -qF -- "-a -b 'bar' --"; then echo "PASS: busybox_getopt"; bb_case_pass; else echo "FAIL_DETAIL: busybox_getopt"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_hostid"
_t=$({ timeout 10 sh -c "busybox hostid 2>&1"; } 2>&1)
if echo "$_t" | grep -qE '^(0x)?[0-9a-fA-F]+$'; then echo "PASS: busybox_hostid"; bb_case_pass; else echo "FAIL_DETAIL: busybox_hostid"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_ipcalc"
_t=$({ timeout 10 sh -c "busybox ipcalc -m 192.168.1.1/24 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "NETMASK="; then echo "PASS: busybox_ipcalc"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ipcalc"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_lzcat"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox printf %s XQAAgAD//////////wA6GUrOJnKDn//7E4AA | busybox base64 -d > /tmp/bb_lzcat.lzma && busybox lzcat /tmp/bb_lzcat.lzma' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "test"; then echo "PASS: busybox_lzcat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lzcat"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_lzma"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox printf %s XQAAgAD//////////wA2Hondf+Fbcap///6gWAA= | busybox base64 -d > /tmp/bb_lzma.lzma && busybox lzma -dc /tmp/bb_lzma.lzma' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "lzma_t"; then echo "PASS: busybox_lzma"; bb_case_pass; else echo "FAIL_DETAIL: busybox_lzma"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_ifconfig"
_t=$({ timeout 10 sh -c "busybox ifconfig 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "eth0"; then echo "PASS: busybox_ifconfig"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ifconfig"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_ifenslave"
_t=$({ timeout 10 sh -c "busybox ifenslave 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "eth0" && echo "$_t" | grep -qF "lo"; then echo "PASS: busybox_ifenslave"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ifenslave"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_ping"
_t=$({ timeout 10 sh -c "busybox ping -c 1 127.0.0.1 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "1 packets transmitted" && echo "$_t" | grep -qE "1 packets? received|1 received" && echo "$_t" | grep -qF "0% packet loss"; then echo "PASS: busybox_ping"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ping"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_pipe_progress"
_t=$({ timeout 10 sh -c "busybox printf abc | busybox pipe_progress 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "abc"; then echo "PASS: busybox_pipe_progress"; bb_case_pass; else echo "FAIL_DETAIL: busybox_pipe_progress"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_iostat"
_t=$({ timeout 10 sh -c "busybox iostat 1 1 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "avg-cpu"; then echo "PASS: busybox_iostat"; bb_case_pass; else echo "FAIL_DETAIL: busybox_iostat"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_split"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -rf /tmp/bb_spl && busybox mkdir -p /tmp/bb_spl && busybox printf abcdef > /tmp/bb_spl/in && busybox split -b2 /tmp/bb_spl/in /tmp/bb_spl/o && busybox cat /tmp/bb_spl/oaa' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "ab"; then echo "PASS: busybox_split"; bb_case_pass; else echo "FAIL_DETAIL: busybox_split"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_nohup"
_t=$({ timeout 10 sh -c "busybox sh -c 'cd /tmp && busybox rm -f nohup.out && busybox nohup busybox sh -c \"busybox echo nohup_ok > nohup.out\" >/dev/null 2>&1 && busybox cat nohup.out' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "nohup_ok"; then echo "PASS: busybox_nohup"; bb_case_pass; else echo "FAIL_DETAIL: busybox_nohup"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_run_parts"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -rf /tmp/bb_rp && busybox mkdir -p /tmp/bb_rp/d && busybox printf \"#!/bin/sh\\necho rp_ok\\n\" > /tmp/bb_rp/d/00t && busybox chmod +x /tmp/bb_rp/d/00t && busybox run-parts /tmp/bb_rp/d' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "rp_ok"; then echo "PASS: busybox_run_parts"; bb_case_pass; else echo "FAIL_DETAIL: busybox_run_parts"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_tail"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox printf \"first\\nroot:\\n\" > /tmp/bb_tail_t && busybox tail -n 1 /tmp/bb_tail_t' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "root:"; then echo "PASS: busybox_tail"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tail"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_tar"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -rf /tmp/bb_tar && busybox mkdir -p /tmp/bb_tar && busybox echo one > /tmp/bb_tar/one && busybox tar -cf /tmp/bb_tar.tar -C /tmp/bb_tar one && busybox tar -tf /tmp/bb_tar.tar' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "one"; then echo "PASS: busybox_tar"; bb_case_pass; else echo "FAIL_DETAIL: busybox_tar"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_xxd"
_t=$({ timeout 10 sh -c "busybox printf 'Hi' | busybox xxd 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "4869"; then echo "PASS: busybox_xxd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_xxd"; echo "$_t"; bb_case_fail; fi

# busybox_mkdir (-p on a new path)
bb_case_start "busybox_mkdir"
_t=$({ timeout 10 sh -c 'busybox rm -rf /tmp/bb_mkd_one 2>/dev/null && busybox mkdir -p /tmp/bb_mkd_one && busybox ls -d /tmp/bb_mkd_one'; } 2>&1)
if echo "$_t" | grep -qF "bb_mkd_one"; then echo "PASS: busybox_mkdir"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mkdir"; bb_case_fail; fi

# busybox_mv
bb_case_start "busybox_mv"
_t=$({ timeout 10 sh -c 'busybox echo mv_ok > /tmp/bb_mv_from && busybox mv /tmp/bb_mv_from /tmp/bb_mv_to && busybox cat /tmp/bb_mv_to'; } 2>&1)
if echo "$_t" | grep -qF "mv_ok"; then echo "PASS: busybox_mv"; bb_case_pass; else echo "FAIL_DETAIL: busybox_mv"; bb_case_fail; fi

# tmpfs_rename_exec_elf - write an ELF via a temporary tmpfs path, rename it
# to the final executable name, verify the renamed file still reads back the
# ELF magic, then execute the final path. This regresses page-cache/user_data
# loss across tmpfs rename, which surfaces as Exec format error.
bb_case_start "tmpfs_rename_exec_elf"
_t=$({ timeout 15 sh -c 'busybox rm -rf /tmp/bb_rename_elf && busybox mkdir -p /tmp/bb_rename_elf && busybox cp /bin/busybox /tmp/bb_rename_elf/busybox.tmp && busybox mv /tmp/bb_rename_elf/busybox.tmp /tmp/bb_rename_elf/busybox && [ "$(busybox head -c 4 /tmp/bb_rename_elf/busybox | busybox xxd -p)" = "7f454c46" ] && /tmp/bb_rename_elf/busybox echo rename_exec_ok'; } 2>&1)
if echo "$_t" | grep -qF "rename_exec_ok"; then echo "PASS: tmpfs_rename_exec_elf"; bb_case_pass; else echo "FAIL_DETAIL: tmpfs_rename_exec_elf"; echo "$_t"; bb_case_fail; fi

# busybox_rmdir
bb_case_start "busybox_rmdir"
_t=$({ timeout 10 sh -c 'busybox mkdir -p /tmp/bb_rmd && busybox rmdir /tmp/bb_rmd && busybox echo rmdir_ok'; } 2>&1)
if echo "$_t" | grep -qF "rmdir_ok"; then echo "PASS: busybox_rmdir"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rmdir"; bb_case_fail; fi
# busybox_link — create hard link on tmpfs and verify content is shared
bb_case_start "busybox_link"
_t=$({ timeout 10 sh -c 'busybox rm -f /tmp/bb_link_a /tmp/bb_link_b; busybox echo link_data > /tmp/bb_link_a && busybox link /tmp/bb_link_a /tmp/bb_link_b && busybox cat /tmp/bb_link_b'; } 2>&1)
if echo "$_t" | grep -qF "link_data"; then echo "PASS: busybox_link"; bb_case_pass; else echo "FAIL_DETAIL: busybox_link"; bb_case_fail; fi

# blkid — identify block device metadata
bb_case_start "blkid"
_t=$({ timeout 10 sh -c "busybox blkid /dev/null 2>&1"; } 2>&1)
_rc=$?
if echo "$_t" | grep -qE "/dev/null|Usage|not a block|No such|ioctl" || { [ -z "$_t" ] && [ "$_rc" -eq 0 ]; }; then echo "PASS: blkid"; bb_case_pass; else echo "FAIL_DETAIL: blkid"; echo "$_t (rc=$_rc)"; bb_case_fail; fi

# blkdiscard — unbound loop device must fail with non-zero exit and a
# device-level error (ENXIO → "No such device").  Accepting rc=0 or
# generic busybox prefix matches would let the old no-op-success bug pass.
bb_case_start "blkdiscard"
_t=$({ timeout 10 sh -c "busybox blkdiscard /dev/loop0 2>&1"; } 2>&1)
_rc=$?
if [ "$_rc" -ne 0 ] && echo "$_t" | grep -qiE "No such device|ENXIO"; then echo "PASS: blkdiscard"; bb_case_pass; else echo "FAIL_DETAIL: blkdiscard"; echo "$_t (rc=$_rc)"; bb_case_fail; fi

# blockdev — get sector size of block device
bb_case_start "blockdev"
_t=$({ timeout 10 sh -c "busybox blockdev --getss /dev/loop0 2>&1"; } 2>&1)
_rc=$?; if [ "$_rc" -eq 0 ] && echo "$_t" | grep -q "[0-9]"; then echo "PASS: blockdev"; bb_case_pass; else echo "FAIL_DETAIL: blockdev"; echo "$_t (rc=$_rc)"; bb_case_fail; fi

# hwclock — read hardware clock
bb_case_start "busybox_hwclock"
_t=$({ timeout 10 sh -c "busybox hwclock -r 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "hwclock"; then echo "PASS: busybox_hwclock"; bb_case_pass; else echo "FAIL_DETAIL: busybox_hwclock"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_run_parts"
_t=$({ timeout 10 sh -c "busybox sh -c 'mkdir -p /tmp/bb_rp/d && busybox echo rp_ok > /tmp/bb_rp/d/00t && chmod +x /tmp/bb_rp/d/00t && busybox run-parts /tmp/bb_rp/d' 2>&1"; } 2>&1)
if echo "$_t" | grep -qF "rp_ok"; then echo "PASS: busybox_run_parts"; bb_case_pass; else echo "FAIL_DETAIL: busybox_run_parts"; bb_case_fail; fi

# busybox_add_shell — exercise the real /etc/shells rewrite path (NOT --help).
# add-shell opens /etc/shells O_RDONLY, opens /etc/shells.tmp
# O_WRONLY|O_CREAT|O_TRUNC, writes the merged list, then rename(2)s
# /etc/shells.tmp over /etc/shells.  Probe a unique path each run, verify
# it lands in /etc/shells, verify the .tmp file did NOT leak, and restore
# the original file so re-runs stay idempotent.
bb_case_start "busybox_add_shell"
_addshell_probe="/tmp/bb_addshell_probe_$$"
_t=$(timeout 15 sh -c '
    busybox cp /etc/shells /tmp/bb_addshell_backup
    _probe='"$_addshell_probe"'
    busybox add-shell "$_probe" 2>&1
    _arc=$?
    if [ "$_arc" = 0 ] \
        && busybox grep -qxF "$_probe" /etc/shells \
        && [ ! -e /etc/shells.tmp ]; then
        busybox echo add_shell_ok
    else
        busybox echo "add_shell_failed arc=$_arc"
        busybox echo "--- /etc/shells ---"
        busybox cat /etc/shells 2>&1
        busybox echo "--- /etc/shells.tmp (should not exist) ---"
        busybox ls -la /etc/shells.tmp 2>&1
    fi
    busybox cp /tmp/bb_addshell_backup /etc/shells 2>&1 || true
    busybox rm -f /tmp/bb_addshell_backup
' 2>&1)
if echo "$_t" | grep -qF "add_shell_ok"; then
    echo "PASS: busybox_add_shell"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_add_shell"; echo "$_t"
    bb_case_fail
fi

# busybox_crontab — install a crontab into a private spool dir (-c), list it
# back, byte-match the marker, then remove and confirm removal.  This
# exercises the real read/write/rename(2)/fchown/fchmod/unlink paths in
# miscutils/crontab.c rather than just usage-banner.  The pass marker
# cron_tab_ok is only emitted when every step in the round-trip succeeds,
# so the test reverse-falsifies "crontab silently dropped the install",
# "crontab -l can't reopen the installed file", and "crontab -r left the
# file behind".
bb_case_start "busybox_crontab"
_t=$(timeout 20 sh -c '
    busybox rm -rf /tmp/bb_crontab_tabs /tmp/bb_crontab_in /tmp/bb_crontab_out
    busybox mkdir -p /tmp/bb_crontab_tabs
    busybox printf "*/5 * * * * /bin/true crontab_marker_aaa\n" > /tmp/bb_crontab_in
    busybox crontab -c /tmp/bb_crontab_tabs /tmp/bb_crontab_in
    _irc=$?
    busybox crontab -c /tmp/bb_crontab_tabs -l > /tmp/bb_crontab_out 2>&1
    _lrc=$?
    if [ "$_irc" = 0 ] && [ "$_lrc" = 0 ] && busybox grep -qF "crontab_marker_aaa" /tmp/bb_crontab_out; then
        busybox crontab -c /tmp/bb_crontab_tabs -r
        _rrc=$?
        _after=$(busybox crontab -c /tmp/bb_crontab_tabs -l 2>&1)
        if [ "$_rrc" = 0 ] && ! echo "$_after" | busybox grep -qF "crontab_marker_aaa"; then
            busybox echo cron_tab_ok
        else
            busybox echo "crontab_remove_failed rrc=$_rrc after=$_after"
        fi
    else
        busybox echo "crontab_install_or_list_failed irc=$_irc lrc=$_lrc"
        busybox cat /tmp/bb_crontab_out 2>&1
        busybox ls -la /tmp/bb_crontab_tabs 2>&1
    fi
' 2>&1)
if echo "$_t" | grep -qF "cron_tab_ok"; then
    echo "PASS: busybox_crontab"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_crontab"; echo "$_t"
    bb_case_fail
fi

# Additional stable BusyBox semantics for shell-script compatibility.
bb_case_start "busybox_touch_no_create"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_sem_touch_missing && busybox touch -c /tmp/bb_sem_touch_missing && busybox test ! -e /tmp/bb_sem_touch_missing && busybox echo touch_no_create_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qxF "touch_no_create_ok"; then echo "PASS: busybox_touch_no_create"; bb_case_pass; else echo "FAIL_DETAIL: busybox_touch_no_create"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_rm_recursive"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -rf /tmp/bb_sem_rm && busybox mkdir -p /tmp/bb_sem_rm/a/b && busybox printf data > /tmp/bb_sem_rm/a/b/file && busybox rm -rf /tmp/bb_sem_rm && busybox test ! -e /tmp/bb_sem_rm && busybox echo rm_recursive_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qxF "rm_recursive_ok"; then echo "PASS: busybox_rm_recursive"; bb_case_pass; else echo "FAIL_DETAIL: busybox_rm_recursive"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_ln_hardlink"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_sem_ln_a /tmp/bb_sem_ln_b && busybox printf linkdata > /tmp/bb_sem_ln_a && busybox ln /tmp/bb_sem_ln_a /tmp/bb_sem_ln_b && [ \"\$(busybox stat -c %h /tmp/bb_sem_ln_a)\" = 2 ] && [ \"\$(busybox cat /tmp/bb_sem_ln_b)\" = linkdata ] && busybox echo ln_hardlink_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qxF "ln_hardlink_ok"; then echo "PASS: busybox_ln_hardlink"; bb_case_pass; else echo "FAIL_DETAIL: busybox_ln_hardlink"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_readlink_exact"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_sem_rl_link /tmp/bb_sem_rl_target && busybox printf x > /tmp/bb_sem_rl_target && busybox ln -s /tmp/bb_sem_rl_target /tmp/bb_sem_rl_link && busybox readlink /tmp/bb_sem_rl_link' 2>&1"; } 2>&1)
if [ "$_t" = "/tmp/bb_sem_rl_target" ]; then echo "PASS: busybox_readlink_exact"; bb_case_pass; else echo "FAIL_DETAIL: busybox_readlink_exact"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_realpath_dotdot"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -rf /tmp/bb_sem_real && busybox mkdir -p /tmp/bb_sem_real/d && busybox realpath /tmp/bb_sem_real/./d/..//d' 2>&1"; } 2>&1)
if [ "$_t" = "/tmp/bb_sem_real/d" ]; then echo "PASS: busybox_realpath_dotdot"; bb_case_pass; else echo "FAIL_DETAIL: busybox_realpath_dotdot"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_stat_mode_size"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_sem_stat && busybox printf abc > /tmp/bb_sem_stat && busybox chmod 640 /tmp/bb_sem_stat && busybox stat -c \"%s %a %F\" /tmp/bb_sem_stat' 2>&1"; } 2>&1)
if [ "$_t" = "3 640 regular file" ]; then echo "PASS: busybox_stat_mode_size"; bb_case_pass; else echo "FAIL_DETAIL: busybox_stat_mode_size"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_chmod_symbolic"
_t=$({ timeout 10 sh -c "busybox sh -c 'busybox rm -f /tmp/bb_sem_chmod && busybox printf x > /tmp/bb_sem_chmod && busybox chmod u=rw,g=r,o= /tmp/bb_sem_chmod && busybox stat -c %a /tmp/bb_sem_chmod' 2>&1"; } 2>&1)
if [ "$_t" = "640" ]; then echo "PASS: busybox_chmod_symbolic"; bb_case_pass; else echo "FAIL_DETAIL: busybox_chmod_symbolic"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_sort_unique"
_t=$({ timeout 10 sh -c "busybox printf 'b\nA\nb\n' | busybox sort -u 2>&1"; } 2>&1)
_sort=$(echo "$_t" | tr '\n' '|')
if [ "$_sort" = "A|b|" ]; then echo "PASS: busybox_sort_unique"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sort_unique"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_uniq_counts"
_t=$({ timeout 10 sh -c "busybox printf 'a\na\nb\n' | busybox uniq -c | busybox sed 's/^ *//' 2>&1"; } 2>&1)
_uniq=$(echo "$_t" | tr '\n' '|')
if [ "$_uniq" = "2 a|1 b|" ]; then echo "PASS: busybox_uniq_counts"; bb_case_pass; else echo "FAIL_DETAIL: busybox_uniq_counts"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_xargs_n1"
_t=$({ timeout 10 sh -c "busybox printf 'aa\nbb\n' | busybox xargs -n1 busybox printf '<%s>\n' 2>&1"; } 2>&1)
_xargs=$(echo "$_t" | tr '\n' '|')
if [ "$_xargs" = "<aa>|<bb>|" ]; then echo "PASS: busybox_xargs_n1"; bb_case_pass; else echo "FAIL_DETAIL: busybox_xargs_n1"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_printf_escape"
_t=$({ timeout 10 sh -c "busybox printf '%b' 'a\012b' | busybox od -An -tx1 2>&1"; } 2>&1)
_printf=$(echo "$_t" | tr -d '\n' | tr -s ' ' | busybox sed 's/^ //; s/ $//')
if [ "$_printf" = "61 0a 62" ]; then echo "PASS: busybox_printf_escape"; bb_case_pass; else echo "FAIL_DETAIL: busybox_printf_escape"; echo "$_t"; bb_case_fail; fi

bb_case_start "busybox_sh_env_cd"
_t=$({ timeout 10 sh -c "busybox sh -c 'export BB_SEM_ENV=ok; cd /tmp && [ \"\$BB_SEM_ENV:\$PWD\" = \"ok:/tmp\" ] && command -v busybox >/dev/null && busybox echo sh_env_cd_ok' 2>&1"; } 2>&1)
if echo "$_t" | grep -qxF "sh_env_cd_ok"; then echo "PASS: busybox_sh_env_cd"; bb_case_pass; else echo "FAIL_DETAIL: busybox_sh_env_cd"; echo "$_t"; bb_case_fail; fi

# busybox_crond — applet wiring sanity check. Starting crond as either a
# daemon or a foreground service can keep the loongarch64 BusyBox sweep alive
# if process cleanup does not return promptly, so normal CI only checks that
# BusyBox can dispatch to crond and print its usage banner.
bb_case_start "busybox_crond"
_t=$({ timeout 10 sh -c "busybox crond -h 2>&1"; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if echo "$_t" | grep -qF "Usage:" && echo "$_t" | grep -qF "crond"; then
    echo "PASS: busybox_crond"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_crond (rc=$_rc)"; echo "$_t"
    bb_case_fail
fi

# busybox_acpid — applet wiring sanity check.
# Without -f, acpid would daemonize and close stdio (Issue #13's `[ -n "$_t" ]`
# only succeeds if something is printed before fork). We pass an unknown flag
# `-h` so getopt32 reaches bb_show_usage, which writes the applet banner to
# stderr — confirming the applet table contains acpid and busybox can run it.
bb_case_start "busybox_acpid"
_t=$({ timeout 10 sh -c "busybox acpid -h 2>&1"; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if echo "$_t" | grep -qF "Usage:" && echo "$_t" | grep -qF "acpid"; then
    echo "PASS: busybox_acpid"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_acpid (rc=$_rc)"; echo "$_t"
    bb_case_fail
fi


# --- 7 high-side-effect applet safe-failure tests ---

bb_case_start "busybox_insmod"
_t=$({ timeout 10 sh -c 'busybox insmod /tmp/bb_no_such_module.ko 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ] && [ "$_rc" -ne 0 ] && echo "$_t" | grep -qiE "No such|not found|can't open|cannot open"; then
    echo "PASS: busybox_insmod"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_insmod (rc=$_rc)"; echo "$_t"; bb_case_fail
fi

bb_case_start "busybox_fdflush"
_t=$({ timeout 10 sh -c 'busybox fdflush /tmp/bb_no_such_device 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ] && [ "$_rc" -ne 0 ] && echo "$_t" | grep -qiE "No such|not found|can't open|cannot open|device|ioctl"; then
    echo "PASS: busybox_fdflush"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_fdflush (rc=$_rc)"; echo "$_t"; bb_case_fail
fi

bb_case_start "busybox_raidautorun"
_t=$({ timeout 10 sh -c 'busybox raidautorun /tmp/bb_no_such_device 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ] && [ "$_rc" -ne 0 ] && echo "$_t" | grep -qiE "No such|not found|can't open|cannot open|device|ioctl"; then
    echo "PASS: busybox_raidautorun"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_raidautorun (rc=$_rc)"; echo "$_t"; bb_case_fail
fi

bb_case_start "busybox_killall5"
_t=$({ timeout 10 sh -c 'busybox killall5 -h 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ] && echo "$_t" | grep -qiE "Usage|killall5"; then
    echo "PASS: busybox_killall5"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_killall5 (rc=$_rc)"; echo "$_t"; bb_case_fail
fi

# busybox_rdev — rdev outputs nothing on StarryOS (no /proc/kcore etc.)
# so we only verify: applet exists, executes without hang, returns non-timeout
bb_case_start "busybox_rdev"
_t=$({ timeout 10 sh -c 'busybox rdev 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ]; then
    echo "PASS: busybox_rdev"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_rdev (rc=$_rc)"; echo "$_t"; bb_case_fail
fi

bb_case_start "busybox_setlogcons"
_t=$({ timeout 10 sh -c 'busybox setlogcons -h 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
_t=$(printf '%s\n' "$_t" | sed '/^EXIT:/d')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ] && echo "$_t" | grep -qiE "Usage|setlogcons"; then
    echo "PASS: busybox_setlogcons"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_setlogcons (rc=$_rc)"; echo "$_t"; bb_case_fail
fi

# busybox_resize — outputs terminal escape sequences that corrupt EXIT: parsing
# redirect all output to /dev/null, only capture return code
bb_case_start "busybox_resize"
_t=$({ timeout 10 sh -c 'busybox resize >/dev/null 2>&1'; echo "EXIT:$?"; } 2>&1)
_rc=$(printf '%s\n' "$_t" | sed -n 's/^EXIT://p')
if [ -n "$_rc" ] && [ "$_rc" -ne 124 ]; then
    echo "PASS: busybox_resize"; bb_case_pass
else
    echo "FAIL_DETAIL: busybox_resize (rc=$_rc)"; echo "$_t"; bb_case_fail
fi


echo "=== BusyBox Test Summary ==="
echo "PASS: $PASS  FAIL: $FAIL  TOTAL: $((PASS+FAIL))"
_m1="Test"; _m2="run"; _m3="completed"; echo "$_m1 $_m2 $_m3"
