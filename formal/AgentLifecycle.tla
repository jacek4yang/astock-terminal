--------------------------- MODULE AgentLifecycle ---------------------------
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Tools, MaxRetries, MaxCrashes, MaxSeq

Phases == {
  "Idle", "Preparing", "WaitingForUser", "Reasoning", "AwaitingTools", "Reviewing",
  "Synthesizing", "Verifying", "VerificationFailed", "Suspended",
  "Completed", "Cancelled", "HardFailed"
}

Terminal == {"Completed", "VerificationFailed", "Cancelled", "HardFailed"}

VARIABLES phase, seq, pending, completed, resultCount, retries,
          evidenceCount, published, checkpointed, crashed, crashCount

vars == <<phase, seq, pending, completed, resultCount, retries,
          evidenceCount, published, checkpointed, crashed, crashCount>>

Init ==
  /\ phase = "Idle"
  /\ seq = 0
  /\ pending = {}
  /\ completed = {}
  /\ resultCount = [t \in Tools |-> 0]
  /\ retries = [t \in Tools |-> 0]
  /\ evidenceCount = 0
  /\ published = FALSE
  /\ checkpointed = TRUE
  /\ crashed = FALSE
  /\ crashCount = 0

Commit(nextPhase) ==
  /\ seq < MaxSeq
  /\ phase' = nextPhase
  /\ seq' = seq + 1
  /\ checkpointed' = TRUE
  /\ crashed' = crashed
  /\ crashCount' = crashCount

Start ==
  /\ phase = "Idle"
  /\ Commit("Preparing")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

Prepared ==
  /\ phase = "Preparing"
  /\ Commit("Reasoning")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

NeedClarification ==
  /\ phase = "Preparing"
  /\ Commit("WaitingForUser")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

AnswerClarification ==
  /\ phase = "WaitingForUser"
  /\ Commit("Preparing")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

RequestTool(t) ==
  /\ phase = "Reasoning"
  /\ t \notin pending
  /\ t \notin completed
  /\ Commit("AwaitingTools")
  /\ pending' = pending \cup {t}
  /\ UNCHANGED <<completed, resultCount, retries, evidenceCount, published>>

ToolResult(t) ==
  /\ t \in pending
  /\ Commit(IF pending = {t} THEN "Reviewing" ELSE "AwaitingTools")
  /\ pending' = pending \ {t}
  /\ completed' = completed \cup {t}
  /\ resultCount' = [resultCount EXCEPT ![t] = @ + 1]
  /\ evidenceCount' = evidenceCount + 1
  /\ retries' = [retries EXCEPT ![t] = 0]
  /\ UNCHANGED published

RetryTool(t) ==
  /\ t \in pending
  /\ retries[t] < MaxRetries
  /\ seq < MaxSeq
  /\ seq' = seq + 1
  /\ retries' = [retries EXCEPT ![t] = @ + 1]
  /\ checkpointed' = TRUE
  /\ UNCHANGED <<phase, pending, completed, resultCount, evidenceCount, published, crashed, crashCount>>

Review ==
  /\ phase = "Reviewing"
  /\ Commit("Synthesizing")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

Synthesize ==
  /\ phase = "Synthesizing"
  /\ Commit("Verifying")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

VerifyPass ==
  /\ phase = "Verifying"
  /\ evidenceCount > 0
  /\ Commit("Completed")
  /\ published' = TRUE
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount>>

VerifyReject ==
  /\ phase = "Verifying"
  /\ Commit("VerificationFailed")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

Suspend ==
  /\ phase \in {"Reasoning", "AwaitingTools"}
  /\ Commit("Suspended")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

Resume ==
  /\ phase = "Suspended"
  /\ Commit("Reasoning")
  /\ UNCHANGED <<pending, completed, resultCount, retries, evidenceCount, published>>

Cancel ==
  /\ phase \notin Terminal
  /\ Commit("Cancelled")
  /\ pending' = {}
  /\ UNCHANGED <<completed, resultCount, retries, evidenceCount, published>>

HardFail ==
  /\ phase \notin Terminal
  /\ Commit("HardFailed")
  /\ pending' = {}
  /\ UNCHANGED <<completed, resultCount, retries, evidenceCount, published>>

Crash ==
  /\ phase \notin Terminal
  /\ checkpointed
  /\ ~crashed
  /\ crashCount < MaxCrashes
  /\ crashed' = TRUE
  /\ crashCount' = crashCount + 1
  /\ UNCHANGED <<phase, seq, pending, completed, resultCount, retries,
                  evidenceCount, published, checkpointed>>

Restart ==
  /\ crashed
  /\ crashed' = FALSE
  /\ UNCHANGED <<phase, seq, pending, completed, resultCount, retries,
                  evidenceCount, published, checkpointed, crashCount>>

DuplicateOrStaleFrame == UNCHANGED vars

ForwardStep ==
  /\ ~crashed
  /\ (\/ Start
      \/ Prepared
      \/ NeedClarification
      \/ AnswerClarification
      \/ (\E t \in Tools: RequestTool(t))
      \/ (\E t \in Tools: ToolResult(t))
      \/ (\E t \in Tools: RetryTool(t))
      \/ Review
      \/ Synthesize
      \/ VerifyPass
      \/ VerifyReject
      \/ Suspend
      \/ Resume
      \/ Cancel
      \/ HardFail)

Next ==
  \/ ForwardStep
  \/ Crash
  \/ Restart
  \/ DuplicateOrStaleFrame

Spec == Init /\ [][Next]_vars
ProgressSpec ==
  /\ Spec
  /\ WF_vars(ForwardStep)
  /\ WF_vars(Restart)

TypeOK ==
  /\ phase \in Phases
  /\ seq \in Nat
  /\ seq <= MaxSeq
  /\ pending \subseteq Tools
  /\ completed \subseteq Tools
  /\ resultCount \in [Tools -> Nat]
  /\ retries \in [Tools -> 0..MaxRetries]
  /\ evidenceCount \in Nat
  /\ published \in BOOLEAN
  /\ checkpointed \in BOOLEAN
  /\ crashed \in BOOLEAN
  /\ crashCount \in 0..MaxCrashes

PendingCorrespondence == pending \cap completed = {}
ResultUniqueness == \A t \in Tools: resultCount[t] <= 1
CompletedHasOneResult == \A t \in completed: resultCount[t] = 1
CancellationSafety == phase = "Cancelled" => pending = {}
FailureSafety == phase = "HardFailed" => pending = {}
ReleaseSafety == published => (phase = "Completed" /\ evidenceCount > 0)
NoUnpublishedCompletion == phase = "Completed" => published
RetryBounded == \A t \in Tools: retries[t] <= MaxRetries

SeqDoesNotDecrease == seq' >= seq
TerminalDoesNotChange == (phase \in Terminal) => phase' = phase
SeqMonotonic == [][SeqDoesNotDecrease]_vars
TerminalAbsorbing == [][TerminalDoesNotChange]_vars
FiniteRangeLiveness == <>((phase \in Terminal) \/ (seq = MaxSeq))

=============================================================================
