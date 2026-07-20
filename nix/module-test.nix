{
  nixpkgs,
  system,
  pkgs,
  nixosModule,
}:
let
  testPackage = pkgs.runCommand "vesc-mcp-test-package" { } ''
    mkdir -p "$out/bin"
    touch "$out/bin/vesc-mcp-server"
  '';
  evaluate = settings:
    nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        nixosModule
        {
          services.vesc-mcp = {
            enable = true;
            package = testPackage;
          }
          // settings;
        }
      ];
    };
  repository = {
    url = "https://github.com/vedderb/bldc.git";
    defaultRef = "refs/heads/master";
    include = [ "**/*.c" ];
    license = "GPL-3.0-or-later";
    attribution = "VESC Project";
  };
  evaluated = evaluate {
    repositories = {
      bldc = repository;
      vesc_tool = repository // {
        url = "https://github.com/vedderb/vesc_tool.git";
        include = [ "**/*.cpp" ];
      };
      refloat = repository // {
        url = "https://github.com/lukash/refloat.git";
        defaultRef = "refs/heads/main";
        required = false;
        include = [ "**/*.lisp" ];
        trustTier = "community";
        attribution = "VESC contributors";
      };
    };
    defaultVersions.bldc = "refs/heads/release_6_06";
    prewarm = [
      {
        bldc = "refs/heads/release_6_05";
        vesc_tool = "refs/heads/release_6_05";
        refloat = "refs/tags/v1.2.3";
      }
    ];
    startup.timeoutSecs = 600;
  };
  defaults = (evaluate { }).config.systemd.services.vesc-mcp;
  service = evaluated.config.systemd.services.vesc-mcp;
  strictLazy = (evaluate {
    startup = {
      refresh = false;
      eagerIndex = false;
      allowOfflineRestart = false;
    };
  }).config.systemd.services.vesc-mcp;
  rejects = settings:
    builtins.tryEval (toString (evaluate settings).config.systemd.services.vesc-mcp.environment.VESC_MCP_CONFIG);
  invalidId = rejects { repositories."Bad/ID" = repository; };
  invalidUrl = rejects { repositories.bldc = repository // { url = "ssh://git@example.com/bldc.git"; }; };
  invalidRef = rejects { repositories.bldc = repository // { defaultRef = "master"; }; };
in
assert !invalidId.success;
assert !invalidUrl.success;
assert !invalidRef.success;
pkgs.runCommand "vesc-mcp-nixos-module-smoke" { } ''
  test "${defaults.serviceConfig.ExecStart}" = "${testPackage}/bin/vesc-mcp-server --http"
  test "${toString defaults.serviceConfig.TimeoutStartSec}" = "900"
  test "${strictLazy.serviceConfig.ExecStart}" = "${testPackage}/bin/vesc-mcp-server --http --skip-repository-refresh --skip-eager-index --require-fresh-repositories"
  test "${nixpkgs.lib.boolToString service.serviceConfig.DynamicUser}" = "true"
  test "${service.serviceConfig.StateDirectory}" = "vesc-mcp"
  test "${service.serviceConfig.CacheDirectory}" = "vesc-mcp"
  test "${toString service.serviceConfig.TimeoutStartSec}" = "600"
  config_file="${service.environment.VESC_MCP_CONFIG}"
  grep -F 'data_root = "/var/lib/vesc-mcp"' "$config_file"
  grep -F 'id = "bldc"' "$config_file"
  grep -F 'id = "vesc_tool"' "$config_file"
  grep -F 'id = "refloat"' "$config_file"
  grep -F 'default_ref = "refs/heads/release_6_06"' "$config_file"
  grep -F 'refloat = "refs/tags/v1.2.3"' "$config_file"
  touch "$out"
''
