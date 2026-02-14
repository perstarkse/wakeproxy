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
  # https://devenv.sh/basics/
  env.GREET = "devenv";

  # https://devenv.sh/packages/
  packages = [
    pkgs.git
    pkgs.cargo-llvm-cov
    llvmTools
  ];

  # https://devenv.sh/languages/
  languages.rust.enable = true;

  env.LLVM_COV = "${llvmToolsBin}/llvm-cov";
  env.LLVM_PROFDATA = "${llvmToolsBin}/llvm-profdata";

  # https://devenv.sh/processes/
  # processes.dev.exec = "${lib.getExe pkgs.watchexec} -n -- ls -la";

  # https://devenv.sh/services/
  # services.postgres.enable = true;

  # https://devenv.sh/scripts/
  scripts.hello.exec = ''
    echo hello from $GREET
  '';

  scripts.ci.exec = ''
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test
    cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 90
  '';

  # https://devenv.sh/basics/
  enterShell = ''
    hello         # Run scripts directly
    git --version # Use packages
  '';

  # https://devenv.sh/tasks/
  # tasks = {
  #   "myproj:setup".exec = "mytool build";
  #   "devenv:enterShell".after = [ "myproj:setup" ];
  # };

  # https://devenv.sh/tests/
  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
    ci
  '';

  # https://devenv.sh/git-hooks/
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
  };

  # See full reference at https://devenv.sh/reference/options/
}
