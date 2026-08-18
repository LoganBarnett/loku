{
  description = "Local video browser and player";
  inputs = {
    # LLM: Do NOT change this URL unless explicitly directed. This is the
    # correct format for nixpkgs stable (25.11 is correct, not nixos-25.11).
    nixpkgs.url = "github:NixOS/nixpkgs/25.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
    changelog-roller.url = "github:LoganBarnett/changelog-roller";
    foundation.url = "github:LoganBarnett/rust-template";
    foundation.inputs.nixpkgs.follows = "nixpkgs";
    org-fmt.url = "github:LoganBarnett/org-fmt";
    org-fmt.inputs.nixpkgs.follows = "nixpkgs";
    org-fmt.inputs.rust-overlay.follows = "rust-overlay";
    org-fmt.inputs.crane.follows = "crane";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    crane,
    changelog-roller,
    foundation,
    org-fmt,
  } @ inputs: let
    forAllSystems =
      nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
    perSystem = forAllSystems (system: let
      # Hoisted so the quarantined pkgsUnfreeFor instance (used only for the
      # Apple-SDK stubs the auth stack's framework links need on darwin) stays
      # overlay-consistent with this build `pkgs`.
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
      };
      craneLib =
        (crane.mkLib pkgs).overrideToolchain
        (p: p.rust-bin.stable.latest.default);
      rust = pkgs.rust-bin.stable.latest.default.override {
        extensions = [
          # For rust-analyzer and others.  See
          # https://nixos.wiki/wiki/Rust#Shell.nix_example for details.
          "rust-src"
          "rust-analyzer"
          "rustfmt"
        ];
      };
      crates = {
        # The server binary embeds the compiled Elm frontend; its custom build
        # lives in nix/packages/server.nix.
        server = {
          name = "loku-server";
          binary = "loku-server";
          description = "Local video browser and player";
        };

        # Note: The 'lib' crate is not included here as it doesn't produce a
        # binary.
      };
      commonArgs = {
        src = craneLib.cleanCargoSource self;
        # This governs only the whole-workspace `default` package below; the
        # per-crate packages and the workspace test check get their test scope
        # from mkRustPackages, which overrides this.  Run only unit tests
        # (--lib --bins) and skip the integration tests under tests/, which need
        # a filesystem library fixture unavailable in the Nix sandbox.
        cargoTestExtraArgs = "--lib --bins";
      };
      rustPackages = foundation.lib.mkRustPackages {
        inherit self pkgs craneLib crates commonArgs;
      };
      # The bundled SQLite ships C that every target must compile, and the
      # static musl variant must compile it with a musl toolchain: the host
      # glibc compiler's fortify hardening and large-file redirects emit
      # `__memset_chk`/`stat64`-family symbol references that musl never
      # provides, so the static link fails.  The CC variable is target-scoped
      # (the cc crate consults it only when compiling for that triple), and it
      # is passed only to the musl builds so the cross toolchain stays out of
      # every other build's closure.  The durable fix belongs in
      # rust-template's mkMuslPackages and is tracked at
      # https://github.com/LoganBarnett/rust-template/issues/94; this is the
      # interim.
      muslCommonArgs =
        commonArgs
        // pkgs.lib.optionalAttrs (system == "x86_64-linux") (let
          cc = pkgs.pkgsCross.musl64.stdenv.cc;
        in {
          CC_x86_64_unknown_linux_musl = "${cc}/bin/${cc.targetPrefix}cc";
        })
        // pkgs.lib.optionalAttrs (system == "aarch64-linux") (let
          cc = pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc;
        in {
          CC_aarch64_unknown_linux_musl = "${cc}/bin/${cc.targetPrefix}cc";
        });
      # On Linux each binary also gets a statically-linked `<name>-musl`
      # variant; on other systems mkMuslPackages returns an empty set.
      muslPackages = foundation.lib.mkMuslPackages {
        inherit self pkgs system crates crane;
        commonArgs = muslCommonArgs;
      };
      # The zig-cross variants (portable-glibc and darwin) drive C compiles
      # through `cargo zigbuild`, whose binary always exports an `AR_<target>`
      # pointing at a wrapper it generates under $HOME/.cache (per-derivation
      # $TMPDIR) unless the variable is already set — while crane's deps step
      # first runs a plain `cargo check` with it unset.  The cc crate reads
      # `AR_<target>` when archiving and marks it rerun-if-env-changed, so
      # the unset→set drift re-runs every C build script into the same
      # OUT_DIR, and libsqlite3-sys cannot overwrite the read-only bindings
      # file its first run copied there.  The same drift recurs between the
      # deps and package derivations.  Pre-setting a stable store-path
      # wrapper (the shape the template already uses for CC/CXX) removes the
      # drift entirely: cargo-zigbuild honors an existing value, and
      # `cargo-zigbuild zig ar` is exactly the code path its own generated
      # `ar` takes.  Names are target-scoped, so one overlay serves both
      # helpers and cannot leak into any other build.  The durable fix
      # belongs in rust-template's zig helpers and is tracked at
      # https://github.com/LoganBarnett/rust-template/issues/95; this is the
      # interim.
      zigAr = pkgs.writeShellScript "zigar" ''
        export PATH="${pkgs.zig}/bin:$PATH"
        exec ${pkgs.cargo-zigbuild}/bin/cargo-zigbuild zig ar -- "$@"
      '';
      zigCrossCommonArgs =
        commonArgs
        // {
          AR_x86_64_unknown_linux_gnu = "${zigAr}";
          AR_aarch64_unknown_linux_gnu = "${zigAr}";
          AR_aarch64_apple_darwin = "${zigAr}";
          AR_x86_64_apple_darwin = "${zigAr}";
        };
      # On Linux each binary also gets a portable `<name>-gnu` variant: a
      # dynamic glibc build that runs off the Nix store and links the host's
      # shared libraries.  Empty on other systems.
      gnuPortablePackages = foundation.lib.mkGnuPortablePackages {
        inherit self pkgs system crates crane;
        commonArgs = zigCrossCommonArgs;
      };
      # The x86_64-linux build cross-compiles macOS `<key>-<arch>-darwin`
      # variants via zig so a release needs no macOS runner; empty on other
      # systems.
      # loku's server links Apple frameworks (the auth TLS stack's native cert
      # store — Security / SystemConfiguration / CoreFoundation), so the darwin
      # cross-build needs the Apple SDK's headers and link stubs.  Opt in with
      # `"apple-frameworks": true` in rust-template.json; when set, appleSdk comes
      # from a quarantined unfree nixpkgs (foundation.lib.pkgsUnfreeFor) that
      # accepts the darwin-gated Apple SDK licence — the visible consent — leaving
      # the build `pkgs` graph free.
      appleFrameworksEnabled =
        (builtins.fromJSON (builtins.readFile ./rust-template.json)).apple-frameworks
        or false;
      darwinCrossPackages = foundation.lib.mkDarwinCrossPackages {
        inherit self pkgs system crates crane;
        commonArgs = zigCrossCommonArgs;
        appleSdk =
          if appleFrameworksEnabled
          then (foundation.lib.pkgsUnfreeFor {inherit nixpkgs system overlays;}).apple-sdk.src
          else null;
      };
      # Native Windows PE variants (`<key>-{x86_64,aarch64}-windows`),
      # cross-compiled via llvm-mingw for the gnullvm targets — no Microsoft
      # SDK, no Cygwin/MSYS2 runtime.  Host-agnostic, so it builds on the Linux
      # CI runners and on a contributor's Mac alike.  See CONTRIBUTING.org.
      windowsCrossPackages = foundation.lib.mkWindowsCrossPackages {
        inherit self pkgs system crates crane commonArgs;
      };
      # The opt-in MSVC-ABI Windows variant (`<key>-x86_64-windows-msvc`), for a
      # dependency that requires the MSVC ABI rather than the default gnullvm
      # path above.  Off unless `"windows-msvc": true` is set in
      # rust-template.json.
      windowsMsvcEnabled =
        (builtins.fromJSON (builtins.readFile ./rust-template.json)).windows-msvc
        or false;
      windowsMsvcCrossPackages = foundation.lib.mkWindowsMsvcCrossPackages {
        inherit self pkgs system crates crane commonArgs;
        xwinSdk =
          if windowsMsvcEnabled
          then foundation.lib.xwinSdk {inherit pkgs;}
          else null;
      };
      packages =
        rustPackages.packages
        // muslPackages
        // gnuPortablePackages
        // darwinCrossPackages
        // windowsCrossPackages
        // windowsMsvcCrossPackages
        // {
          default =
            craneLib.buildPackage (commonArgs // {pname = "loku";});
        };
      # The arm64 subset of the darwin cross outputs — the only ones re-signed
      # (and so the only ones the signature guard below verifies).  Empty
      # except on x86_64-linux.
      aarch64DarwinPackages =
        nixpkgs.lib.filterAttrs
        (name: _: nixpkgs.lib.hasSuffix "-aarch64-darwin" name)
        darwinCrossPackages;
      # The x86_64 subset of the Windows cross outputs, smoke-tested under wine.
      windowsX86Packages =
        nixpkgs.lib.filterAttrs
        (name: _: nixpkgs.lib.hasSuffix "-x86_64-windows" name)
        windowsCrossPackages;
    in {
      inherit packages;
      inherit (rustPackages) apps;
      # The darwin ad-hoc signature guard: mkDarwinCrossPackages re-signs each
      # arm64 binary after the release profile's `strip = true` would otherwise
      # invalidate zig's link-time signature.  Only the arm64 outputs are
      # checked.  Empty (and so absent) on every other system.
      checks =
        rustPackages.checks
        // nixpkgs.lib.optionalAttrs (aarch64DarwinPackages != {}) {
          darwinSignatures = foundation.lib.mkDarwinSignatureCheck {
            inherit pkgs;
            darwinPackages = aarch64DarwinPackages;
          };
        }
        # Run the x86_64 Windows cross binaries under wine to prove they
        # execute, not merely link.  Gated to x86_64-linux.
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
          windowsSmoke = foundation.lib.mkWindowsSmokeCheck {
            inherit pkgs;
            windowsPackages = windowsX86Packages;
          };
        };
      devShells = {
        default = pkgs.mkShell {
          buildInputs = [
            # Rust toolchain (compiler, cargo, rustfmt, rust-analyzer).
            rust
            # Prunes stale per-profile artifacts from target/ to reclaim disk.
            pkgs.cargo-sweep
            # JSON parsing for the shellHook's cargo-package listing.
            pkgs.jq
            # Elm toolchain for the frontend/ app: compiler, formatter, and the
            # elm2nix bridge that pins Elm deps for reproducible builds.
            pkgs.elmPackages.elm
            pkgs.elmPackages.elm-format
            pkgs.elm2nix
            # Unified formatter and the per-language binaries it invokes.
            pkgs.treefmt
            pkgs.alejandra
            pkgs.prettier
            # Command runner for the project's justfile recipes.
            pkgs.just
            # ffprobe/ffmpeg for local runs and the ffmpeg-dependent ignored
            # tests (`just test-ffmpeg`); headless keeps the closure small.
            pkgs.ffmpeg-headless
            # Rolls the CHANGELOG on release; used by the reusable CI workflow's
            # `changelog` job and runnable locally for the same flow.
            changelog-roller.packages.${system}.default
            # Formats org-mode documents (treefmt delegates .org files to it).
            org-fmt.packages.${system}.default
            # ABI baseline check used by the reusable CI workflow's `abi` job;
            # `doCheck = false` skips upstream snapshot tests that assume x86_64.
            (pkgs.cargo-semver-checks.overrideAttrs (_: {doCheck = false;}))
          ];
          shellHook = ''
            ${foundation.lib.cargoHuskyHookSnippet pkgs}
            echo "Loku development environment"
            echo ""
            echo "Start the app:"
            echo "  just run -- --library tmp --listen 127.0.0.1:8081"
            echo ""
            echo "Available Cargo packages (use 'cargo build -p <name>'):"
            cargo metadata --no-deps --format-version 1 2>/dev/null | \
              jq --raw-output '.packages[].name' | \
              sort | \
              sed 's/^/  • /' || echo "  Run 'cargo init' to get started"

            echo ""
            echo "Elm frontend (frontend/):"
            echo "  Build:   cd frontend && elm make src/Main.elm --output public/elm.js"
            echo "  Format:  treefmt"
            echo "  After changing elm.json dependency versions, regenerate Nix files:"
            echo "    cd frontend"
            echo "    elm2nix convert 2>/dev/null > elm-srcs.nix"
            echo "    elm2nix snapshot"
            echo "    git add elm-srcs.nix registry.dat && git commit"
          '';
          # A runtime marker identifying this as the default dev shell.  A
          # compliance check reads it back with `nix eval`; the `ci` shell
          # carries the same marker with the value "ci".
          RUST_TEMPLATE_SHELL = "default";
        };
        # Minimal shell for the reusable CI workflow: the Rust toolchain plus the
        # release CLIs the `nix develop .#ci` jobs invoke.  Its baseline comes
        # from foundation's mkCiShell.
        ci = foundation.lib.mkCiShell {
          inherit pkgs system;
          toolchain = rust;
        };
      };
    });
  in {
    devShells =
      nixpkgs.lib.mapAttrs (_: p: p.devShells) perSystem;
    packages = nixpkgs.lib.mapAttrs (_: p: p.packages) perSystem;
    apps = nixpkgs.lib.mapAttrs (_: p: p.apps) perSystem;
    checks = nixpkgs.lib.mapAttrs (_: p: p.checks) perSystem;

    # ================================================================
    # NIXOS MODULES
    # ================================================================
    nixosModules = {
      server = import ./nix/modules/server.nix {inherit self;};
      default = self.nixosModules.server;
    };
  };
}
