final: prev: {
  llamafarm-web = final.callPackage ./web/package.nix { };

  llamafarm = final.callPackage ./package.nix {
    rustToolchain = final.fenix.stable.withComponents [
      "cargo"
      "clippy"
      "rust-src"
      "rustc"
      "rustfmt"
    ];
  };
}
