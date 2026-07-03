{
  description = "Fix unbalanced parentheses in Lisp code using parinfer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix/monthly";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      fenix,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # A recent toolchain for Rust edition 2024
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        src = craneLib.path ./.;

        commonArgs = {
          inherit src;
          pname = "lisp-balance";
          version = "0.1.0";
          strictDeps = true;
        };

        # Build just the cargo dependencies so caching works across builds
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        lisp-balance = craneLib.buildPackage commonArgs;
      in
      {
        packages.default = lisp-balance;

        apps.default = {
          type = "app";
          program = "${lisp-balance}/bin/lisp-balance";
        };

        devShells.default = craneLib.devShell { };
      }
    );
}
