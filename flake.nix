{
  description = "wakeproxy: Wake-on-LAN authenticated reverse proxy";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    git-hooks.url = "github:cachix/git-hooks.nix";
    git-hooks.inputs.nixpkgs.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      imports = [
        ./nix/modules/context.nix
        ./nix/modules/packages.nix
        ./nix/modules/checks.nix
        ./nix/modules/formatter.nix
        ./nix/modules/dev-shell.nix
      ];

      flake.nixosModules = {
        default = self.nixosModules.wake-proxy;
        wake-proxy = import ./nix/modules/wake-proxy.nix { inherit self; };
        keep-awake-target = import ./nix/modules/keep-awake-target.nix { inherit self; };
      };
    };
}
