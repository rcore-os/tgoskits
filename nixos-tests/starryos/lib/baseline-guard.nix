{ lib, extraModules }:
let
  textEnablesForbidden =
    text:
    lib.any (
      pattern: builtins.match pattern text != null
    ) [
      ".*services\\.udev\\.enable[[:space:]]*=[[:space:]]*true.*"
      ".*services\\.dbus\\.enable[[:space:]]*=[[:space:]]*true.*"
      ".*services\\.nscd\\.enable[[:space:]]*=[[:space:]]*true.*"
      ".*services\\.logind\\.enable[[:space:]]*=[[:space:]]*true.*"
      ".*console-getty\\.enable[[:space:]]*=[[:space:]]*true.*"
      ".*getty@tty1.*enable[[:space:]]*=[[:space:]]*true.*"
      ".*autovt@.*enable[[:space:]]*=[[:space:]]*true.*"
      ".*networking\\.useDHCP[[:space:]]*=[[:space:]]*true.*"
      ".*nix\\.enable[[:space:]]*=[[:space:]]*true.*"
    ];

  attrEnablesForbidden =
    module:
    (module.services.udev.enable or false)
    || (module.services.dbus.enable or false)
    || (module.services.nscd.enable or false)
    || (module.services.logind.enable or false)
    || (module.networking.useDHCP or false)
    || (module.nix.enable or false)
    || (module.systemd.services.console-getty.enable or false)
    || (module.systemd.services."getty@tty1".enable or false)
    || (module.systemd.services."autovt@".enable or false);

  moduleEnablesForbidden =
    module:
    if builtins.isPath module || builtins.isString module then
      textEnablesForbidden (builtins.readFile module)
    else if builtins.isAttrs module && !builtins.isFunction module then
      attrEnablesForbidden module
    else
      false;

  forbidden = lib.filter moduleEnablesForbidden extraModules;
in
if forbidden == [ ] then
  extraModules
else
  throw "STARRY_NIXOS_PHASE_FAILED=artifact-preparation: extra modules must not enable services.udev, services.dbus, services.nscd, services.logind, getty, networking.useDHCP, or nix.enable"
