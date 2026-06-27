{
  description = "vesc-mcp — MCP server for VESC / vescpkg development";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
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

        devTools = with pkgs; [
          rustToolchain
          pkg-config
          cargo-nextest
          cargo-deny
          cargo-audit
          clippy
          rustfmt
          jq
        ];
      in {
        devShells.default = pkgs.mkShell {
          packages = devTools;

          shellHook = ''
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
            export CARGO_TARGET_DIR="$PWD/target"
            echo "vesc-mcp dev shell (host-only, stable Rust)"
          '';
        };

        formatter = pkgs.rustfmt;
      });
}
