# Unattended D3 network bootstrap from the USB containing this Shell.
# The Shell invokes this file from its own EFI/BOOT directory.

@echo -off
set -v d3_auto_err 0x0
set -v d3_auto_home %homefilesystem%
set -v d3_auto_d3net %d3_auto_home%\EFI\D3NET
set -v d3_auto_rng %d3_auto_d3net%\RngDxe.efi
set -v d3_auto_hash2 %d3_auto_d3net%\Hash2DxeCrypto.efi
set -v d3_auto_serial %d3_auto_d3net%\PciSioSerialDxe.efi
set -v d3_auto_load %d3_auto_d3net%\load-network.nsh
set -v d3_auto_launcher %d3_auto_home%\EFI\AXLOADER\LAUNCHER.EFI

echo [D3-AUTOBOOT] START
echo [D3-AUTOBOOT] startup=%0
echo [D3-AUTOBOOT] home=%d3_auto_home%

if not exist %d3_auto_rng% then
  echo [D3-AUTOBOOT] ERROR: missing %d3_auto_rng%
  exit /b 0xE
endif

if not exist %d3_auto_hash2% then
  echo [D3-AUTOBOOT] ERROR: missing %d3_auto_hash2%
  exit /b 0xE
endif

if not exist %d3_auto_load% then
  echo [D3-AUTOBOOT] ERROR: missing %d3_auto_load%
  exit /b 0xE
endif

if not exist %d3_auto_launcher% then
  echo [D3-AUTOBOOT] ERROR: missing %d3_auto_launcher%
  exit /b 0xE
endif

echo [D3-AUTOBOOT] select volume %d3_auto_home%
%d3_auto_home%
set -v d3_auto_err %lasterror%
if not %d3_auto_err% == 0x0 then
  echo [D3-AUTOBOOT] ERROR: cannot select startup volume.
  echo [D3-AUTOBOOT] Preserved error: %d3_auto_err%
  exit /b %d3_auto_err%
endif

echo [D3-AUTOBOOT] cd \EFI\D3NET
cd \EFI\D3NET
set -v d3_auto_err %lasterror%
if not %d3_auto_err% == 0x0 then
  echo [D3-AUTOBOOT] ERROR: cannot enter EFI/D3NET.
  echo [D3-AUTOBOOT] Preserved error: %d3_auto_err%
  exit /b %d3_auto_err%
endif

if exist %d3_auto_serial% then
  echo [D3-AUTOBOOT] load optional PciSioSerialDxe.efi
  load -nc .\PciSioSerialDxe.efi
  set -v d3_auto_err %lasterror%
  if not %d3_auto_err% == 0x0 then
    echo [D3-AUTOBOOT] ERROR: PciSioSerialDxe.efi failed.
    echo [D3-AUTOBOOT] Preserved error: %d3_auto_err%
    exit /b %d3_auto_err%
  endif
endif

echo [D3-AUTOBOOT] load RngDxe.efi
load -nc .\RngDxe.efi
set -v d3_auto_err %lasterror%
if not %d3_auto_err% == 0x0 then
  echo [D3-AUTOBOOT] ERROR: RngDxe.efi failed.
  echo [D3-AUTOBOOT] Preserved error: %d3_auto_err%
  exit /b %d3_auto_err%
endif

echo [D3-AUTOBOOT] load Hash2DxeCrypto.efi
load -nc .\Hash2DxeCrypto.efi
set -v d3_auto_err %lasterror%
if not %d3_auto_err% == 0x0 then
  echo [D3-AUTOBOOT] ERROR: Hash2DxeCrypto.efi failed.
  echo [D3-AUTOBOOT] Preserved error: %d3_auto_err%
  exit /b %d3_auto_err%
endif

echo [D3-AUTOBOOT] run load-network.nsh no-report
.\load-network.nsh no-report
set -v d3_auto_err %lasterror%
if not %d3_auto_err% == 0x0 then
  echo [D3-AUTOBOOT] ERROR: load-network.nsh no-report failed.
  echo [D3-AUTOBOOT] Preserved error: %d3_auto_err%
  exit /b %d3_auto_err%
endif

echo [D3-AUTOBOOT] launch %d3_auto_launcher%
%d3_auto_launcher%
set -v d3_auto_launcher_status %lasterror%
echo [D3-AUTOBOOT] launcher returned: %d3_auto_launcher_status%
exit /b %d3_auto_launcher_status%
