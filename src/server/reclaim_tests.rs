//! A full server takes back one idle connection per waiting client, the oldest first.

use super::*;
use std::task::{Context, Waker};

fn woken(turn: &mut IdleTurn<'_>) -> bool {
    turn.turn
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_ready()
}

/// Enabling takes a stored wake, and dropping the `Notified` afterwards would lose it.
#[test]
fn a_stored_wake_taken_by_a_connection_that_leaves_reaches_the_next_in_line() {
    let slots = SlotReclaim::default();
    let _request = slots.request();
    drop(slots.join_line());
    let mut next = slots.join_line();
    assert!(woken(&mut next));
    assert!(slots.take_request());
}

#[test]
fn a_request_wakes_only_the_connection_that_turned_idle_first() {
    let slots = SlotReclaim::default();
    let mut oldest = slots.join_line();
    let mut younger = slots.join_line();
    let _request = slots.request();
    assert!(woken(&mut oldest));
    assert!(!woken(&mut younger));
    assert!(slots.take_request());
}

#[test]
fn a_full_server_without_a_waiting_client_wakes_nobody() {
    let slots = SlotReclaim::default();
    let mut idle = slots.join_line();
    assert!(!woken(&mut idle));
    assert!(!slots.is_wanted());
}

#[test]
fn a_request_with_nobody_idle_reaches_the_next_connection_to_turn_idle() {
    let slots = SlotReclaim::default();
    let _request = slots.request();
    let mut idle = slots.join_line();
    assert!(woken(&mut idle));
    assert!(slots.take_request());
}

#[test]
fn a_connection_leaving_the_line_passes_its_wake_on() {
    let slots = SlotReclaim::default();
    let oldest = slots.join_line();
    let mut younger = slots.join_line();
    let _request = slots.request();
    drop(oldest);
    assert!(woken(&mut younger));
}

#[test]
fn a_busy_connection_can_hand_its_wake_to_the_next_in_line() {
    let slots = SlotReclaim::default();
    let mut oldest = slots.join_line();
    let mut younger = slots.join_line();
    let _request = slots.request();
    assert!(woken(&mut oldest));
    drop(oldest);
    assert!(woken(&mut younger));
    assert!(slots.take_request());
}

#[test]
fn a_wake_that_outlives_its_request_closes_nothing() {
    let slots = SlotReclaim::default();
    drop(slots.request());
    let mut idle = slots.join_line();
    assert!(woken(&mut idle));
    assert!(!slots.is_wanted());
    assert!(!slots.take_request());
}

#[test]
fn only_one_connection_takes_a_request() {
    let slots = SlotReclaim::default();
    let _request = slots.request();
    assert!(slots.take_request());
    assert!(!slots.take_request());
}

/// A place in line belongs to the idle phase, so leaving it passes on a wake in any direction.
#[test]
fn leaving_the_idle_phase_gives_up_the_place_in_line() {
    let slots = SlotReclaim::default();
    let oldest = ConnectionPhase::Idle {
        turn: slots.join_line(),
    };
    let mut younger = slots.join_line();
    let _request = slots.request();
    assert!(!woken(&mut younger));
    drop(oldest);
    assert!(woken(&mut younger));
}
