{
  description = "Pinned x86_64 StarryNixOS stage-2 rootfs";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      mkNixosSystem =
        modules:
        nixpkgs.lib.nixosSystem {
          inherit system modules;
        };
      mkRootfs =
        toplevel:
        let
          rootfsBase = pkgs.callPackage "${nixpkgs}/nixos/lib/make-ext4-fs.nix" {
            storePaths = [ toplevel ];
            volumeLabel = "STARRYNIXOS";
            populateImageCommands = ''
              mkdir -p ./files/etc/starry-nixos ./files/nix/var/nix/profiles
              ln -s ${toplevel}/init ./files/init
              ln -s ${toplevel} ./files/nix/var/nix/profiles/system
              cat > ./files/etc/starry-nixos/provenance <<EOF
              architecture=${system}
              system=${toplevel}
              EOF
            '';
          };
        in
        rootfsBase.overrideAttrs (previous: {
          # nixpkgs' ext4 helper estimates inode overhead, but this minimal NixOS
          # closure has more small files than mke2fs' default inode ratio permits.
          buildCommand = builtins.replaceStrings
            [ "bytes=$((2 * 4096 * $numInodes + 4096 * $numDataBlocks))" ]
            [ "bytes=$((3 * 4096 * $numInodes + 4096 * $numDataBlocks))" ]
            previous.buildCommand;
        });
      nixos = mkNixosSystem [ ./configuration.nix ];
      toplevel = nixos.config.system.build.toplevel;
      rootfs = mkRootfs toplevel;
      starryNixosInterface = {
        inherit mkNixosSystem mkRootfs;
        systemModule = ./configuration.nix;
        qemuConfig = ./qemu-x86_64.toml;
      };
    in
    {
      packages.${system} = {
        inherit rootfs;
        systemd = nixos.config.systemd.package;
        system = toplevel;
        default = rootfs;
      };
      lib.${system}.starryNixos = starryNixosInterface;
    };
}
