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

# Build the UNIVERSAL (arm64 + x86_64) Swift sidecar and stage the THREE files
# Tauri's `--target universal-apple-darwin` build needs at once: two thin per-arch
# sidecars (each per-arch cargo sub-build validates its OWN triple name) PLUS one
# fat `-universal-` sidecar (what the bundler copies into the .app — Tauri does not
# lipo sidecars itself). Missing a per-arch → build-script fails; missing the fat →
# bundler fails.
#
# NOTE: multi-arch `swift build --arch arm64 --arch x86_64` needs full Xcode's
# xcbuild. On a Command-Line-Tools-only Mac (this one) it fails, so build each
# slice separately and lipo. Signed here with the local dev identity; for the
# notarized RELEASE, re-sign all three Developer ID + `--options runtime` and run
# the universal `tauri build` per docs/releasing-beta.md.
build-sidecar-universal:
    cd adapters/macos && swift build -c release --scratch-path .build-arm64
    cd adapters/macos && swift build -c release --scratch-path .build-x86 \
        -Xswiftc -target -Xswiftc x86_64-apple-macosx13.0 \
        -Xcc     -target -Xcc     x86_64-apple-macosx13.0 \
        -Xlinker -arch   -Xlinker x86_64
    mkdir -p apps/tauri/src-tauri/sidecars
    lipo -create \
        adapters/macos/.build-arm64/arm64-apple-macosx/release/typeassist-input-macos \
        adapters/macos/.build-x86/arm64-apple-macosx/release/typeassist-input-macos \
        -output apps/tauri/src-tauri/sidecars/typeassist-input-macos-universal-apple-darwin
    lipo apps/tauri/src-tauri/sidecars/typeassist-input-macos-universal-apple-darwin -thin arm64 \
        -output apps/tauri/src-tauri/sidecars/typeassist-input-macos-aarch64-apple-darwin
    lipo apps/tauri/src-tauri/sidecars/typeassist-input-macos-universal-apple-darwin -thin x86_64 \
        -output apps/tauri/src-tauri/sidecars/typeassist-input-macos-x86_64-apple-darwin
    rm -rf adapters/macos/.build-arm64 adapters/macos/.build-x86
    for f in aarch64 x86_64 universal; do \
        codesign --force --sign "{{signing_identity}}" --identifier "{{sidecar_identifier}}" \
            apps/tauri/src-tauri/sidecars/typeassist-input-macos-$f-apple-darwin; \
    done

# Run the Tauri app in dev (spawns the Swift sidecar)
dev: build-sidecar
    cd apps/tauri && npm run tauri dev

# Dev run against a THROWAWAY data store — for previewing onboarding (or anything
# that captures typing) WITHOUT touching real learned data. Every engine write
# (motor map, word patterns, snapshots, allow-list, progress, guesser) is
# redirected to ~/.typeassist-sandbox via the TYPEASSIST_DATA_DIR valve, so your
# real ~/.typeassist is never read or written. Throwaway — delete it any time with
# `just sandbox-clean`.
dev-sandbox: build-sidecar
    mkdir -p "$HOME/.typeassist-sandbox"
    @echo "▶ sandbox data store: $HOME/.typeassist-sandbox (real ~/.typeassist is untouched)"
    cd apps/tauri && TYPEASSIST_DATA_DIR="$HOME/.typeassist-sandbox" npm run tauri dev

# Delete the throwaway sandbox data store (never touches real ~/.typeassist)
sandbox-clean:
    rm -rf "$HOME/.typeassist-sandbox"
    @echo "✓ removed ~/.typeassist-sandbox"

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
