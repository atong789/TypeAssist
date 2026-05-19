use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Per-key confidence in [0.0, 1.0]. 1.0 = always hit cleanly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Confidence(pub f32);

impl Confidence {
    pub fn new(v: f32) -> Self {
        Self(v.clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Hand {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Finger {
    Thumb,
    Index,
    Middle,
    Ring,
    Pinky,
}
