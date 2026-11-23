//! One resource, one outstanding conditional write, no I/O or allocator.
//!
//! The adapter must serialize reconciliation, supply fresh authoritative
//! observations, persist the pending action and issuer watermark before
//! dispatch, and atomically enforce the write's resource version.

#![no_std]

use core::{fmt, num::NonZeroU64};
use periscope_machines::{Issuer, sequence_of};

/// A strictly increasing desired revision, assigned by the caller.
pub type Revision = NonZeroU64;

/// An opaque compare-and-set token, including for absent resources.
///
/// The resource must advance this token on every accepted write and never
/// reuse it, even across deletion, recreation, or restart. Equality is all
/// Sextant uses. A plain unversioned "not found" is not sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version(pub u64);

/// Scoped action correlation, not proof of resource-side write authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionId(u64);

impl ActionId {
    /// Decode a nonzero sequence; the receiving machine checks liveness.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Option<Self> {
        if sequence_of(raw) == 0 {
            return None;
        }
        Some(Self(raw))
    }

    /// The scoped value to persist or send with a receipt.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// The value to install and the revision of that intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent<T> {
    /// A revision must not name two different desired values.
    pub revision: Revision,
    /// An owned value or a stable, equality-comparable description of it.
    pub value: T,
}

/// An observation supplied for one reconciliation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Observation<T> {
    /// Missing, stale, or unavailable evidence; not proof of absence.
    Unknown,
    /// Fresh state from the authority that enforces the write precondition.
    Fresh {
        /// The version of this exact state, even if absent.
        version: Version,
        /// `None` means authoritatively absent.
        value: Option<T>,
    },
}

/// A conditional write. Persist these fields before first dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action<T> {
    /// Correlates terminal evidence with this write.
    pub id: ActionId,
    /// The desired value and revision captured when this action was issued.
    pub intent: Intent<T>,
    /// Only write if this version is still current; advance it on success.
    pub expected: Version,
}

/// A decision for the adapter. No decision performs I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision<T> {
    /// Obtain fresh authoritative state.
    ObserveAgain,
    /// The outstanding action could still land. Do not dispatch another.
    Wait(ActionId),
    /// Persist and then dispatch this newly issued conditional write.
    Apply(Action<T>),
    /// Fresh evidence matches this intent and no outstanding write can undo it.
    Converged(Revision),
}

/// Why a requested change was not made. A refusal leaves Sextant unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// An older desired revision cannot replace the current one.
    OlderIntent,
    /// This revision already names a different value.
    ConflictingIntent,
    /// The action ID is not the one currently outstanding.
    StaleAction,
    /// The scope has no IDs left.
    Exhausted,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OlderIntent => "older desired revision",
            Self::ConflictingIntent => "desired revision names a different value",
            Self::StaleAction => "action is not outstanding",
            Self::Exhausted => "action IDs exhausted",
        })
    }
}

impl core::error::Error for Refusal {}

/// Reconciliation state for one resource. Do not serve from two copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sextant<T> {
    intent: Intent<T>,
    pending: Option<Action<T>>,
    issuer: Issuer,
}

impl<T: Copy + Eq> Sextant<T> {
    /// Start with a scope not previously used for this resource's action IDs.
    #[must_use]
    pub const fn new(scope: u16, intent: Intent<T>) -> Self {
        Self {
            intent,
            pending: None,
            issuer: Issuer::new(scope),
        }
    }

    /// Rebuild from the adapter's latest atomic record. Returns `None` if the
    /// issuer watermark is invalid or the pending action contradicts it or the
    /// stored intent. The caller must prevent older incarnations from serving.
    #[must_use]
    pub fn restored(
        scope: u16,
        intent: Intent<T>,
        last_issued: Option<ActionId>,
        pending: Option<Action<T>>,
    ) -> Option<Self> {
        let issuer = Issuer::restored(scope, last_issued.map(ActionId::raw))?;
        if let Some(action) = pending
            && (Some(action.id) != last_issued
                || action.intent.revision > intent.revision
                || (action.intent.revision == intent.revision && action.intent != intent))
        {
            return None;
        }
        Some(Self {
            intent,
            pending,
            issuer,
        })
    }

    /// The latest desired revision and value.
    #[must_use]
    pub const fn intent(&self) -> Intent<T> {
        self.intent
    }

    /// The one action that might still take effect.
    #[must_use]
    pub const fn pending(&self) -> Option<Action<T>> {
        self.pending
    }

    /// The issuer watermark, including after an action has settled.
    #[must_use]
    pub const fn last_issued(&self) -> Option<ActionId> {
        match self.issuer.last() {
            Some(raw) => Some(ActionId(raw)),
            None => None,
        }
    }

    /// Set desired state without cancelling a write that may still land.
    /// Repeating identical intent is a no-op.
    ///
    /// # Errors
    /// Refuses an older revision or a different value for the same revision.
    pub fn set_intent(&mut self, intent: Intent<T>) -> Result<(), Refusal> {
        if intent.revision < self.intent.revision {
            return Err(Refusal::OlderIntent);
        }
        if intent.revision == self.intent.revision && intent != self.intent {
            return Err(Refusal::ConflictingIntent);
        }
        self.intent = intent;
        Ok(())
    }

    /// Consume a fresh observation or ask for one. For an outstanding write,
    /// the read must be taken after the observation that created the action.
    /// A changed resource token then makes that write permanently ineligible;
    /// the contents of a read alone cannot establish that fact.
    ///
    /// # Errors
    /// Refuses if a new action needs an ID and the issuer is exhausted.
    pub fn observe(&mut self, observation: Observation<T>) -> Result<Decision<T>, Refusal> {
        let Observation::Fresh { version, value } = observation else {
            return Ok(Decision::ObserveAgain);
        };
        if let Some(pending) = self.pending
            && version == pending.expected
        {
            return Ok(Decision::Wait(pending.id));
        }
        if value == Some(self.intent.value) {
            self.pending = None;
            return Ok(Decision::Converged(self.intent.revision));
        }
        let id = self.issuer.issue().map_err(|reason| match reason {
            periscope_machines::Refusal::Exhausted => Refusal::Exhausted,
        })?;
        let action = Action {
            id: ActionId(id),
            intent: self.intent,
            expected: version,
        };
        self.pending = Some(action);
        Ok(Decision::Apply(action))
    }

    /// Record authoritative terminal evidence for this exact action. The
    /// adapter must know the write finished and cannot take effect again, or
    /// that it can never take effect. A timeout is not such evidence.
    /// The receipt never proves convergence; observe the resource again.
    ///
    /// # Errors
    /// Refuses any ID other than the current outstanding action's.
    pub const fn settle(&mut self, id: ActionId) -> Result<(), Refusal> {
        match self.pending {
            Some(action) if action.id.raw() == id.raw() => {
                self.pending = None;
                Ok(())
            }
            _ => Err(Refusal::StaleAction),
        }
    }
}

#[cfg(test)]
mod tests;
