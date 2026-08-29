{ pkgs, ... }:
{
  systemd.services.starry-nixos-marker.onSuccess = [ "starry-nixos-service-assert-fail.service" ];

  systemd.services.starry-nixos-service-assert-fail = {
    description = "StarryNixOS declared failing service assertion";
    after = [ "starry-nixos-marker.service" ];
    serviceConfig = {
      Type = "oneshot";
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
    path = [
      pkgs.coreutils
    ];
    script = ''
      set -eu
      echo "STARRY_NIXOS_ASSERT_BEGIN"
      echo "STARRY_NIXOS_ASSERT_CMD=false"
      echo "STARRY_NIXOS_ASSERT_STATUS=1"
      echo "STARRY_NIXOS_ASSERT_OUTPUT_BEGIN"
      echo "STARRY_NIXOS_ASSERT_OUTPUT_END"
      echo "STARRY_NIXOS_ASSERT_FAILED:declared command false exited 1"
      systemctl --force --force poweroff
    '';
  };
}
