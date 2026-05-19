# Volatility map (L3 contract)

This is the load-bearing contract between L2 (writer) and L4 (reader). Both depend on the `volatility-map` crate as a peer.

## Stability

The Rust types in `crates/volatility-map/src/schema.rs` are the source of truth. The corresponding JSON Schema is emitted to `crates/volatility-map/schema/volatility-map.v1.json` by:

```bash
just schema
```

Regenerate after any change to the schema crate. The generated file is checked in so external consumers (future non-Rust adapters, validation tests, doc tooling) can rely on it.

## Versioning

`SCHEMA_VERSION` is a constant in `schema.rs`. Bumping it requires:

1. A new schema file (e.g. `volatility-map.v2.json`) — keep the old one.
2. Migration logic in any code that persists snapshots.
3. A deliberate review of L2 (writers) and L4 (readers).

`volatility_map::parse` rejects mismatched versions to make accidental drift a loud error.

## Top-level shape

```jsonc
{
  "version": 1,
  "generated_at_ms": 1729000000000,
  "profile": {
    "time_of_day": "morning",
    "session_fatigue": 0.18
  },
  "keys": [
    {
      "key": "t",
      "confidence": 0.92,
      "sample_count": 412,
      "mean_dwell_ms": 84.0,
      "hand": "left",
      "finger": "index"
    }
  ],
  "swap_pairs": [
    {
      "aimed_for": "t",
      "hit_instead": "y",
      "frequency": 0.04,
      "hand": "right",
      "finger": "index"
    }
  ]
}
```

## Field notes

- `confidence`: clamped to `[0.0, 1.0]`. 1.0 = always hit cleanly.
- `swap_pairs`: ordered pairs. "aimed for T, hit Y" is distinct from "aimed for Y, hit T".
- `profile.session_fatigue`: monotonically non-decreasing within a session; reset on session boundary.
- `profile.time_of_day`: which temporal bucket this snapshot belongs to. L2 maintains separate aggregates per bucket.

## What does *not* live here

- Raw event streams (those are L2-internal).
- Lexicon entries (those live in L4 / storage overlay).
- UI state (that belongs to L5).

If you find yourself wanting to add UI prefs or a wordlist field here, that is a signal to revisit the boundary instead.
