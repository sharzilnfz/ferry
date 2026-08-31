#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Offline,
    Synced,
    Syncing,
    Conflict,
    Pinned,
    Idle,
    Error,
}

impl SyncState {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Synced => "SYNCED",
            Self::Syncing => "SYNCING",
            Self::Conflict => "CONFLICT",
            Self::Pinned => "PINNED",
            Self::Idle => "IDLE",
            Self::Error => "ERROR",
            Self::Offline => "OFFLINE",
        }
    }

    #[must_use]
    pub const fn badge_text(&self) -> &'static str {
        self.label()
    }

    #[must_use]
    pub const fn pulse_speed(&self) -> f64 {
        match self {
            Self::Synced => 0.8,
            Self::Syncing => 2.0,
            Self::Pinned => 1.0,
            Self::Conflict => 3.0,
            Self::Idle | Self::Offline | Self::Error => 0.0,
        }
    }

    #[must_use]
    pub const fn is_pulsing(&self) -> bool {
        self.pulse_speed() > 0.0
    }

    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        matches!(self, Self::Pinned)
    }
}

pub const HOLDING_ALIAS: SyncState = SyncState::Pinned;

impl SyncState {
    pub const Holding: Self = Self::Pinned;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alias_holding_equals_pinned() {
        assert_eq!(SyncState::Holding, SyncState::Pinned);
        assert_eq!(SyncState::Holding.label(), "PINNED");
        assert_eq!(SyncState::Pinned.label(), "PINNED");
        assert_eq!(SyncState::Holding.pulse_speed(), SyncState::Pinned.pulse_speed());
    }
}
