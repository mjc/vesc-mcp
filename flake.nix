{
  description = "vesc-mcp — MCP server for VESC / vescpkg development";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
      flake-utils,
    }:
    let
      packageFor =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          semanticModelId = "jinaai/jina-embeddings-v2-base-code";
          semanticModelRepository = semanticModelId;
          semanticModelRevision = "516f4baf13dec4ddddda8631e019b5737c8bc250";
          semanticFeatures =
            "semantic-fastembed" + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ",semantic-coreml";
          semanticModel = pkgs.linkFarm "jina-embeddings-v2-base-code-quantized" (
            map
              (file: {
                name = file.name;
                path = pkgs.fetchurl {
                  url = "https://huggingface.co/${semanticModelRepository}/resolve/${semanticModelRevision}/${file.source}";
                  inherit (file) hash;
                };
              })
              [
                {
                  name = "model.onnx";
                  source = "onnx/model_quantized.onnx";
                  hash = "sha256-7UWHAlHJ8M9lbniqsNN6I0iQZt+KIiuxyMr4pF8ssW0=";
                }
                {
                  name = "tokenizer.json";
                  source = "tokenizer.json";
                  hash = "sha256-sBx4qQKqT6yy9H+VRJ9I4ve7/qXSRy7i9s6SMjxvhuU=";
                }
                {
                  name = "config.json";
                  source = "config.json";
                  hash = "sha256-5CaqaEx/mpXF8CCqhV+vk6JPBl9frQyeF7EkZwyr3qY=";
                }
                {
                  name = "special_tokens_map.json";
                  source = "special_tokens_map.json";
                  hash = "sha256-BuQFo23+S5YE9IT2oeYZrxp/fQnjSoVV6wt3tmMYBn8=";
                }
                {
                  name = "tokenizer_config.json";
                  source = "tokenizer_config.json";
                  hash = "sha256-9HeusV/59408Hd8jYdKwuLIM9VIg+DnymjfzoY793Yk=";
                }
              ]
          );
          legacySemanticModel = pkgs.linkFarm "bge-small-en-v1.5-quantized" (
            map
              (file: {
                name = file.name;
                path = pkgs.fetchurl {
                  url = "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/ea104dacec62c0de699686887e3f920caeb4f3e3/${file.source}";
                  inherit (file) hash;
                };
              })
              [
                {
                  name = "model.onnx";
                  source = "onnx/model_quantized.onnx";
                  hash = "sha256-bJxhAalW1i37XnGQxTgibAxbucsntlEjS23wY+59v+Q=";
                }
                {
                  name = "tokenizer.json";
                  source = "tokenizer.json";
                  hash = "sha256-0kGmDV6PBMwbKz6e96SSGye/Um2fYFCrkPkmeh+eXGY=";
                }
                {
                  name = "config.json";
                  source = "config.json";
                  hash = "sha256-+nP5C/ksjKzh+8twliYwbyvbyeo+W1+UtEDfm2qlY1A=";
                }
                {
                  name = "special_tokens_map.json";
                  source = "special_tokens_map.json";
                  hash = "sha256-ttNGvjZqfR1IMy28n987+JYLXYeVIrd5ndulnnYjfuM=";
                }
                {
                  name = "tokenizer_config.json";
                  source = "tokenizer_config.json";
                  hash = "sha256-kmHn15tEyBlcHK2itFPlWwCuuB6QemZkl0tNd3YXKrM=";
                }
              ]
          );
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              craneLib.filterCargoSources path type
              || pkgs.lib.hasSuffix "/docs/vesc-pkg-lib-abi.md" path
              || pkgs.lib.hasInfix "/crates/vesc-knowledge-index/generated" path
              || pkgs.lib.hasInfix "/crates/vesc-mcp-core/src/resources/snippets" path
              || pkgs.lib.hasInfix "/vendor/fastembed/src/sparse_text_embedding/weights" path;
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
        in
        craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
              pkgs.makeWrapper
              pkgs.gzip
            ];
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
              mkdir -p "$out/share/vesc-mcp/models"
              ln -s ${./catalog} "$out/share/vesc-mcp/catalog"
              ln -s ${legacySemanticModel} "$out/share/vesc-mcp/models/bge-small-en-v1.5-quantized"
              wrapProgram "$out/bin/vesc-mcp-server" \
                --set-default VESC_MCP_WORKSPACE_ROOT "$out/share/vesc-mcp" \
                --set-default VESC_RAG_ARTIFACT "$knowledge" \
                --set-default VESC_RAG_MODE auto \
                --set-default VESC_RAG_SEMANTIC_MODEL_DIR "${semanticModel}" \
                --set-default VESC_RAG_SEMANTIC_MODEL_ID "${semanticModelId}" \
                --set-default VESC_RAG_SEMANTIC_MODEL_REVISION "${semanticModelRevision}" \
                --set-default VESC_RAG_SEMANTIC_MAX_LENGTH 512 \
                --set-default VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS 300 \
                --set-default ORT_DYLIB_PATH "${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
            '';
            meta.mainProgram = "vesc-mcp-server";
          }
        );

      nixosModule = import ./nix/module.nix {
        defaultPackage = pkgs: self.packages.${pkgs.system}.default;
      };
    in
    (flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "llvm-tools-preview"
          ];
        };
        ciRustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "clippy"
            "rustfmt"
            "llvm-tools-preview"
          ];
        };
        rustShellHook = toolchain: ''
          export RUST_SRC_PATH="${toolchain}/lib/rustlib/src/rust/library"
          export CARGO_TARGET_DIR="$PWD/target"
          export VESC_GIT_BIN="${pkgs.git}/bin/git"
          export VESC_TIME_BIN="${pkgs.time}/bin/time"
        '';
        # Current ONNX Runtime releases build MIGraphX for AMD. The legacy
        # ROCm execution provider is no longer present in the upstream 1.26
        # source, so keep this name for the shell output while using nixpkgs'
        # supported AMD configuration.
        rocmOnnxruntime =
          if pkgs.stdenv.isLinux then pkgs.onnxruntime.override { rocmSupport = true; } else null;
      in
      {
        packages.default = packageFor system;
        packages.vesc-mcp = packageFor system;
        devShells = {
          ci = pkgs.mkShell {
            packages = with pkgs; [
              ciRustToolchain
              pkg-config
              openssl
              cargo-nextest
              cargo-llvm-cov
              git
              time
              python3
            ];
            shellHook = rustShellHook ciRustToolchain;
          };
          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                rustToolchain
                pkg-config
                openssl
                cargo-nextest
                cargo-llvm-cov
                cargo-deny
                cargo-audit
                clippy
                rustfmt
                git
                jq
                hyperfine
                time
                onnxruntime
                python3Packages.onnx
                python3Packages.onnxruntime
              ]
              ++ lib.optionals stdenv.isLinux [
                heaptrack
                perf
                rocmPackages.rocm-runtime
                rocmPackages.rocminfo
                vulkan-tools
              ];
            shellHook = rustShellHook rustToolchain + ''
              export ORT_DYLIB_PATH="${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
              echo "vesc-mcp dev shell (stable Rust; provider benchmark tools available)" >&2
            '';
          };
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          rocm = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              pkg-config
              openssl
              cargo-nextest
              clippy
              rustfmt
              git
              jq
              hyperfine
              time
              heaptrack
              python3Packages.onnx
              python3Packages.onnxruntime
              rocmOnnxruntime
              rocmPackages.rocm-runtime
              rocmPackages.rocminfo
            ];
            shellHook = rustShellHook rustToolchain + ''
              export ORT_DYLIB_PATH="${rocmOnnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
              export ORT_MIGRAPHX_MODEL_CACHE_PATH="$PWD/target/provider-bench/migraphx-cache"
              mkdir -p "$ORT_MIGRAPHX_MODEL_CACHE_PATH"
              echo "vesc-mcp AMD shell; build with --features semantic-fastembed,semantic-migraphx" >&2
            '';
          };
        };
        formatter = pkgs.rustfmt;
        checks.nixos-module = import ./nix/module-test.nix {
          inherit
            nixpkgs
            system
            pkgs
            nixosModule
            ;
        };
      }
    ))
    // {
      overlays.default = final: _prev: {
        vesc-mcp = packageFor final.system;
      };
      nixosModules.default = nixosModule;
    };
}
