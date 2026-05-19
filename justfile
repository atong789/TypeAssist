set shell := ["bash", "-cu"]

# Apple Silicon target triple — Tauri appends this to sidecar binary names.
# Override on Intel: `just sidecar_triple=x86_64-apple-darwin build-sidecar`
sidecar_triple := "aarch64-apple-darwin"
sidecar_dest := "apps/tauri/src-tauri/sidecars/typeassist-input-macos-" + sidecar_triple

default:
    @just --list

# Build everything: Swift sidecar → Rust workspace → Tauri bundle
build-mac: build-sidecar
    cargo build --workspace
    cd apps/tauri && npm run tauri build

# Build the Swift sidecar, stage it for Tauri, and ad-hoc codesign
build-sidecar:
    cd adapters/macos && swift build -c release
    mkdir -p apps/tauri/src-tauri/sidecars
    cp adapters/macos/.build/release/typeassist-input-macos {{sidecar_dest}}
    just sign-dev

# Ad-hoc codesign so Accessibility permission persists across rebuilds
sign-dev:
    codesign --force --deep --sign - {{sidecar_dest}}

# Run the Tauri app in dev (spawns the Swift sidecar)
dev: build-sidecar
    cd apps/tauri && npm run tauri dev

# Regenerate the L3 JSON Schema artifact from serde types
schema:
    cargo run -p volatility-map --bin emit_schema

# Run Rust tests
test:
    cargo test --workspace

# Lint
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --check

# Format
fmt:
    cargo fmt
