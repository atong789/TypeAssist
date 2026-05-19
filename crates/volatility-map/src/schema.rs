use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::confidence::{Confidence, Finger, Hand};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VolatilityMap {
    pub version: u32,
    pub generated_at_ms: u64,
    pub profile: ProfileContext,
    pub keys: Vec<KeyConfidence>,
    pub swap_pairs: Vec<SwapPair>,
}

impl VolatilityMap {
    pub fn empty(generated_at_ms: u64, profile: ProfileContext) -> Self {
        Self {
            version: SCHEMA_VERSION,
            generated_at_ms,
            profile,
            keys: Vec::new(),
            swap_pairs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeyConfidence {
    pub key: String,
    pub confidence: Confidence,
    pub sample_count: u32,
    pub mean_dwell_ms: f32,
    pub hand: Hand,
    pub finger: Finger,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SwapPair {
    pub aimed_for: String,
    pub hit_instead: String,
    pub frequency: f32,
    pub hand: Hand,
    pub finger: Finger,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProfileContext {
    pub time_of_day: TimeOfDay,
    pub session_fatigue: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeOfDay {
    Morning,
    Afternoon,
    Evening,
    Night,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_round_trips() {
        let map = VolatilityMap::empty(
            0,
            ProfileContext {
                time_of_day: TimeOfDay::Morning,
                session_fatigue: 0.0,
            },
        );
        let json = serde_json::to_string(&map).unwrap();
        let back: VolatilityMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, SCHEMA_VERSION);
    }

    #[test]
    fn version_is_stable() {
        assert_eq!(SCHEMA_VERSION, 1, "do not change without a migration plan");
    }
}
