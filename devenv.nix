{
  pkgs,
  lib,
  config,
  inputs,
  ...
}: let
  fenixPkgs = inputs.fenix.packages.${pkgs.stdenv.hostPlatform.system};
  llvmTools = fenixPkgs.stable."llvm-tools-preview";
  rustTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget or pkgs.stdenv.hostPlatform.config;
  llvmToolsBin = "${llvmTools}/lib/rustlib/${rustTarget}/bin";
in {
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
    clippy = {
      enable = true;
      settings.allFeatures = true;
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
    "e2e-local" = {
      enable = true;
      name = "Local E2E smoke";
      entry = "./scripts/e2e-local.sh";
      language = "system";
      pass_filenames = false;
      stages = ["pre-commit"];
    };
  };
}
