{
  description = "vesc-mcp — MCP server for VESC / vescpkg development";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    let
      packageFor = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
        in pkgs.rustPlatform.buildRustPackage {
          pname = "vesc-mcp";
          version = "0.1.0";
          # Include local workspace modules even when they are not tracked yet.
          src = builtins.path {
            path = ./.;
            name = "vesc-mcp-source";
          };
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "vesc-mcp-server" ];
          doCheck = false;
          nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper pkgs.gzip ];
          postInstall = ''
            knowledge="$out/share/vesc-mcp/knowledge"
            mkdir -p "$knowledge"
            cp -R ${./release/knowledge}/. "$knowledge/"
            gzip -dc "$knowledge/active.json.gz" > "$knowledge/active.json"
            rm "$knowledge/active.json.gz"
            gzip -d "$knowledge"/generations/*/lexical.json.gz
            test -s "$knowledge/active.json"
            test -s "$knowledge/generations/"*/lexical.json
            wrapProgram "$out/bin/vesc-mcp-server" \
              --set-default VESC_RAG_ARTIFACT "$knowledge"
          '';
          meta.mainProgram = "vesc-mcp-server";
        };

      nixosModule = { config, lib, pkgs, ... }:
        let
          cfg = config.services.vesc-mcp;
          package = if cfg.package != null then cfg.package else self.packages.${pkgs.system}.default;
          httpBind = "${cfg.bind}:${toString cfg.port}";
          environment = {
            VESC_MCP_HTTP_BIND = httpBind;
            VESC_MCP_HTTP_PATH = cfg.path;
            VESC_MCP_HTTP_ALLOWED_HOSTS = lib.concatStringsSep "," cfg.allowedHosts;
            VESC_MCP_HTTP_ALLOWED_ORIGINS = lib.concatStringsSep "," cfg.allowedOrigins;
            VESC_RAG_MODE = cfg.retrievalMode;
            VESC_PACKAGE_ROOTS = lib.concatStringsSep ":" (map toString cfg.packageRoots);
          } // lib.optionalAttrs (cfg.artifactPath != null) {
            VESC_RAG_ARTIFACT = toString cfg.artifactPath;
          } // lib.optionalAttrs (cfg.semanticModelDir != null) {
            VESC_RAG_SEMANTIC_MODEL_DIR = toString cfg.semanticModelDir;
          } // lib.optionalAttrs (cfg.semanticModelId != null) {
            VESC_RAG_SEMANTIC_MODEL_ID = cfg.semanticModelId;
          } // lib.optionalAttrs (cfg.semanticModelRevision != null) {
            VESC_RAG_SEMANTIC_MODEL_REVISION = cfg.semanticModelRevision;
          };
        in {
          options.services.vesc-mcp = {
            enable = lib.mkEnableOption "the shared VESC MCP Streamable HTTP service";
            package = lib.mkOption {
              type = lib.types.nullOr lib.types.package;
              default = null;
              description = "vesc-mcp package to run.";
            };
            bind = lib.mkOption {
              type = lib.types.str;
              default = "127.0.0.1";
              description = "Listen address. Keep the local default unless remote exposure is intentional.";
            };
            port = lib.mkOption {
              type = lib.types.port;
              default = 8080;
            };
            path = lib.mkOption {
              type = lib.types.strMatching "^/";
              default = "/mcp";
            };
            allowedHosts = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ "localhost" "127.0.0.1" "::1" ];
              description = "Host authorities accepted by rmcp's DNS-rebinding protection.";
            };
            allowedOrigins = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
            };
            authTokenFile = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              description = "EnvironmentFile containing VESC_MCP_HTTP_AUTH_TOKEN for bearer auth.";
            };
            packageRoots = lib.mkOption {
              type = lib.types.listOf lib.types.path;
              default = [ ];
              description = "Roots reserved for future authenticated package-tool policy; HTTP exposes knowledge tools only.";
            };
            artifactPath = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
            };
            retrievalMode = lib.mkOption {
              type = lib.types.enum [ "lexical" "legacy" "auto" "hybrid" ];
              default = "lexical";
            };
            semanticModelDir = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
            };
            semanticModelId = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
            };
            semanticModelRevision = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.vesc-mcp = {
              description = "Shared VESC MCP Streamable HTTP service";
              wantedBy = [ "multi-user.target" ];
              wants = [ "network-online.target" ];
              after = [ "network-online.target" ];
              environment = environment;
              serviceConfig = {
                ExecStart = "${package}/bin/vesc-mcp-server --http";
                Restart = "on-failure";
                RestartSec = 2;
                DynamicUser = true;
                StateDirectory = "vesc-mcp";
                CacheDirectory = "vesc-mcp";
                NoNewPrivileges = true;
                PrivateTmp = true;
                ProtectSystem = "strict";
                ProtectHome = "read-only";
                RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
                EnvironmentFile = lib.optional (cfg.authTokenFile != null) cfg.authTokenFile;
              };
            };
          };
        };
    in (flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "llvm-tools-preview" ];
        };
      in {
        packages.default = packageFor system;
        packages.vesc-mcp = packageFor system;
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain pkg-config cargo-nextest cargo-llvm-cov cargo-deny
            cargo-audit clippy rustfmt jq
          ];
          shellHook = ''
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
            export CARGO_TARGET_DIR="$PWD/target"
            echo "vesc-mcp dev shell (host-only, stable Rust)"
          '';
        };
        formatter = pkgs.rustfmt;
        checks.nixos-module =
          let
            evaluated = nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                nixosModule
                ({ ... }: {
                  services.vesc-mcp.enable = true;
                  services.vesc-mcp.package = packageFor system;
                })
              ];
            };
          in pkgs.runCommand "vesc-mcp-nixos-module-smoke" { }
            ''
              test "${evaluated.config.systemd.services.vesc-mcp.serviceConfig.ExecStart}" = "${packageFor system}/bin/vesc-mcp-server --http"
              test "${nixpkgs.lib.boolToString evaluated.config.systemd.services.vesc-mcp.serviceConfig.DynamicUser}" = "true"
              touch "$out"
            '';
      })) // {
        overlays.default = final: _prev: {
          vesc-mcp = packageFor final.system;
        };
        nixosModules.default = nixosModule;
      };
}
