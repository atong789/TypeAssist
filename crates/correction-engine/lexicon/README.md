# Lexicon

Layer 4 is lexicon-aware. We target the **SCOWL English wordlist, size 60** ("common" tier) — broad enough to cover typical correspondence but small enough to keep the bundle modest.

## Source

http://wordlist.aspell.net/scowl-readme

Pick `scowl-2020.12.07.tar.gz` or newer. We need `final/english-words.60` (and likely `english-contractions.60`, `english-abbreviations.60`).

## License

SCOWL is under a permissive composite license. The pieces we'll bundle are explicitly redistributable. The `Copyright` file in the SCOWL tarball must be shipped alongside the wordlist.

## Status — not yet bundled

The actual wordlist file is **not yet committed**. This pass scaffolds the layout only. The next pass should:

1. Add a `fetch.sh` (or `xtask` Rust subcommand) that downloads + verifies SCOWL by SHA, extracts the size-60 lists, and writes a single deduplicated, lowercased, sorted `words.txt` here.
2. Add the resulting `words.txt` and SCOWL `Copyright` file to the repo (committing the artifact, not the tarball).
3. Wire `correction-engine` to load `words.txt` at startup via `include_str!` or a runtime open.

Until then, the engine is a stub.

## User overlay (future)

The user's "always-allow" words (names, project terms, jargon) belong in a separate overlay stored in SQLite, not in this file. The bundled list should be treated as immutable.
