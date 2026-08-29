{ pkgs, ... }:
{
  systemd.services.starry-nixos-marker.onSuccess = [ "starry-nixos-service-assert.service" ];

  systemd.services.starry-nixos-service-assert = {
    description = "StarryNixOS declared service assertion";
    after = [ "starry-nixos-marker.service" ];
    serviceConfig = {
      Type = "oneshot";
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
    path = [
      pkgs.coreutils
      pkgs.hello
    ];
    script = ''
      set -eu
      cmd="hello"
      echo "STARRY_NIXOS_ASSERT_BEGIN"
      echo "STARRY_NIXOS_ASSERT_CMD=$cmd"
      output="$(hello)"
      status=0
      echo "STARRY_NIXOS_ASSERT_STATUS=$status"
      echo "STARRY_NIXOS_ASSERT_OUTPUT_BEGIN"
      printf '%s\n' "$output"
      echo "STARRY_NIXOS_ASSERT_OUTPUT_END"
      test -f /etc/starry-nixos/provenance
      test -f /etc/starry-nixos/keep-running
      test "$output" = "Hello, world!"
      echo "STARRY_NIXOS_ASSERT_PASSED"
      systemctl --force --force poweroff
    '';
  };
}
