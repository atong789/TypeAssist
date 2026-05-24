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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Hand {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Finger {
    Thumb,
    Index,
    Middle,
    Ring,
    Pinky,
}

/// Standard touch-typing finger assignment for a US QWERTY layout.
///
/// Returns `None` for keys we don't model (function keys, arrows, control
/// chars, multi-character key names). Case-insensitive: shifted characters
/// map to the same finger as their unshifted form (e.g. `A` → left pinky).
/// Space is conventionally assigned to the right thumb — either thumb works
/// in practice and we don't yet know which the user uses.
///
/// Lives at L3 next to `Finger`/`Hand` because both L2 (timing rollups) and
/// L4 (correction priors) need to ask this question; layering it here keeps
/// them from depending on each other.
pub fn finger_for(key: &str) -> Option<(Hand, Finger)> {
    // Multi-character key names ("Escape", "ArrowLeft", …) aren't typing keys.
    let lower = key.to_lowercase();
    let mut chars = lower.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    use Finger::*;
    use Hand::*;
    let pair = match c {
        '`' | '~' | '1' | '!' | 'q' | 'a' | 'z' => (Left, Pinky),
        '2' | '@' | 'w' | 's' | 'x' => (Left, Ring),
        '3' | '#' | 'e' | 'd' | 'c' => (Left, Middle),
        '4' | '$' | '5' | '%' | 'r' | 't' | 'f' | 'g' | 'v' | 'b' => (Left, Index),
        ' ' => (Right, Thumb),
        '6' | '^' | '7' | '&' | 'y' | 'u' | 'h' | 'j' | 'n' | 'm' => (Right, Index),
        '8' | '*' | 'i' | 'k' | ',' | '<' => (Right, Middle),
        '9' | '(' | 'o' | 'l' | '.' | '>' => (Right, Ring),
        '0' | ')' | 'p' | ';' | ':' | '/' | '?' | '-' | '_' | '=' | '+' | '[' | '{' | ']'
        | '}' | '\\' | '|' | '\'' | '"' => (Right, Pinky),
        _ => return None,
    };
    Some(pair)
}

/// Stable left-to-right ordering of every `(Hand, Finger)` pair, anatomical:
/// left pinky first, right pinky last. Use as a sort key wherever per-finger
/// data is displayed so the rows read like a keyboard.
///
/// Lives at L3 next to `Finger`/`Hand`/`finger_for` so L2's aggregators
/// (timing, ghost-keys, …) share one definition.
pub fn anatomical_order(hand: Hand, finger: Finger) -> u8 {
    use Finger::*;
    use Hand::*;
    match (hand, finger) {
        (Left, Pinky) => 0,
        (Left, Ring) => 1,
        (Left, Middle) => 2,
        (Left, Index) => 3,
        (Left, Thumb) => 4,
        (Right, Thumb) => 5,
        (Right, Index) => 6,
        (Right, Middle) => 7,
        (Right, Ring) => 8,
        (Right, Pinky) => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_row_is_correctly_assigned() {
        assert_eq!(finger_for("a"), Some((Hand::Left, Finger::Pinky)));
        assert_eq!(finger_for("s"), Some((Hand::Left, Finger::Ring)));
        assert_eq!(finger_for("d"), Some((Hand::Left, Finger::Middle)));
        assert_eq!(finger_for("f"), Some((Hand::Left, Finger::Index)));
        assert_eq!(finger_for("j"), Some((Hand::Right, Finger::Index)));
        assert_eq!(finger_for("k"), Some((Hand::Right, Finger::Middle)));
        assert_eq!(finger_for("l"), Some((Hand::Right, Finger::Ring)));
        assert_eq!(finger_for(";"), Some((Hand::Right, Finger::Pinky)));
    }

    #[test]
    fn space_is_right_thumb() {
        assert_eq!(finger_for(" "), Some((Hand::Right, Finger::Thumb)));
    }

    #[test]
    fn shifted_and_unshifted_map_the_same() {
        assert_eq!(finger_for("A"), finger_for("a"));
        assert_eq!(finger_for("!"), finger_for("1"));
        assert_eq!(finger_for("?"), finger_for("/"));
    }

    #[test]
    fn unmodeled_keys_return_none() {
        assert_eq!(finger_for("Escape"), None);
        assert_eq!(finger_for("ArrowLeft"), None);
        assert_eq!(finger_for(""), None);
        assert_eq!(finger_for("\t"), None); // we don't assign tab here
    }
}
