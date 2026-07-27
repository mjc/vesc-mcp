{
  description = "vesc-mcp — MCP server for VESC / vescpkg development";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    rust-overlay,
    flake-utils,
  }: let
    migraphxOnnxruntime = pkgs:
      (pkgs.onnxruntime.override {rocmSupport = true;}).overrideAttrs {
        src = pkgs.fetchFromGitHub {
          owner = "mjc";
          repo = "onnxruntime";
          rev = "c9ef1ac8df0ea0c1fb950eb1972fbcd6dfe9d1d8";
          hash = "sha256-ziwXPFF1kBaIStZhfl4VG2HkRivyb0Vg2/n0dx8iB94=";
        };
      };
    packageFor = system: accelerated: profiling: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      rustToolchain = pkgs.rust-bin.stable."1.97.1".default;
      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
      semanticModelId = "jinaai/jina-embeddings-v2-base-code";
      semanticModelRepository = semanticModelId;
      semanticModelRevision = "516f4baf13dec4ddddda8631e019b5737c8bc250";
      semanticFeatures =
        "semantic-fastembed"
        + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ",semantic-coreml"
        + pkgs.lib.optionalString accelerated ",semantic-migraphx"
        + pkgs.lib.optionalString profiling ",coz-profile";
      semanticRuntime =
        if accelerated
        then migraphxOnnxruntime pkgs
        else pkgs.onnxruntime;
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
      semanticIngestionModel = pkgs.linkFarm "jina-embeddings-v2-base-code-fp16" [
        {
          name = "model.onnx";
          path = pkgs.fetchurl {
            url = "https://huggingface.co/${semanticModelRepository}/resolve/${semanticModelRevision}/onnx/model_fp16.onnx";
            hash = "sha256-Gq/E/NY9LmiZ6IQC/3MefGRsLkNQSClKPLyQikDUXXw=";
          };
        }
        {
          name = "tokenizer.json";
          path = "${semanticModel}/tokenizer.json";
        }
        {
          name = "config.json";
          path = "${semanticModel}/config.json";
        }
        {
          name = "special_tokens_map.json";
          path = "${semanticModel}/special_tokens_map.json";
        }
        {
          name = "tokenizer_config.json";
          path = "${semanticModel}/tokenizer_config.json";
        }
      ];
      src = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          craneLib.filterCargoSources path type
          || pkgs.lib.hasSuffix "/docs/vesc-pkg-lib-abi.md" path
          || pkgs.lib.hasInfix "/crates/vesc-knowledge-index/generated" path
          || pkgs.lib.hasInfix "/crates/vesc-mcp-core/src/resources/snippets" path
          || pkgs.lib.hasInfix "/vendor/fastembed/src/sparse_text_embedding/weights" path;
      };
      commonArgs =
        {
          pname = "vesc-mcp";
          version = "0.1.0";
          inherit src;
          strictDeps = true;
          cargoExtraArgs = "-p vesc-mcp-server --features ${semanticFeatures}";
          nativeBuildInputs = [pkgs.pkg-config];
          doCheck = false;
        }
        // pkgs.lib.optionalAttrs profiling {
          CARGO_BUILD_JOBS = "1";
          CARGO_BUILD_RUSTFLAGS = "-C force-frame-pointers=yes";
          CARGO_PROFILE_RELEASE_DEBUG = "line-tables-only";
          CARGO_PROFILE_RELEASE_STRIP = "none";
          dontStrip = true;
        };
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;
    in
      craneLib.buildPackage (
        commonArgs
        // {
          inherit cargoArtifacts;
          nativeBuildInputs =
            commonArgs.nativeBuildInputs
            ++ [
              pkgs.makeWrapper
            ];
          postInstall = ''
            mkdir -p "$out/share/vesc-mcp"
            ln -s ${./catalog} "$out/share/vesc-mcp/catalog"
            wrapProgram "$out/bin/vesc-mcp-server" \
              --set-default VESC_MCP_WORKSPACE_ROOT "$out/share/vesc-mcp" \
              --set-default VESC_RAG_MODE auto \
              --set-default VESC_RAG_SEMANTIC_MODEL_DIR "${semanticModel}" \
              --set-default VESC_RAG_SEMANTIC_MODEL_ID "${semanticModelId}" \
              --set-default VESC_RAG_SEMANTIC_MODEL_REVISION "${semanticModelRevision}" \
              --set-default VESC_RAG_SEMANTIC_MAX_LENGTH 512 \
              --set-default VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS 300 \
              ${pkgs.lib.optionalString accelerated ''
              --set-default VESC_RAG_SEMANTIC_INGEST_MODEL_DIR "${semanticIngestionModel}" \
              --set-default VESC_RAG_SEMANTIC_INGEST_MODEL_SHA256 "1aafc4fcd63d2e6899e88402ff731e7c646c2e435048294a3cbc908a40d45d7c" \
              --set-default VESC_RAG_SEMANTIC_INGEST_PROVIDER migraphx \
              --set-default VESC_RAG_SEMANTIC_INGEST_DEVICE_ID 0 \
              --set-default VESC_RAG_SEMANTIC_INGEST_MAX_LENGTH 64 \
              --set-default VESC_RAG_SEMANTIC_INGEST_BATCH_SIZE 64 \
              --set-default VESC_RAG_SEMANTIC_INGEST_WINDOW_AGGREGATION token_weighted_mean \
              --prefix LD_LIBRARY_PATH : "${pkgs.rocmPackages.migraphx}/lib/migraphx/lib" \
            ''}--set-default ORT_DYLIB_PATH "${semanticRuntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
          '';
          meta.mainProgram = "vesc-mcp-server";
        }
      );

    nixosModule = import ./nix/module.nix {
      defaultPackage = pkgs: self.packages.${pkgs.system}.default;
    };
  in
    (flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {inherit system overlays;};
        rustToolchain = pkgs.rust-bin.stable."1.97.1".default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "llvm-tools-preview"
            "clippy"
            "rustfmt"
          ];
        };
        ciRustToolchain = pkgs.rust-bin.stable."1.97.1".default.override {
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
        gungraunRunner = pkgs.rustPlatform.buildRustPackage rec {
          pname = "gungraun-runner";
          version = "0.19.4";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-DrIbeUVI+fhrp87rzIxYRvAlPSJ3ksa6cHHNFg4I+zE=";
          };
          cargoLock.lockFile = "${src}/Cargo.lock";
          doCheck = false;
        };
        # Current ONNX Runtime releases build MIGraphX for AMD. The ROCm
        # execution provider is no longer present in the upstream 1.26
        # source, so keep this name for the shell output while using nixpkgs'
        # supported AMD configuration.
        rocmOnnxruntime =
          if pkgs.stdenv.isLinux
          then migraphxOnnxruntime pkgs
          else null;
      in {
        packages =
          {
            default = packageFor system false false;
            vesc-mcp = packageFor system false false;
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            vesc-mcp-migraphx = packageFor system true false;
            vesc-mcp-migraphx-profile = packageFor system true true;
          };
        devShells =
          {
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
              packages = with pkgs;
                [
                  rustToolchain
                  pkg-config
                  openssl
                  cargo-nextest
                  cargo-llvm-cov
                  cargo-deny
                  cargo-audit
                  git
                  jq
                  hyperfine
                  time
                  onnxruntime
                  python3Packages.onnx
                  python3Packages.onnxruntime
                ]
                ++ lib.optionals stdenv.isLinux [
                  coz
                  gungraunRunner
                  heaptrack
                  inferno
                  perf
                  procps
                  rocmPackages.rocm-runtime
                  rocmPackages.rocminfo
                  valgrind
                  vulkan-tools
                  zstd
                ];
              shellHook =
                rustShellHook rustToolchain
                + ''
                  export ORT_DYLIB_PATH="${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
                  echo "vesc-mcp dev shell (Rust 1.97.1; provider benchmark tools available)" >&2
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
                git
                jq
                hyperfine
                time
                coz
                gungraunRunner
                heaptrack
                procps
                python3Packages.onnx
                python3Packages.onnxruntime
                rocmOnnxruntime
                rocmPackages.rocm-runtime
                rocmPackages.rocminfo
                valgrind
                zstd
              ];
              shellHook =
                rustShellHook rustToolchain
                + ''
                  export ORT_DYLIB_PATH="${rocmOnnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
                  export ORT_MIGRAPHX_MODEL_CACHE_PATH="$PWD/target/provider-bench/migraphx-cache"
                  mkdir -p "$ORT_MIGRAPHX_MODEL_CACHE_PATH"
                  echo "vesc-mcp AMD shell; build with --features semantic-fastembed,semantic-migraphx" >&2
                '';
            };
          };
        formatter = rustToolchain;
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
        vesc-mcp = packageFor final.system false false;
      };
      nixosModules.default = nixosModule;
    };
}
