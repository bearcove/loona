{
  inputs = {
    flake-utils = { url = "github:numtide/flake-utils"; };
    nixpkgs = { url = "github:NixOS/nixpkgs/nixos-unstable"; };
    rust-overlay =
      {
        url = "github:oxalica/rust-overlay";
        inputs = {
          nixpkgs.follows = "nixpkgs";
          flake-utils.follows = "flake-utils";
        };
      };
    crane = {
      url = "github:ipetkov/crane";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };
  outputs =
    { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
      src = craneLib.cleanCargoSource (craneLib.path ./.);

      buildInputs = with pkgs; [
        pkgs.stdenv.cc.cc
        lld
      ];
      nativeBuildInputs = with pkgs; [
        rustToolchain
        clang
        curl
        libunwind
        perl
        ninja
        nasm
        go
      ]
      ++ lib.optionals pkgs.stdenv.isLinux [ autoPatchelfHook ]
      ++ lib.optionals pkgs.stdenv.isDarwin
        (with pkgs.darwin.apple_sdk.frameworks; [
          CoreFoundation
          CoreServices
          SystemConfiguration
          Security
        ]);
      commonArgs = {
        pname = "fluke";
        version = "latest";
        strictDeps = true;
        dontStrip = true;
        inherit src buildInputs nativeBuildInputs;
      };
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      bin = craneLib.buildPackage (commonArgs // {
        inherit cargoArtifacts;
      });
    in
    with pkgs;
    {
      packages = {
        inherit bin;
        default = bin;
      };
      devShells.default = mkShell {
        packages = with pkgs; [ clang lld just nixpkgs-fmt cargo-nextest libiconv cmake pkg-config curl ];
      };
    }
    );
}
