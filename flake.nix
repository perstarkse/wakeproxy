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
    in rec {
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

      wakeproxy-keep-awake-target = import ./nix/packages/keep-awake-target.nix {
        inherit pkgs;
      };
    });

    apps = forAllSystems (system: {
      default = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/wakeproxy";
        meta = self.packages.${system}.default.meta;
      };
    });

    checks = forAllSystems (system: let
      pkgs = import nixpkgs {inherit system;};
    in {
      keep-awake-target = import ./nix/tests/keep-awake-target.nix {
        inherit self pkgs;
      };
      deployment-e2e = import ./nix/tests/deployment-e2e.nix {
        inherit self pkgs;
      };
      auto-suspend-interaction = import ./nix/tests/auto-suspend-interaction.nix {
        inherit self pkgs;
      };
      keep-awake-debug = import ./nix/tests/keep-awake-debug.nix {
        inherit self pkgs;
      };
    });

    nixosModules = {
      default = self.nixosModules.wake-proxy;
      wake-proxy = import ./nix/modules/wake-proxy.nix {inherit self;};
      keep-awake-target = import ./nix/modules/keep-awake-target.nix {inherit self;};
    };
  };
}
