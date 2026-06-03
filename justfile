set shell := ["bash", "-cu"]

# Apple Silicon target triple — Tauri appends this to sidecar binary names.
# Override on Intel: `just sidecar_triple=x86_64-apple-darwin build-sidecar`
sidecar_triple := "aarch64-apple-darwin"
sidecar_dest := "apps/tauri/src-tauri/sidecars/typeassist-input-macos-" + sidecar_triple

# Code-signing identity. A STABLE identity (vs ad-hoc `-`) is what lets macOS
# remember the Accessibility / Input-Monitoring grant across rebuilds: TCC keys
# the grant on the signature's Designated Requirement (cert + identifier), which
# is stable, instead of the ad-hoc cdhash, which changes every build. Locally
# this is the self-signed "TypeAssist Local Signing" cert (see docs). Override
# for ad-hoc: `just signing_identity=- build-sidecar`.
signing_identity := "TypeAssist Local Signing"
# Stable identifier for the sidecar so its Designated Requirement (and thus its
# TCC grant) is invariant across rebuilds. Matches the identifier the Tauri
# bundler assigns the nested copy, so the dev sidecar and the installed one
# share ONE grant (cert + this identifier), in dev and from /Applications alike.
sidecar_identifier := "typeassist-input-macos"

default:
    @just --list

# Build everything: Swift sidecar → Rust workspace → Tauri bundle
build-mac: build-sidecar
    cargo build --workspace
    cd apps/tauri && npm run tauri build

# Build the Swift sidecar, stage it for Tauri, and codesign it
build-sidecar:
    cd adapters/macos && swift build -c release
    mkdir -p apps/tauri/src-tauri/sidecars
    cp adapters/macos/.build/release/typeassist-input-macos {{sidecar_dest}}
    just sign-dev

# Codesign the sidecar with a STABLE identity + identifier so its TCC grant
# (Accessibility / Input Monitoring) survives rebuilds. The Tauri app bundle is
# signed with the same identity via `bundle.macOS.signingIdentity` in
# tauri.conf.json, so both share one signing authority.
sign-dev:
    codesign --force --sign "{{signing_identity}}" --identifier "{{sidecar_identifier}}" {{sidecar_dest}}

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
