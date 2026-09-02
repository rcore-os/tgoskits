{ config, lib, ... }:
{
  # Keep these assertions at force priority so extra modules cannot clear them.
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
  ];
}
