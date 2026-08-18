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
    cargo test --workspace -- --ignored

# Build Elm then run via cargo, forwarding all arguments.
run *args: build-elm
    cargo run {{args}}
