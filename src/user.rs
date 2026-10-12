use bitflags::bitflags;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod discord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(u64);

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UserPermissionSet: u8 {
        const ADMIN       = 0b001;
        const REVIEWER    = 0b010;
        const PARTICIPANT = 0b100;
    }
}

impl Serialize for UserPermissionSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(self.bits() as i64)
    }
}

impl<'de> Deserialize<'de> for UserPermissionSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits = i64::deserialize(deserializer)?;
        let Ok(bits) = u8::try_from(bits) else {
            return Err(serde::de::Error::custom("invalid permission bits"));
        };
        UserPermissionSet::from_bits(bits).ok_or_else(|| serde::de::Error::custom("invalid permission bits"))
    }
}
