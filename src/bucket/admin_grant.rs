//! The authority of trusted local administration: seeding, bucket initialization and offline
//! import. [`BucketAccess`](super::BucketAccess) carries it, so it lives beside the bucket model.
//! Only `storage::admin` mints a grant, for operator-owned input; Clippy's `disallowed_methods`
//! rejects [`AdminGrant::new`] anywhere else, so API requests keep authorizing through their
//! profile.

use crate::notification::NotificationMode;

/// The administrative operation that holds a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Api,
    Seed,
    BucketInit,
    Import,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdminGrant {
    origin: Origin,
    notification_mode: NotificationMode,
}

impl AdminGrant {
    pub(crate) fn new(origin: Origin, notification_mode: NotificationMode) -> Self {
        Self {
            origin,
            notification_mode,
        }
    }

    /// The grant that startup seeding holds when local administration is enabled.
    #[cfg(test)]
    pub(crate) fn seed() -> Self {
        Self::new(Origin::Seed, NotificationMode::Emit)
    }

    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }

    /// What the grant's operations report. An import reproduces its source and reports nothing,
    /// whatever mode was stored.
    pub(crate) fn notification_mode(&self) -> NotificationMode {
        match self.origin {
            Origin::Import => NotificationMode::Suppress,
            Origin::Api | Origin::Seed | Origin::BucketInit => self.notification_mode,
        }
    }

    pub(crate) fn with_notification_mode(self, notification_mode: NotificationMode) -> Self {
        Self {
            notification_mode,
            ..self
        }
    }

    /// The bucket access every administrative operation of this grant checks.
    pub(crate) fn access(&self) -> super::BucketAccess {
        super::BucketAccess::Admin(self.clone())
    }
}
