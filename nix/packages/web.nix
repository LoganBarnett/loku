# Crane-based derivation for the loku-web binary.
# Called from flake.nix with: import ./web.nix { inherit craneLib commonArgs; }
{ craneLib, commonArgs }:
craneLib.buildPackage (commonArgs // {
  pname = "loku-web";
  cargoExtraArgs = "-p loku-web";
})
