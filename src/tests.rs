use super::*;
use periscope_machines::{SEQUENCE_MAX, scoped};

const fn intent(revision: u64, value: u8) -> Intent<u8> {
    Intent {
        revision: NonZeroU64::new(revision).expect("nonzero test revision"),
        value,
    }
}

const fn fresh(version: u64, value: u8) -> Observation<u8> {
    Observation::Fresh {
        version: Version(version),
        value: Some(value),
    }
}

fn apply(machine: &mut Sextant<u8>, observation: Observation<u8>) -> Action<u8> {
    let Ok(Decision::Apply(action)) = machine.observe(observation) else {
        panic!("expected a new action");
    };
    action
}

#[test]
fn lost_reply_resolves_after_restart_only_on_fresh_proof() {
    let mut machine = Sextant::new(7, intent(2, 20));
    let action = apply(&mut machine, fresh(1, 10));
    let mut machine = Sextant::restored(
        7,
        machine.intent(),
        machine.last_issued(),
        machine.pending(),
    )
    .expect("valid persisted state");
    assert_eq!(
        machine.observe(Observation::Unknown),
        Ok(Decision::ObserveAgain)
    );
    assert_eq!(machine.observe(fresh(1, 10)), Ok(Decision::Wait(action.id)));
    assert_eq!(machine.pending(), Some(action));
    assert_eq!(
        machine.observe(fresh(2, 20)),
        Ok(Decision::Converged(action.intent.revision))
    );
    assert_eq!(machine.pending(), None);
    assert_eq!(machine.last_issued(), Some(action.id));
}

#[test]
fn newer_intent_does_not_cancel_the_old_write() {
    let mut machine = Sextant::new(7, intent(2, 20));
    let old = apply(&mut machine, fresh(1, 10));
    assert_eq!(machine.set_intent(intent(3, 10)), Ok(()));
    // The read matches v3, but the old write could still undo it.
    assert_eq!(machine.observe(fresh(1, 10)), Ok(Decision::Wait(old.id)));
    assert_eq!(machine.settle(old.id), Ok(()));
    assert_eq!(machine.pending(), None);
    let current = apply(&mut machine, fresh(2, 20));
    let before = machine.clone();
    assert_eq!(machine.settle(old.id), Err(Refusal::StaleAction));
    assert_eq!(
        machine.settle(ActionId(scoped(8, 2))),
        Err(Refusal::StaleAction)
    );
    assert_eq!(machine, before);
    assert_eq!(current.intent, intent(3, 10));
}

#[test]
fn a_changed_token_disarms_the_old_write_even_when_state_still_differs() {
    let mut machine = Sextant::new(7, intent(2, 20));
    let absent = Observation::Fresh {
        version: Version(1),
        value: None,
    };
    let old = apply(&mut machine, absent);
    let new = apply(
        &mut machine,
        Observation::Fresh {
            version: Version(2),
            value: None,
        },
    );
    assert_ne!(old.id, new.id);
    assert_eq!(new.expected, Version(2));
    assert_eq!(machine.settle(old.id), Err(Refusal::StaleAction));
}

#[test]
fn a_superseded_write_cannot_land_after_its_replacement_is_planned() {
    let mut machine = Sextant::new(7, intent(2, 20));
    let old = apply(&mut machine, fresh(1, 10));
    // Another writer advanced the token without installing our desired value.
    let mut resource = (Version(2), 15);
    let new = apply(&mut machine, fresh(resource.0.0, resource.1));
    let mut write = |action: Action<u8>| {
        if resource.0 != action.expected {
            return false;
        }
        resource = (Version(resource.0.0 + 1), action.intent.value);
        true
    };
    assert!(!write(old));
    assert!(write(new));
    assert_eq!(
        machine.observe(fresh(resource.0.0, resource.1)),
        Ok(Decision::Converged(new.intent.revision))
    );
}

#[test]
fn invalid_restore_and_refusals_do_not_reuse_or_spend_an_id() {
    let last = ActionId(scoped(7, SEQUENCE_MAX));
    assert!(Sextant::restored(8, intent(2, 20), Some(last), None).is_none());
    let action = Action {
        id: last,
        intent: intent(2, 21),
        expected: Version(1),
    };
    assert!(Sextant::restored(7, intent(2, 20), Some(last), Some(action)).is_none());
    let mut machine =
        Sextant::restored(7, intent(2, 20), Some(last), None).expect("valid watermark");
    let before = machine.clone();
    assert_eq!(machine.set_intent(intent(1, 20)), Err(Refusal::OlderIntent));
    assert_eq!(
        machine.set_intent(intent(2, 21)),
        Err(Refusal::ConflictingIntent)
    );
    assert_eq!(machine.observe(fresh(1, 10)), Err(Refusal::Exhausted));
    assert_eq!(machine, before);
}
