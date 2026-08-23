//! Protocol version arithmetic and the negotiation rule.
//!
//! A version is one `u16`: high byte major, low byte minor. Wire v1 is
//! `1.0`. The negotiation rule (normative in `docs/store-format.md`):
//!
//! - Peers advertise the MAXIMUM version they speak.
//! - Majors MUST match exactly; a mismatch is a clean disconnect
//!   ([`ProtoError::VersionIncompatible`]) after BYE(1).
//! - The agreed version is `min(a.minor, b.minor)` under the common major.
//!   Both sides speak at or below it for the rest of the session.
//! - Messages introduced in minors above the agreed version MUST NOT be
//!   sent, even when both sides understand them.

use crate::error::ProtoError;

/// Major.minor packed into a u16: `(major << 8) | minor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    pub const V1_0: ProtocolVersion = ProtocolVersion(0x0100);

    pub const fn new(major: u8, minor: u8) -> Self {
        ProtocolVersion(((major as u16) << 8) | minor as u16)
    }

    pub const fn major(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub const fn minor(self) -> u8 {
        self.0 as u8
    }

    pub const fn to_u16(self) -> u16 {
        self.0
    }

    pub const fn from_u16(v: u16) -> Self {
        ProtocolVersion(v)
    }

    /// Componentwise minimum via the packed representation: within a major
    /// this is the lower minor; across majors the lower major wins, so the
    /// result can never silently upgrade past either speaker.
    pub const fn min_minor(a: ProtocolVersion, b: ProtocolVersion) -> ProtocolVersion {
        if a.0 <= b.0 {
            a
        } else {
            b
        }
    }
}

impl core::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.major(), self.minor())
    }
}

/// Negotiate the session version from both sides' advertised maxima.
///
/// Returns [`ProtoError::VersionIncompatible`] on any major mismatch — the
/// caller translates that into BYE(1) and a clean disconnect.
pub fn negotiate(
    ours: ProtocolVersion,
    theirs: ProtocolVersion,
) -> Result<ProtocolVersion, ProtoError> {
    if ours.major() != theirs.major() {
        return Err(ProtoError::VersionIncompatible { ours, theirs });
    }
    Ok(ProtocolVersion::min_minor(ours, theirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_round_trips_and_orders() {
        let v = ProtocolVersion::new(1, 9);
        assert_eq!(v.to_u16(), 0x0109);
        assert_eq!(ProtocolVersion::from_u16(0x0109), v);
        assert!(ProtocolVersion::new(1, 2) < ProtocolVersion::new(1, 10));
        assert!(ProtocolVersion::new(1, 0) < ProtocolVersion::new(2, 0));
        assert_eq!(ProtocolVersion::V1_0.to_string(), "1.0");
        assert_eq!(ProtocolVersion::new(3, 21).to_string(), "3.21");
    }

    #[test]
    fn negotiation_takes_the_lower_minor_within_a_major() {
        assert_eq!(
            negotiate(ProtocolVersion::new(1, 4), ProtocolVersion::new(1, 7)).unwrap(),
            ProtocolVersion::new(1, 4)
        );
        // Symmetric: either side may be the lower one.
        assert_eq!(
            negotiate(ProtocolVersion::new(1, 7), ProtocolVersion::new(1, 4)).unwrap(),
            ProtocolVersion::new(1, 4)
        );
        assert_eq!(
            negotiate(ProtocolVersion::new(1, 3), ProtocolVersion::new(1, 3)).unwrap(),
            ProtocolVersion::new(1, 3)
        );
    }

    #[test]
    fn major_mismatch_is_a_typed_incompatibility() {
        let err = negotiate(ProtocolVersion::new(1, 5), ProtocolVersion::new(2, 0)).unwrap_err();
        assert!(matches!(
            err,
            ProtoError::VersionIncompatible { ours, theirs }
                if ours == ProtocolVersion::new(1, 5) && theirs == ProtocolVersion::new(2, 0)
        ));
    }

    #[test]
    fn min_minor_never_crosses_majors() {
        let a = ProtocolVersion::new(1, 9);
        let b = ProtocolVersion::new(2, 0);
        // Even misused across majors the helper keeps ONE major; negotiate()
        // is the gate callers must pass first.
        assert_eq!(ProtocolVersion::min_minor(a, b).major(), 1);
    }
}
