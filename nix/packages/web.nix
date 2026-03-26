# Crane-based derivation for the loku-web binary.
# Called from flake.nix with: import ./web.nix { inherit craneLib commonArgs pkgs; }
#
# The package output includes:
#   $out/bin/loku-web                    — the server binary
#   $out/share/loku-web/frontend/        — pre-built Elm frontend assets
#
# The NixOS module passes --frontend-path pointing at the share directory.
# When Elm sources change, rebuild locally with:
#   nix develop --command sh -c 'cd frontend && elm make src/Main.elm --optimize --output dist/elm.js'
# then commit dist/elm.js before deploying.
{ craneLib, commonArgs, pkgs }:
let
  # Extend the Cargo source filter to also include frontend/dist/ so the
  # pre-built Elm assets are available during the postInstall phase.
  src = pkgs.lib.cleanSourceWith {
    src = ../../.;
    filter = path: type:
      (craneLib.filterCargoSources path type) ||
      (pkgs.lib.hasPrefix (toString ../../frontend/dist) path);
  };
in
craneLib.buildPackage (commonArgs // {
  inherit src;
  pname = "loku-web";
  cargoExtraArgs = "-p loku-web";

  postInstall = ''
    mkdir -p $out/share/loku-web
    cp -r frontend/dist $out/share/loku-web/frontend
  '';
})
