# Crane-based derivation for the loku-web binary.
# Called from flake.nix with: import ./web.nix { inherit craneLib commonArgs pkgs; }
#
# The package output includes:
#   $out/bin/loku-web                 — the server binary
#   $out/share/loku-web/frontend/     — compiled Elm frontend assets
#
# The NixOS module passes --frontend-path pointing at the share directory.
#
# Elm build strategy — two phases to keep compilation offline and hermetic:
#
#   1. elmPackagesCache (fixed-output derivation): network access is allowed.
#      Downloads all packages listed in elm.json into an ELM_HOME directory.
#      The outputHash locks the exact set of packages.  Update it by running
#        nix build .#web 2>&1 | grep "got:"
#      whenever elm.json dependency versions change.
#
#   2. elmFrontend (standard derivation): no network access.
#      Compiles src/Main.elm to elm.js using the cached packages from phase 1.
#      Rebuilds whenever any Elm source file changes.
{ craneLib, commonArgs, pkgs }:
let
  elmPackagesCache = pkgs.stdenv.mkDerivation {
    name = "loku-elm-packages";
    src = ../../frontend;
    nativeBuildInputs = [ pkgs.elmPackages.elm ];

    outputHash = "sha256-QloSJ8mwM+U18XBSMmE/lxWzEgO1gIG/rng67ls23ME=";
    outputHashAlgo = "sha256";
    outputHashMode = "recursive";

    buildPhase = ''
      export HOME=$TMPDIR/elm-home
      mkdir -p $HOME
      elm make src/Main.elm --output=/dev/null
      cp -r $HOME/.elm/. $out/
    '';
    installPhase = "true";
  };

  elmFrontend = pkgs.stdenv.mkDerivation {
    name = "loku-elm-frontend";
    src = ../../frontend;
    nativeBuildInputs = [ pkgs.elmPackages.elm ];

    buildPhase = ''
      export HOME=$TMPDIR/elm-home
      mkdir -p $HOME/.elm
      cp -r ${elmPackagesCache}/. $HOME/.elm/
      chmod -R u+w $HOME/.elm/
      elm make src/Main.elm --optimize --output elm.js
    '';

    installPhase = ''
      mkdir -p $out
      cp elm.js $out/
      cp dist/index.html $out/
    '';
  };

  binary = craneLib.buildPackage (commonArgs // {
    pname = "loku-web";
    cargoExtraArgs = "-p loku-web";
  });
in
pkgs.symlinkJoin {
  name = "loku-web";
  paths = [ binary ];
  postBuild = ''
    mkdir -p $out/share/loku-web
    ln -s ${elmFrontend} $out/share/loku-web/frontend
  '';
}
