{
  description = "vesc-mcp — MCP server for VESC / vescpkg development";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils }:
    let
      packageFor = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          # The committed vector artifact records the repository model ID. The
          # ONNX file is quantized, but the variant tag is not part of the
          # artifact identity and caused runtime metadata validation to fail.
          semanticModelId = "Xenova/bge-small-en-v1.5";
          semanticModelRepository = "Xenova/bge-small-en-v1.5";
          semanticModelRevision = "ea104dacec62c0de699686887e3f920caeb4f3e3";
          semanticFeatures = "semantic-fastembed"
            + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ",semantic-coreml";
          semanticModel = pkgs.linkFarm "bge-small-en-v1.5" (map
            (file: {
              name = file.name;
              path = pkgs.fetchurl {
                url = "https://huggingface.co/${semanticModelRepository}/resolve/${semanticModelRevision}/${file.source}";
                inherit (file) hash;
              };
            })
            [
              { name = "model.onnx"; source = "onnx/model_quantized.onnx"; hash = "sha256-bJxhAalW1i37XnGQxTgibAxbucsntlEjS23wY+59v+Q="; }
              { name = "tokenizer.json"; source = "tokenizer.json"; hash = "sha256-0kGmDV6PBMwbKz6e96SSGye/Um2fYFCrkPkmeh+eXGY="; }
              { name = "config.json"; source = "config.json"; hash = "sha256-+nP5C/ksjKzh+8twliYwbyvbyeo+W1+UtEDfm2qlY1A="; }
              { name = "special_tokens_map.json"; source = "special_tokens_map.json"; hash = "sha256-ttNGvjZqfR1IMy28n987+JYLXYeVIrd5ndulnnYjfuM="; }
              { name = "tokenizer_config.json"; source = "tokenizer_config.json"; hash = "sha256-kmHn15tEyBlcHK2itFPlWwCuuB6QemZkl0tNd3YXKrM="; }
            ]);
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              craneLib.filterCargoSources path type
              || pkgs.lib.hasInfix "/crates/vesc-knowledge-index/generated" path
              || pkgs.lib.hasInfix "/crates/vesc-mcp-core/src/resources/snippets" path;
          };
          commonArgs = {
            pname = "vesc-mcp";
            version = "0.1.0";
            inherit src;
            strictDeps = true;
            cargoExtraArgs = "-p vesc-mcp-server --features ${semanticFeatures}";
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          doCheck = false;
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper pkgs.gzip ];
          postInstall = ''
            knowledge="$out/share/vesc-mcp/knowledge"
            mkdir -p "$knowledge/generations"
            gzip -dc ${./release/knowledge}/active.json.gz > "$knowledge/active.json"
            for source in ${./release/knowledge}/generations/*/lexical.json.gz; do
              generation="$(basename "$(dirname "$source")")"
              mkdir "$knowledge/generations/$generation"
              gzip -dc "$source" > "$knowledge/generations/$generation/lexical.json"
              if [ -f "$(dirname "$source")/vectors.bin.gz" ]; then
                gzip -dc "$(dirname "$source")/vectors.bin.gz" \
                  > "$knowledge/generations/$generation/vectors.bin"
              fi
            done
            test -s "$knowledge/active.json"
            test -s "$knowledge/generations/"*/lexical.json
            wrapProgram "$out/bin/vesc-mcp-server" \
              --set-default VESC_RAG_ARTIFACT "$knowledge" \
              --set-default VESC_RAG_MODE auto \
              --set-default VESC_RAG_SEMANTIC_MODEL_DIR "${semanticModel}" \
              --set-default VESC_RAG_SEMANTIC_MODEL_ID "${semanticModelId}" \
              --set-default VESC_RAG_SEMANTIC_MODEL_REVISION "${semanticModelRevision}" \
              --set-default VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS 300 \
              --set-default ORT_DYLIB_PATH "${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
          '';
          meta.mainProgram = "vesc-mcp-server";
        });

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
            VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS = toString cfg.semanticIdleTimeoutSecs;
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
              default = "auto";
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
            semanticIdleTimeoutSecs = lib.mkOption {
              type = lib.types.nonnegativeInt;
              default = 300;
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
        # Current ONNX Runtime releases build MIGraphX for AMD. The legacy
        # ROCm execution provider is no longer present in the upstream 1.26
        # source, so keep this name for the shell output while using nixpkgs'
        # supported AMD configuration.
        rocmOnnxruntime = if pkgs.stdenv.isLinux then
          pkgs.onnxruntime.override { rocmSupport = true; }
        else null;
      in {
        packages.default = packageFor system;
        packages.vesc-mcp = packageFor system;
        devShells = {
          default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain pkg-config openssl cargo-nextest cargo-llvm-cov cargo-deny
            cargo-audit clippy rustfmt jq hyperfine time onnxruntime
            python3Packages.onnx python3Packages.onnxruntime
          ]
          ++ lib.optionals stdenv.isLinux [
            perf
            rocmPackages.rocm-runtime
            rocmPackages.rocminfo
            vulkan-tools
          ];
          shellHook = ''
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
            export CARGO_TARGET_DIR="$PWD/target"
            export VESC_TIME_BIN="${pkgs.time}/bin/time"
            export ORT_DYLIB_PATH="${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
            echo "vesc-mcp dev shell (stable Rust; provider benchmark tools available)"
          '';
          };
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          rocm = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain pkg-config openssl cargo-nextest clippy rustfmt jq hyperfine time
              python3Packages.onnx python3Packages.onnxruntime rocmOnnxruntime
              rocmPackages.rocm-runtime rocmPackages.rocminfo
            ];
            shellHook = ''
              export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
              export CARGO_TARGET_DIR="$PWD/target"
              export VESC_TIME_BIN="${pkgs.time}/bin/time"
              export ORT_DYLIB_PATH="${rocmOnnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
              export ORT_MIGRAPHX_MODEL_CACHE_PATH="$PWD/target/provider-bench/migraphx-cache"
              mkdir -p "$ORT_MIGRAPHX_MODEL_CACHE_PATH"
              echo "vesc-mcp AMD shell; build with --features semantic-fastembed,semantic-migraphx"
            '';
          };
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
