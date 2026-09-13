{ config, lib, ... }:
{
  # `mkForce` wins against extra modules that clear `assertions`, including
  # `assertions = lib.mkForce [ ]`. Keep the Stage-2 sysctl guards in this
  # same forced list so those app baseline invariants still run.
  assertions = lib.mkForce [
    {
      assertion = !(config.services.udev.enable or false);
      message = "Starry nixosTest extra modules must not enable services.udev";
    }
    {
      assertion = !(config.services.dbus.enable or false);
      message = "Starry nixosTest extra modules must not enable services.dbus";
    }
    {
      assertion = !(config.services.nscd.enable or false);
      message = "Starry nixosTest extra modules must not enable services.nscd";
    }
    {
      assertion = !(config.services.logind.enable or false);
      message = "Starry nixosTest extra modules must not enable services.logind";
    }
    {
      assertion = !(config.networking.useDHCP or false);
      message = "Starry nixosTest extra modules must not enable networking.useDHCP";
    }
    {
      assertion = !(config.nix.enable or false);
      message = "Starry nixosTest extra modules must not enable nix.enable";
    }
    {
      assertion = !(config.systemd.services.console-getty.enable or false);
      message = "Starry nixosTest extra modules must not enable console-getty";
    }
    {
      assertion = !(config.systemd.services."getty@tty1".enable or false);
      message = "Starry nixosTest extra modules must not enable getty@tty1";
    }
    {
      assertion = !(config.systemd.services."autovt@".enable or false);
      message = "Starry nixosTest extra modules must not enable autovt@";
    }
    {
      assertion = (config.boot.kernel.sysctl."kernel.pid_max" or null) == null;
      message = "StarryNixOS Stage-2 must not configure kernel.pid_max before PID allocation enforces it";
    }
    {
      assertion = (config.boot.kernel.sysctl."vm.max_map_count" or null) == null;
      message = "StarryNixOS Stage-2 must not configure vm.max_map_count before VMA admission enforces it";
    }
  ];
}
