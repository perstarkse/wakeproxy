{
  self,
  lib,
  ...
}:
{
  perSystem =
    { pkgs, ... }:
    let
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "wakeproxy";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./../../Cargo.lock;
        doCheck = false;
        meta = {
          description = "wakeproxy: Wake-on-LAN authenticated reverse proxy";
          homepage = "https://github.com/perstarkse/wakeproxy";
          mainProgram = "wakeproxy";
          platforms = lib.platforms.linux;
        };
      };
    in
    {
      packages = {
        inherit default;
        wakeproxy-keep-awake-target = import ../packages/keep-awake-target.nix { inherit pkgs; };
        release-archive = import ../packages/release-archive.nix {
          inherit pkgs;
          wakeproxy = default;
        };
      };
      apps.default = {
        type = "app";
        program = "${default}/bin/wakeproxy";
        meta.description = "Run the wakeproxy reverse proxy";
      };
    };
}
