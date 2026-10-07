# Build both Rust and Elm.
build: build-elm build-rust

# Build the Elm frontend.
build-elm:
    cd frontend && elm make src/Main.elm --output public/elm.js

# Build all Rust workspace crates.
build-rust:
    cargo build --workspace

# Run all tests (Elm compile check + Rust test suite).
test: build-elm test-rust

# Run the Rust test suite.
test-rust:
    cargo test --workspace

# Run the ffmpeg-dependent tests, which are ignored by default because they
# need real ffmpeg/ffprobe binaries (the dev shell provides them).
test-ffmpeg:
    cargo test --workspace --test ffmpeg_real_test -- --ignored

# Build Elm then run via cargo, forwarding all arguments.
run *args: build-elm
    cargo run {{args}}

# Combine all passing open Dependabot PRs into one PR and merge it.
#
# One-shot catch-up for a Dependabot backlog: takes the versions Dependabot
# already resolved, bundles the passing bumps onto one branch, opens a single
# PR, and merges it once green (failing bumps are left for a human).  Per-PR
# auto-merge handles the steady-state trickle.  Pass --dry-run or --no-merge to
# hold back.
dependabot-combine *args:
    dependabot-combine {{args}}

# Bump every dependency the workspace's constraints allow, with changelog.
#
# The working-tree half of the scheduled dependency-bump flow: runs `cargo
# update` across the workspace, classifies each bump against `cargo audit`,
# and composes the CHANGELOG entries — then stops.  Nothing is committed or
# pushed; review the diff and commit yourself.  The scheduled workflow
# (.github/workflows/dependency-bump.yml) runs the same engine and owns the
# branch/PR/merge half.  Pass --dry-run true to preview.
dependency-bump *args:
    dependency-bump {{args}}

# Review the current change set against the project's conventions.
#
# Sends the change set to a nested reviewer that judges it against the
# project's convention documents, and reports what stands.  Verdicts are
# recorded per file in review.json so addressing one file does not re-review
# the rest; pass --priors clear to judge those files afresh.  The reviewer is
# your own `claude` install, which must be on PATH.
#
# Exit code 1 means the review ran and found something; 2 means it could not
# judge the tree in full.  `just` reports either as a failed recipe, so read
# the code: a 1 with findings printed above it is the tool working, not
# breaking.
review *args:
    review {{args}}

# Run against a fake optical drive: the shims under scripts/fake-drive stand in
# for udevadm, makemkvcon, and eject, so the rip pipeline and the Rips page can
# be exercised on a machine without a drive.  Set FAKE_DISC_LABEL (and
# optionally FAKE_DISC_UUID, FAKE_TITLES, FAKE_RIP_STEP_SECS) to "insert" a
# disc; the discs library is tmp/discs.
run-fake-drive *args: build-elm
    mkdir --parents tmp/discs
    PATH="$PWD/scripts/fake-drive:$PATH" \
      cargo run -- --config scripts/fake-drive/config.toml --listen 127.0.0.1:8081 {{args}}

# Run the rip-tool smoke test, which needs udevadm, makemkvcon, and eject on
# PATH and an optical drive at /dev/sr0 (Linux only).
test-rip-tools:
    cargo test --workspace --test disc_tools_real_test -- --ignored

# Run the real-TMDB test, which needs network access and a key in the file
# named by loku_tmdb_api_key_file.
test-tmdb:
    cargo test --workspace --test tmdb_real_test -- --ignored
