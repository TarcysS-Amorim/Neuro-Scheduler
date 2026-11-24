{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ rustc cargo rustfmt clippy rust-analyzer pkg-config sqlite git ];
  RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
}
