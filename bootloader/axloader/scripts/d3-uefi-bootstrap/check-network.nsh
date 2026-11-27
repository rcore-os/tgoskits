# Collect read-only network protocol information for the D3 diagnostics.
# Run from the USB EFI/D3NET directory; the current directory receives
# NETWORK-CHECK.LOG. This script does not load or connect drivers.

@echo -off
set -v d3net_check_err 0x0

echo ============================================================

# TOUCH only updates an existing file. SETSIZE can create it with
# EFI_FILE_MODE_CREATE, so use SETSIZE only when the report is absent.
if exist NETWORK-CHECK.LOG then
  touch NETWORK-CHECK.LOG
  set -v d3net_check_err %lasterror%
else
  setsize 0 NETWORK-CHECK.LOG
  set -v d3net_check_err %lasterror%
endif

if not %d3net_check_err% == 0x0 then
  echo [NETCHECK] ERROR: cannot write report .\NETWORK-CHECK.LOG
  echo [NETCHECK] Preserved preflight error: %d3net_check_err%
  exit /b %d3net_check_err%
endif

echo ============================================================ >>a NETWORK-CHECK.LOG
set -v d3net_check_err %lasterror%
if not %d3net_check_err% == 0x0 then
  echo [NETCHECK] ERROR: cannot append .\NETWORK-CHECK.LOG
  echo [NETCHECK] Preserved last error: %d3net_check_err%
  exit /b %d3net_check_err%
endif

echo D3NET NETWORK CHECK RUN START >>a NETWORK-CHECK.LOG
echo [NETCHECK] RUN START >>a NETWORK-CHECK.LOG
echo [NETCHECK] DATE >>a NETWORK-CHECK.LOG
date >>a NETWORK-CHECK.LOG
echo [NETCHECK] TIME >>a NETWORK-CHECK.LOG
time >>a NETWORK-CHECK.LOG

echo [NETCHECK] VER >>a NETWORK-CHECK.LOG
ver >>a NETWORK-CHECK.LOG
echo [NETCHECK] MAP >>a NETWORK-CHECK.LOG
map >>a NETWORK-CHECK.LOG
echo [NETCHECK] DRIVERS >>a NETWORK-CHECK.LOG
drivers >>a NETWORK-CHECK.LOG

echo [NETCHECK] NII3.1
echo [NETCHECK] NII3.1 >>a NETWORK-CHECK.LOG
dh -p 1ACED566-76ED-4218-BC81-767F1F977A89
dh -p 1ACED566-76ED-4218-BC81-767F1F977A89 >>a NETWORK-CHECK.LOG

echo [NETCHECK] RNG
echo [NETCHECK] RNG >>a NETWORK-CHECK.LOG
dh -p 3152BCA5-EADE-433D-862E-C01CDC291F44
dh -p 3152BCA5-EADE-433D-862E-C01CDC291F44 >>a NETWORK-CHECK.LOG

echo [NETCHECK] HASH2 SERVICE BINDING
echo [NETCHECK] HASH2 SERVICE BINDING >>a NETWORK-CHECK.LOG
dh -p DA836F8D-217F-4CA0-99C2-1CA4E16077EA
dh -p DA836F8D-217F-4CA0-99C2-1CA4E16077EA >>a NETWORK-CHECK.LOG

echo [NETCHECK] SNP
echo [NETCHECK] SNP >>a NETWORK-CHECK.LOG
dh -p A19832B9-AC25-11D3-9A2D-0090273FC14D
dh -p A19832B9-AC25-11D3-9A2D-0090273FC14D >>a NETWORK-CHECK.LOG

echo [NETCHECK] IP4CONFIG2
echo [NETCHECK] IP4CONFIG2 >>a NETWORK-CHECK.LOG
dh -p 5B446ED1-E30B-4FAA-871A-3654ECA36080
dh -p 5B446ED1-E30B-4FAA-871A-3654ECA36080 >>a NETWORK-CHECK.LOG

echo [NETCHECK] UDP4 SERVICE BINDING
echo [NETCHECK] UDP4 SERVICE BINDING >>a NETWORK-CHECK.LOG
dh -p 83F01464-99BD-45E5-B383-AF6305D8E9E6
dh -p 83F01464-99BD-45E5-B383-AF6305D8E9E6 >>a NETWORK-CHECK.LOG

echo [NETCHECK] TCP4 SERVICE BINDING
echo [NETCHECK] TCP4 SERVICE BINDING >>a NETWORK-CHECK.LOG
dh -p 00720665-67EB-4A99-BAF7-D3C33A1C7CC9
dh -p 00720665-67EB-4A99-BAF7-D3C33A1C7CC9 >>a NETWORK-CHECK.LOG

echo [NETCHECK] RUN END >>a NETWORK-CHECK.LOG
set -v d3net_check_err %lasterror%
if not %d3net_check_err% == 0x0 then
  echo [NETCHECK] ERROR: final report append failed.
  echo [NETCHECK] Preserved last error: %d3net_check_err%
  exit /b %d3net_check_err%
endif

echo [NETCHECK] Check complete. Report appended: .\NETWORK-CHECK.LOG
exit /b 0
