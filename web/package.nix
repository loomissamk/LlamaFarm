{ buildNpmPackage, lib }:
buildNpmPackage {
  pname = "llamafarm-web";
  version = "0.1.0";

  src =
    let
      fs = lib.fileset;
    in
    fs.toSource {
      root = ./.;
      fileset = fs.unions [
        ./src
        ./index.html
        ./package.json
        ./package-lock.json
        ./tsconfig.json
        ./tsconfig.app.json
        ./tsconfig.node.json
        ./vite.config.ts
      ];
    };

  # package-lock.json gained @monaco-editor/react + monaco-editor for the IDE panel,
  # so the old pinned hash no longer matches. lib.fakeHash makes `nix build` fail with
  # the real hash in the error message — paste that value back in here.
  npmDepsHash = lib.fakeHash;

  installPhase = ''
    runHook preInstall
    cp -r dist $out
    runHook postInstall
  '';
}
