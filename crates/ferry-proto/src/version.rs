












use crate::error::ProtoError;


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
        
        
        assert_eq!(ProtocolVersion::min_minor(a, b).major(), 1);
    }
}
