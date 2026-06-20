{
  description = "wakeproxy: Wake-on-LAN authenticated reverse proxy";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    devenv.url = "github:cachix/devenv";
    devenv.inputs.nixpkgs.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    devenv,
    ...
  } @ inputs: let
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

    devShells = forAllSystems (system: let
      pkgs = import nixpkgs {inherit system;};
    in {
      default = devenv.lib.mkShell {
        inherit inputs pkgs;
        modules = [
          (
            {inputs, ...}: let
              fenixPkgs = inputs.fenix.packages.${pkgs.stdenv.hostPlatform.system};
              llvmTools = fenixPkgs.stable."llvm-tools-preview";
              rustTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget or pkgs.stdenv.hostPlatform.config;
              llvmToolsBin = "${llvmTools}/lib/rustlib/${rustTarget}/bin";
            in {
              devenv.warnOnNewVersion = false;

              packages = [
                pkgs.git
                pkgs.curl
                pkgs.cargo-llvm-cov
                llvmTools
              ];

              languages.rust.enable = true;

              env.LLVM_COV = "${llvmToolsBin}/llvm-cov";
              env.LLVM_PROFDATA = "${llvmToolsBin}/llvm-profdata";

              git-hooks.install.enable = true;
              git-hooks.hooks = {
                rustfmt.enable = true;
                "cargo-clippy" = {
                  enable = true;
                  name = "clippy";
                  entry = "cargo clippy --workspace --all-targets --all-features -- -D warnings";
                  language = "system";
                  pass_filenames = false;
                };
                "cargo-test" = {
                  enable = true;
                  name = "Cargo tests";
                  entry = "cargo test";
                  language = "system";
                  pass_filenames = false;
                };
                "cargo-coverage" = {
                  enable = true;
                  name = "Coverage summary";
                  entry = "cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 90";
                  language = "system";
                  pass_filenames = false;
                };
              };
            }
          )
        ];
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

      keep-awake-ssh = import ./nix/tests/keep-awake-ssh.nix {
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
