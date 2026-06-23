{ self, pkgs, tier ? "ci" }:
let
  importTest = path: import path { inherit self pkgs; };

  unit = {
    vm-unit-module-smoke = importTest ./unit/module-smoke.nix;
    vm-unit-keep-awake-helper = importTest ./unit/keep-awake-helper.nix;
    vm-unit-keep-awake-local = importTest ./unit/keep-awake-local.nix;
  };

  integration = {
    vm-integration-proxy-wake-flow = importTest ./integration/proxy-wake-flow.nix;
    vm-integration-proxy-wake-ssh = importTest ./integration/proxy-wake-ssh.nix;
    vm-integration-firewall-sources = importTest ./integration/firewall-sources.nix;
    vm-integration-ssh-known-hosts = importTest ./integration/ssh-known-hosts.nix;
  };

  optional = {
    vm-optional-auto-suspend = importTest ./optional/auto-suspend.nix;
  };

  ci = unit // integration;
in
  if tier == "unit" then unit
  else if tier == "integration" then integration
  else if tier == "optional" then optional
  else if tier == "ci" then ci
  else ci // optional
