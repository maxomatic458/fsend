pub mod code;
pub mod pairing;
pub mod peer;
pub mod relay;

#[cfg(test)]
mod testing {
    use serde::{Deserialize, Serialize};

    /// `message` serializes to exactly `json` and parses back.
    pub fn pinned<T>(message: T, json: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_string(&message).unwrap(), json);
        assert_eq!(serde_json::from_str::<T>(json).unwrap(), message);
    }
}
