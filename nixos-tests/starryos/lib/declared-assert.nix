{
  pkgs,
  lib,
  command,
  expectPass,
  packages ? [ ],
}:
let
  quotedCommand = lib.escapeShellArg command;
in
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
    path = [ pkgs.coreutils ] ++ packages;
    script = ''
      set +e
      cmd=${quotedCommand}
      echo "STARRY_NIXOS_ASSERT_BEGIN"
      echo "STARRY_NIXOS_ASSERT_CMD=$cmd"
      output="$(eval "$cmd" 2>&1)"
      status=$?
      set -e
      echo "STARRY_NIXOS_ASSERT_STATUS=$status"
      echo "STARRY_NIXOS_ASSERT_OUTPUT_BEGIN"
      printf '%s\n' "$output"
      echo "STARRY_NIXOS_ASSERT_OUTPUT_END"
      ${
        if expectPass then
          ''
            if [ "$status" -ne 0 ]; then
              echo "STARRY_NIXOS_ASSERT_FAILED:declared command $cmd exited $status"
            else
              echo "STARRY_NIXOS_ASSERT_PASSED"
            fi
          ''
        else
          ''
            if [ "$status" -eq 0 ]; then
              echo "STARRY_NIXOS_ASSERT_PASSED"
            else
              echo "STARRY_NIXOS_ASSERT_FAILED:declared command $cmd exited $status"
            fi
          ''
      }
      systemctl --force --force poweroff
    '';
  };
}
