///|
/// Abstract sequence transition used to prove that accepted event positions
/// advance strictly. Mapping a persisted runtime event to this integer model
/// remains an explicitly documented trusted boundary.
pub fn accept_sequence(
  current : Int,
  incoming : Int,
) -> Int where {
  proof_require: 0 <= current,
  proof_require: current < incoming,
  proof_ensure: result => current < result,
  proof_ensure: result => result == incoming,
} {
  incoming
}

///|
/// Increment a bounded round counter. The caller changes to HardFailed when
/// this returns the sentinel `limit + 1`, so no unbounded Agent loop is
/// represented by the abstract core.
pub fn next_bounded_round(
  current : Int,
  limit : Int,
) -> Int where {
  proof_require: 0 <= current,
  proof_require: current <= limit,
  proof_require: 0 <= limit,
  proof_ensure: result => current < result,
  proof_ensure: result => result <= limit + 1,
} {
  current + 1
}

///|
/// A tool result consumes exactly one pending operation and cannot make the
/// abstract pending count negative.
pub fn consume_pending(
  pending : Int,
) -> Int where {
  proof_require: 0 < pending,
  proof_ensure: result => 0 <= result,
  proof_ensure: result => result < pending,
  proof_ensure: result => result == pending - 1,
} {
  pending - 1
}

///|
/// Cancellation clears all pending effects before the terminal event is
/// emitted.
pub fn cancel_pending(
  _pending : Int,
) -> Int where {
  proof_require: 0 <= _pending,
  proof_ensure: result => result == 0,
} {
  0
}

///|
/// Abstract publication gate. A report is publishable only from the verifying
/// state, with no blocking findings and at least one evidence reference.
pub fn publication_allowed(
  is_verifying : Bool,
  blocking_findings : Int,
  evidence_count : Int,
) -> Bool where {
  proof_require: 0 <= blocking_findings,
  proof_require: 0 <= evidence_count,
  proof_ensure: result => {
    result == (is_verifying && blocking_findings == 0 && evidence_count > 0)
  },
  proof_ensure: result => !result || is_verifying,
  proof_ensure: result => !result || blocking_findings == 0,
  proof_ensure: result => !result || evidence_count > 0,
} {
  is_verifying && blocking_findings == 0 && evidence_count > 0
}

///|
/// Terminal states reject all later ordinary events in the abstract model.
pub fn event_accepted(
  is_terminal : Bool,
  sequence_is_fresh : Bool,
) -> Bool where {
  proof_ensure: result => result == (!is_terminal && sequence_is_fresh),
  proof_ensure: result => !is_terminal || !result,
} {
  !is_terminal && sequence_is_fresh
}

///|
/// Reconcile is idempotent: a completed effect always stays completed and a
/// single durable result can only move a pending effect to completed once.
pub fn reconcile_completed(
  was_completed : Bool,
  has_durable_result : Bool,
) -> Bool where {
  proof_ensure: result => result == (was_completed || has_durable_result),
  proof_ensure: result => !was_completed || result,
} {
  was_completed || has_durable_result
}

///|
/// Abstract pending/completed accounting. A valid tool ledger never has a
/// negative bucket and never accounts for more operations than were issued.
pub fn pending_correspondence(
  pending : Int,
  completed : Int,
  issued : Int,
) -> Bool where {
  proof_require: 0 <= pending,
  proof_require: 0 <= completed,
  proof_require: 0 <= issued,
  proof_ensure: result => result == (pending + completed <= issued),
  proof_ensure: result => !result || pending <= issued,
  proof_ensure: result => !result || completed <= issued,
} {
  pending + completed <= issued
}

///|
/// A durable tool call can be completed exactly once. Replayed delivery is
/// rejected after the completed bit has been committed.
pub fn complete_once(
  was_completed : Bool,
  has_single_result : Bool,
) -> Bool where {
  proof_ensure: result => result == (!was_completed && has_single_result),
  proof_ensure: result => !result || !was_completed,
  proof_ensure: result => !result || has_single_result,
  proof_ensure: result => was_completed || !has_single_result || result,
} {
  !was_completed && has_single_result
}

///|
/// The abstract durable result counter is a set cardinality, not a delivery
/// count: duplicate delivery cannot increase it beyond one.
pub fn unique_result_count(
  was_completed : Bool,
  has_durable_result : Bool,
) -> Int where {
  proof_ensure: result => 0 <= result,
  proof_ensure: result => result <= 1,
  proof_ensure: result => (!was_completed && !has_durable_result) || result == 1,
  proof_ensure: result => was_completed || has_durable_result || result == 0,
} {
  if was_completed || has_durable_result {
    1
  } else {
    0
  }
}

///|
/// Replaying the same abstract state/event pair is deterministic. The runtime
/// event decoder and full production state remain a documented trusted
/// refinement boundary.
pub fn replay_deterministic(
  first_state : Int,
  replay_state : Int,
  first_event : Int,
  replay_event : Int,
) -> Bool where {
  proof_require: first_state == replay_state,
  proof_require: first_event == replay_event,
  proof_ensure: result => {
    result == (first_state + first_event == replay_state + replay_event)
  },
  proof_ensure: result => result,
} {
  first_state + first_event == replay_state + replay_event
}

///|
/// Applying reconcile twice with the same durable fact has the same result as
/// applying it once.
pub fn reconcile_idempotent(
  was_completed : Bool,
  has_durable_result : Bool,
) -> Bool where {
  proof_ensure: result => result == true,
} {
  let once = was_completed || has_durable_result
  let twice = once || has_durable_result
  (once && twice) || (!once && !twice)
}

///|
/// Context compression is publishable only when its required structural
/// sections survive: TaskSpec, evidence index, pending-effect ledger and the
/// visible summary. Content selection correctness remains property-tested.
pub fn compression_structure_complete(
  has_task_spec : Bool,
  has_evidence_index : Bool,
  has_pending_ledger : Bool,
  has_visible_summary : Bool,
) -> Bool where {
  proof_ensure: result => {
    result ==
    (
      has_task_spec &&
      has_evidence_index &&
      has_pending_ledger &&
      has_visible_summary
    )
  },
  proof_ensure: result => !result || has_task_spec,
  proof_ensure: result => !result || has_evidence_index,
  proof_ensure: result => !result || has_pending_ledger,
  proof_ensure: result => !result || has_visible_summary,
} {
  has_task_spec &&
  has_evidence_index &&
  has_pending_ledger &&
  has_visible_summary
}
