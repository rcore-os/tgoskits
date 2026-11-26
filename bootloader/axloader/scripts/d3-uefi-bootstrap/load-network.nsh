# Load the IPv4 network stack for the D3 diagnostics session.
# Run this script from the EFI/D3NET directory containing these EFI files.
# Pass "no-report" to skip check-network.nsh for unattended startup.

@echo -off
set -v d3net_err 0x0
set -v d3net_skip_check 0
if x%1 == xno-report then
  set -v d3net_skip_check 1
endif

echo [D3NET] Loading DpcDxe.efi
load -nc .\DpcDxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: DpcDxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading SnpDxe.efi
load -nc .\SnpDxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: SnpDxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading MnpDxe.efi
load -nc .\MnpDxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: MnpDxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading ArpDxe.efi
load -nc .\ArpDxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: ArpDxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading Ip4Dxe.efi
load -nc .\Ip4Dxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: Ip4Dxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading Udp4Dxe.efi
load -nc .\Udp4Dxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: Udp4Dxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading Dhcp4Dxe.efi
load -nc .\Dhcp4Dxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: Dhcp4Dxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Loading TcpDxe.efi
load -nc .\TcpDxe.efi
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: TcpDxe.efi failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

echo [D3NET] Connecting drivers recursively.
connect -r
set -v d3net_err %lasterror%
if not %d3net_err% == 0x0 then
  echo [D3NET] ERROR: connect -r failed.
  echo [D3NET] Preserved last error: %d3net_err%
  exit /b %d3net_err%
endif

if %d3net_skip_check% == 0 then
  if exist check-network.nsh then
    echo [D3NET] Running check-network.nsh.
    check-network.nsh
    set -v d3net_check_result %lasterror%
    if not %d3net_check_result% == 0x0 then
      echo [D3NET] ERROR: check-network.nsh failed.
      echo [D3NET] Preserved query error: %d3net_check_result%
      exit /b %d3net_check_result%
    endif
  else
    echo [D3NET] check-network.nsh not found; network load completed.
  endif
else
  echo [D3NET] no-report mode: skipping check-network.nsh.
endif

echo [D3NET] Core stack load completed. No loader was started.
exit /b 0
