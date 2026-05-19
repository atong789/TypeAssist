-- Initial schema. Versioned by sqlx-migrate.

CREATE TABLE IF NOT EXISTS volatility_snapshots (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    captured_at_ms  INTEGER NOT NULL,
    schema_version  INTEGER NOT NULL,
    payload_json    TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS word_outcomes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms    INTEGER NOT NULL,
    word            TEXT    NOT NULL,
    outcome         TEXT    NOT NULL CHECK (outcome IN (
                        'clean_hit',
                        'self_corrected',
                        'uncorrected_miss',
                        'two_keys_together'
                    ))
);

CREATE INDEX IF NOT EXISTS idx_word_outcomes_ts ON word_outcomes(timestamp_ms);

CREATE TABLE IF NOT EXISTS user_lexicon_overlay (
    word            TEXT PRIMARY KEY,
    added_at_ms     INTEGER NOT NULL
);
