{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
{
  imports = [ (modulesPath + "/profiles/docker-container.nix") ];

  boot.isContainer = true;
  boot.kernel.enable = false;
  networking.hostName = "starrynixos";
  networking.useDHCP = false;
  networking.resolvconf.enable = false;
  networking.firewall.enable = false;

  nix.enable = false;
  nix.channel.enable = false;
  system.installer.channel.enable = lib.mkForce false;
  services.dbus.enable = lib.mkForce false;
  services.nscd.enable = false;
  services.logind.enable = false;
  system.nssModules = lib.mkForce [ ];
  security.sudo.enable = false;
  documentation.enable = false;
  users.manageLingering = false;

  assertions = [
    {
      assertion = (config.boot.kernel.sysctl."kernel.pid_max" or null) == null;
      message = "StarryNixOS Stage-2 must not configure kernel.pid_max before PID allocation enforces it";
    }
    {
      assertion = (config.boot.kernel.sysctl."vm.max_map_count" or null) == null;
      message = "StarryNixOS Stage-2 must not configure vm.max_map_count before VMA admission enforces it";
    }
  ];

  # The container profile's defaults tune limits that StarryOS can expose but
  # cannot yet enforce in PID allocation or VMA admission. Omit only these
  # writes so systemd-sysctl does not claim a limit that the kernel ignores.
  boot.kernel.sysctl = {
    "kernel.pid_max" = lib.mkForce null;
    "vm.max_map_count" = lib.mkForce null;
  };

  # StarryOS provides the mounted device tree and does not expose Linux uevents.
  # Running udevd cannot discover additional devices and leaves sysinit waiting
  # for a readiness notification from an unsupported device-management path.
  services.udev.enable = false;
  systemd.suppressedSystemUnits = [
    "systemd-udevd-control.socket"
    "systemd-udevd-kernel.socket"
    "systemd-udevd.service"
    "systemd-udev-trigger.service"
  ];

  # The reusable image intentionally keeps a transient per-boot machine ID.
  # Committing it requires the mount-namespace transition used to replace the
  # temporary /etc/machine-id mount, which is outside the Stage-2 baseline.
  systemd.services.systemd-machine-id-commit.enable = false;

  systemd.services.console-getty.enable = false;
  systemd.services."getty@tty1".enable = false;
  systemd.services."autovt@".enable = false;
  systemd.targets.getty.wants = lib.mkForce [ ];

  users.users.root.hashedPassword = "!";
  users.users.starry = {
    isNormalUser = true;
    hashedPassword = "!";
    description = "StarryNixOS declarative account";
  };

  environment.systemPackages = [
    pkgs.coreutils
    pkgs.hello
    pkgs.procps
  ];

  systemd.services.starry-nixos-marker = {
    description = "Verify the StarryNixOS stage-2 baseline";
    after = [ "multi-user.target" ];
    serviceConfig = {
      Type = "oneshot";
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
    path = [
      config.systemd.package
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gnused
      pkgs.hello
    ];
    script = ''
      set -eu

      pid1_exe="$(readlink -f /proc/1/exe)"
      case "$pid1_exe" in
        /nix/store/*-systemd-*/lib/systemd/systemd) ;;
        *)
          echo "STARRY_NIXOS_SYSTEM_FAILED: phase=pid1 unexpected=$pid1_exe"
          exit 1
          ;;
      esac
      echo "STARRY_NIXOS_PHASE=pid1"

      active_system="$(readlink -f /run/current-system)"
      declared_system="$(sed -n 's/^system=//p' /etc/starry-nixos/provenance)"
      test "$active_system" = "$declared_system"
      test "$(cat /etc/hostname)" = "starrynixos"
      grep -q '^starry:' /etc/passwd
      test "$(hello)" = 'Hello, world!'
      test -f /etc/starry-nixos/provenance
      echo "STARRY_NIXOS_PHASE=activation"

      systemctl is-active --quiet multi-user.target
      echo "STARRY_NIXOS_PHASE=systemd"
      echo "STARRY_NIXOS_PHASE=marker"
      echo "STARRY_NIXOS_SYSTEM_PASSED"
      systemctl --force --force poweroff
    '';
  };

  systemd.timers.starry-nixos-marker = {
    description = "Start StarryNixOS verification after multi-user activation";
    wantedBy = [ "multi-user.target" ];
    timerConfig = {
      OnActiveSec = "1s";
      Unit = "starry-nixos-marker.service";
    };
  };

  system.nixos.label = "starry-nixos-stage2";
  system.stateVersion = "26.05";
}
