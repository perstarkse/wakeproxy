{
  description = "wakeproxy: Wake-on-LAN authenticated reverse proxy";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = {
    self,
    nixpkgs,
    ...
  }: let
    systems = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    packages = forAllSystems (system: let
      pkgs = import nixpkgs {inherit system;};
    in {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "wakeproxy";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./Cargo.lock;

        doCheck = false;

        meta = {
          description = "wakeproxy: Wake-on-LAN authenticated reverse proxy";
          homepage = "https://github.com/perstarkse/wakeproxy";
          mainProgram = "wakeproxy";
          platforms = pkgs.lib.platforms.linux;
        };
      };
    });

    apps = forAllSystems (system: {
      default = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/wakeproxy";
      };
    });
  };
}
