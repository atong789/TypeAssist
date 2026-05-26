# Lexicon

Layer 4 is lexicon-aware. The bundled `words_freq.txt` is the **word source** that Component 3 (confidence scoring) will check candidates against. This pass (3a) ships the data + loader only — no scoring, no candidate generation.

## Source

`words_freq.txt` is the **top 50,000 unigrams** from Peter Norvig's extraction of the Google Web Trillion Word Corpus:

http://norvig.com/ngrams/count_1w.txt

Lines are tab-separated `word<TAB>count`, lowercase ASCII, sorted by descending count. We slice the top 50k from the full 333,333-entry file — enough coverage that everyday correspondence stays in the lexicon, small enough that the bundle stays under 1 MB.

To refresh:

```sh
curl -s https://norvig.com/ngrams/count_1w.txt | head -n 50000 \
  > crates/correction-engine/lexicon/words_freq.txt
```

Bump [`LEXICON_VERSION`](../src/lexicon.rs) on every refresh so consumers can refuse stale snapshots.

## License

Norvig's `count_1w.txt` is published freely on his site, derived from the publicly distributed Google Web 1T 5-gram corpus and widely redistributed in open-source spelling tools (pyspellchecker, language detectors, etc.). No explicit license header ships with the file. If a stricter provenance is needed downstream, swap in a SCOWL-derived list paired with a separately licensed frequency source — the loader contract (`word<TAB>count` per line, lowercase) is what `lexicon.rs` depends on, not the specific source.

## Seed proper nouns

The loader also injects a **temporary hardcoded** seed list of proper nouns (`Krutrim`, `ZAMS`, `ONDC`) so the engine recognises them while we develop the correction logic. This is a **dev fixture** — production populates the user's proper-noun set from live-learning (Component 5: user typed it, we didn't correct it, after N repetitions add it). The seed list is **never** populated by the user typing words into any UI.

## User overlay (future)

The user's "always-allow" words (names, project terms, jargon) belong in a separate overlay stored in SQLite, not in this file. The bundled list is immutable; the seed list is a temporary stand-in until Component 5 lands.
