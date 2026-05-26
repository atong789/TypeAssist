# Lexicon

Layer 4 is lexicon-aware. The bundled files are the **word source** that Components 3a (lookup) and 3b (candidates) check against. This pass ships data + loaders only — no scoring, no correction.

The lexicon does **two different jobs** and uses **two different sources**:

| Job | Source | File | Purpose |
|---|---|---|---|
| Membership (`is_known`) | SCOWL en_US + contractions, size 50 cumulative | `words_clean.txt` | Gate correction — only correctly-spelled words count as "known" |
| Frequency (`frequency`) | Norvig top-50k web unigrams | `words_freq.txt` | Rank candidates of *unknown* words; never gates membership |

This split exists because the Norvig web-unigram corpus contains common misspellings (`teh`, `recieve`, `thier`) and apostrophe-stripped contractions (`didnt`, `thats`) as legitimate-looking entries. Treating "in the freq table" as "known" would protect the very typos the engine should catch — and treating apostrophes as splitters made `didn't` look unknown (so the engine suggested `didnt` as the "correction"). See `crates/correction-engine/src/lexicon.rs` for the runtime API.

## `words_clean.txt` — membership set (SCOWL en_US, size 50 cumulative)

~81,000 words, lowercase, sorted, deduplicated. Includes contractions (`didn't`, `can't`, `should've`, …).

Source: SCOWL release `scowl-2020.12.07` from http://wordlist.aspell.net/, specifically the size-≤50 union of:

- `english-words.{10,20,35,40,50}`
- `american-words.{10,20,35,40,50}`
- `variant_1-words.{10,20,35,40,50}` (common American spelling variants)
- `english-contractions.{10,35,40,50}`
- `variant_1-contractions.{35,50}`

To rebuild:

```sh
curl -sL https://downloads.sourceforge.net/project/wordlist/SCOWL/2020.12.07/scowl-2020.12.07.tar.gz \
  | tar -xz -C /tmp
cd /tmp/scowl-2020.12.07/final
cat english-words.{10,20,35,40,50} \
    american-words.{10,20,35,40,50} \
    variant_1-words.{10,20,35,40,50} \
    english-contractions.{10,35,40,50} \
    variant_1-contractions.{35,50} \
  | iconv -f ISO-8859-1 -t UTF-8 \
  | LC_ALL=C tr 'A-Z' 'a-z' \
  | sort -u \
  > crates/correction-engine/lexicon/words_clean.txt
```

License: SCOWL composite (Kevin Atkinson et al.) — see `SCOWL_COPYRIGHT.txt` shipped alongside. Permissive and explicitly redistributable.

## `words_freq.txt` — frequency table (Norvig top-50k web unigrams)

~50,000 entries, `word<TAB>count` per line, lowercase, sorted by descending count.

Source: Peter Norvig's extraction of the Google Web Trillion Word Corpus — http://norvig.com/ngrams/count_1w.txt — top 50k lines. To refresh:

```sh
curl -s https://norvig.com/ngrams/count_1w.txt | head -n 50000 \
  > crates/correction-engine/lexicon/words_freq.txt
```

License: published freely on Norvig's site, widely redistributed in open-source spelling tools. No explicit license header. The file is used here **only as a frequency-ranking table** for candidates of words deemed unknown by the clean dict.

## Versioning

Bump [`LEXICON_VERSION`](../src/lexicon.rs) on any change to either bundled file or the seed list. Currently at **v2** (split sources; v1 was Norvig-only and suffered the typos-pass-as-known bug).

## Seed proper nouns

The loader injects a **temporary hardcoded** seed list of proper nouns (`Krutrim`, `ZAMS`, `ONDC`) into the clean set so the engine recognises them while development continues. This is a **dev fixture** — production populates the user's proper-noun set from live-learning (Component 5: user typed it, we didn't correct it, after N repetitions add it). The seed list is **never** populated by the user typing words into any UI.

## User overlay (future)

The user's "always-allow" words (names, project terms, jargon) belong in a separate overlay stored in SQLite, not in these files. Both bundled files are immutable; the seed list is a temporary stand-in until Component 5 lands.
