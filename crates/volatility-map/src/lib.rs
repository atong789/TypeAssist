//! Layer 3 — the spatial volatility map.
//!
//! This crate defines the shared contract that L2 writes into and L4 reads from.
//! Every field is versioned and JSON-serializable. The corresponding JSON Schema
//! lives at `schema/volatility-map.v1.json` and is regenerated with `just schema`.

pub mod confidence;
pub mod schema;

pub use confidence::{finger_for, Confidence, Finger, Hand};
pub use schema::{
    KeyConfidence, ProfileContext, SwapPair, TimeOfDay, VolatilityMap, SCHEMA_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("schema version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: u32, found: u32 },

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn parse(json: &str) -> Result<VolatilityMap> {
    let map: VolatilityMap = serde_json::from_str(json)?;
    if map.version != SCHEMA_VERSION {
        return Err(Error::VersionMismatch {
            expected: SCHEMA_VERSION,
            found: map.version,
        });
    }
    Ok(map)
}
