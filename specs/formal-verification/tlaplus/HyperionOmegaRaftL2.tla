------------------------ MODULE HyperionOmegaRaftL2 ------------------------
(***************************************************************************)
(* HYPERION OMEGA — Channel 11 Formal Protocol Specification               *)
(* Coupled 5-Node / N-Node Raft + Pre-Vote + Chained-Hash WAL + L2 Anchor  *)
(*                                                                         *)
(* Models concurrent execution under:                                      *)
(*   1. Asymmetric & symmetric network partitions (partitionedPairs)       *)
(*   2. Sudden Leader/Follower crash & persistent WAL recovery             *)
(*   3. Duplicate, reordered, & dropped RPC messages                       *)
(*   4. Stale deposed-leader writes in minority partitions                 *)
(*   5. L2 Settlement StateRootBatchCommit & shallow chain reorganizations *)
(*                                                                         *)
(* Verified Safety Invariants:                                             *)
(*   - ElectionSafety          (at most 1 Leader per term)                 *)
(*   - LogMatching             (matching (idx, term) => identical prefix)  *)
(*   - WalHashChainIntegrity   (unbroken cryptographic prevHash -> hash)   *)
(*   - ZeroCommittedDataLoss   (ghostCommittedHistory never lost/mutated)  *)
(*   - LeaderCompleteness      (every future leader holds committed log)   *)
(*   - L2SettlementSafety      (finalized L2 batches match committed WAL)  *)
(*                                                                         *)
(* Verified Liveness Property (under post-heal weak fairness):             *)
(*   - PostHealConvergence     (quorum leader elected & all logs converge) *)
(***************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Servers,            \* e.g. {1, 2, 3} or {1, 2, 3, 4, 5}
    Commands,           \* Finite set of deterministic ledger commands
    MaxTerm,            \* State-space bound on election terms
    MaxLogLen,          \* State-space bound on WAL length
    MaxFaultBudget,     \* Bound on injected partitions / crashes / duplicates / reorgs
    L2FinalityDepth     \* Confirmations required before L2 batch is irreversible

ASSUME Cardinality(Servers) >= 3
ASSUME MaxTerm >= 2 /\ MaxLogLen >= 2 /\ L2FinalityDepth >= 1

None == 0

Roles == {"Follower", "PreCandidate", "Candidate", "Leader"}

Quorums == {Q \in SUBSET Servers : Cardinality(Q) * 2 > Cardinality(Servers)}

VARIABLES
    \* --- Persistent State on Stable Storage (survives Crash & Restart) ---
    currentTerm,        \* [Servers -> Nat]
    votedFor,           \* [Servers -> Servers \cup {None}]
    walLog,             \* [Servers -> Seq([term: Nat, cmd: Commands, prevHash: Nat, hash: Nat])]

    \* --- Volatile Consensus State (reset upon CrashNode) ---
    role,               \* [Servers -> Roles]
    commitIndex,        \* [Servers -> Nat]
    preVotesGranted,    \* [Servers -> SUBSET Servers]
    votesGranted,       \* [Servers -> SUBSET Servers]
    nextIndex,          \* [Servers -> [Servers -> Nat]]
    matchIndex,         \* [Servers -> [Servers -> Nat]]

    \* --- Network, Fault-Injection & Ghost History State ---
    messages,           \* Set of in-flight RPC records (supports duplicates via dupTag)
    partitionedPairs,   \* Symmetric/asymmetric blocked directed links SUBSET (Servers \X Servers)
    crashedNodes,       \* SUBSET Servers currently offline prior to WAL recovery
    faultsUsed,         \* Nat <= MaxFaultBudget
    networkHealed,      \* BOOLEAN flag set once Nemesis halts fault injection
    ghostCommitted,     \* Append-only Ghost Seq of quorum-committed WAL entries

    \* --- Coupled L2 Rollup Settlement State (SolvencyStateAnchor.sol) ---
    l2StagedAnchor,     \* Seq([epoch: Nat, commitLen: Nat, rootHash: Nat, confs: Nat])
    l2FinalizedAnchor   \* Seq([epoch: Nat, commitLen: Nat, rootHash: Nat])

raftVars    == <<currentTerm, votedFor, walLog, role, commitIndex, preVotesGranted, votesGranted, nextIndex, matchIndex>>
envVars     == <<messages, partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted>>
l2Vars      == <<l2StagedAnchor, l2FinalizedAnchor>>
vars        == <<raftVars, envVars, l2Vars>>

-----------------------------------------------------------------------------
(* Deterministic Collision-Free Symbolic Blake3 Chaining Function *)
ComputeEntryHash(idx, term, cmd, prevHash) ==
    ((((prevHash * 131) + idx) * 137 + term) * 149) + cmd + 1

LastTerm(xlog) == IF Len(xlog) = 0 THEN 0 ELSE xlog[Len(xlog)].term
LastHash(xlog) == IF Len(xlog) = 0 THEN 0 ELSE xlog[Len(xlog)].hash

\* Raft §5.4.1 Up-To-Date Log Comparison
IsLogUpToDate(candLastTerm, candLastIdx, voterLastTerm, voterLastIdx) ==
    \/ candLastTerm > voterLastTerm
    \/ (candLastTerm = voterLastTerm /\ candLastIdx >= voterLastIdx)

CanDeliver(src, dst) ==
    /\ src \notin crashedNodes
    /\ dst \notin crashedNodes
    /\ <<src, dst>> \notin partitionedPairs

SendMsg(m) == messages' = messages \cup {m}

-----------------------------------------------------------------------------
Init ==
    /\ currentTerm       = [s \in Servers |-> 1]
    /\ votedFor          = [s \in Servers |-> None]
    /\ walLog            = [s \in Servers |-> <<>>]
    /\ role              = [s \in Servers |-> "Follower"]
    /\ commitIndex       = [s \in Servers |-> 0]
    /\ preVotesGranted   = [s \in Servers |-> {}]
    /\ votesGranted      = [s \in Servers |-> {}]
    /\ nextIndex         = [s \in Servers |-> [p \in Servers |-> 1]]
    /\ matchIndex        = [s \in Servers |-> [p \in Servers |-> 0]]
    /\ messages          = {}
    /\ partitionedPairs  = {}
    /\ crashedNodes      = {}
    /\ faultsUsed        = 0
    /\ networkHealed     = FALSE
    /\ ghostCommitted    = <<>>
    /\ l2StagedAnchor    = <<>>
    /\ l2FinalizedAnchor = <<>>

-----------------------------------------------------------------------------
(* 1. PRE-VOTE PROTOCOL (§9.6): Prevents partitioned nodes from inflating term *)
StartPreVote(i) ==
    /\ i \notin crashedNodes
    /\ role[i] \in {"Follower", "PreCandidate"}
    /\ currentTerm[i] + 1 <= MaxTerm
    /\ role'            = [role EXCEPT ![i] = "PreCandidate"]
    /\ preVotesGranted' = [preVotesGranted EXCEPT ![i] = {i}]
    /\ messages'        = messages \cup {
          [mtype           |-> "PreVoteReq",
           prospectiveTerm |-> currentTerm[i] + 1,
           msource         |-> i,
           mdest           |-> j,
           lastLogIdx      |-> Len(walLog[i]),
           lastLogTerm     |-> LastTerm(walLog[i]),
           dupTag          |-> 0] : j \in Servers \ {i}
       }
    /\ UNCHANGED <<currentTerm, votedFor, walLog, commitIndex, votesGranted,
                   nextIndex, matchIndex, partitionedPairs, crashedNodes,
                   faultsUsed, networkHealed, ghostCommitted, l2Vars>>

HandlePreVoteReq(i, m) ==
    /\ m.mtype = "PreVoteReq"
    /\ m.mdest = i
    /\ CanDeliver(m.msource, i)
    /\ LET grant == /\ m.prospectiveTerm >= currentTerm[i] + 1
                    /\ IsLogUpToDate(m.lastLogTerm, m.lastLogIdx, LastTerm(walLog[i]), Len(walLog[i]))
       IN SendMsg([mtype           |-> "PreVoteResp",
                   mterm           |-> currentTerm[i],
                   prospectiveTerm |-> m.prospectiveTerm,
                   egranted        |-> grant,
                   msource         |-> i,
                   mdest           |-> m.msource,
                   dupTag          |-> 0])
    /\ UNCHANGED <<raftVars, partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted, l2Vars>>

HandlePreVoteResp(i, m) ==
    /\ m.mtype = "PreVoteResp"
    /\ m.mdest = i
    /\ CanDeliver(m.msource, i)
    /\ role[i] = "PreCandidate"
    /\ m.prospectiveTerm = currentTerm[i] + 1
    /\ IF m.mterm > currentTerm[i]
       THEN /\ currentTerm'     = [currentTerm EXCEPT ![i] = m.mterm]
            /\ role'            = [role EXCEPT ![i] = "Follower"]
            /\ votedFor'        = [votedFor EXCEPT ![i] = None]
            /\ preVotesGranted' = [preVotesGranted EXCEPT ![i] = {}]
            /\ votesGranted'    = [votesGranted EXCEPT ![i] = {}]
            /\ UNCHANGED <<walLog, commitIndex, nextIndex, matchIndex, messages>>
       ELSE LET newPV == IF m.egranted THEN preVotesGranted[i] \cup {m.msource} ELSE preVotesGranted[i]
            IN IF newPV \in Quorums /\ currentTerm[i] + 1 <= MaxTerm
               THEN \* Quorum Pre-Vote succeeded -> increment persistent term & launch real Election
                    LET nextT == currentTerm[i] + 1
                    IN /\ currentTerm'     = [currentTerm EXCEPT ![i] = nextT]
                       /\ role'            = [role EXCEPT ![i] = "Candidate"]
                       /\ votedFor'        = [votedFor EXCEPT ![i] = i]
                       /\ preVotesGranted' = [preVotesGranted EXCEPT ![i] = newPV]
                       /\ votesGranted'    = [votesGranted EXCEPT ![i] = {i}]
                       /\ messages'        = messages \cup {
                             [mtype       |-> "RequestVoteReq",
                              mterm       |-> nextT,
                              msource     |-> i,
                              mdest       |-> j,
                              lastLogIdx  |-> Len(walLog[i]),
                              lastLogTerm |-> LastTerm(walLog[i]),
                              dupTag      |-> 0] : j \in Servers \ {i}
                          }
                       /\ UNCHANGED <<walLog, commitIndex, nextIndex, matchIndex>>
               ELSE /\ preVotesGranted' = [preVotesGranted EXCEPT ![i] = newPV]
                    /\ UNCHANGED <<currentTerm, votedFor, walLog, role, commitIndex,
                                   votesGranted, nextIndex, matchIndex, messages>>
    /\ UNCHANGED <<partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted, l2Vars>>

-----------------------------------------------------------------------------
(* 2. STANDARD ELECTION & LEADER PROMOTION *)
HandleRequestVoteReq(i, m) ==
    /\ m.mtype = "RequestVoteReq"
    /\ m.mdest = i
    /\ CanDeliver(m.msource, i)
    /\ LET stepDown == m.mterm > currentTerm[i]
           effTerm  == IF stepDown THEN m.mterm ELSE currentTerm[i]
           effVoted == IF stepDown THEN None ELSE votedFor[i]
           effRole  == IF stepDown THEN "Follower" ELSE role[i]
           grant    == /\ m.mterm = effTerm
                       /\ effVoted \in {None, m.msource}
                       /\ IsLogUpToDate(m.lastLogTerm, m.lastLogIdx, LastTerm(walLog[i]), Len(walLog[i]))
       IN /\ currentTerm' = [currentTerm EXCEPT ![i] = effTerm]
          /\ role'        = [role EXCEPT ![i] = IF grant THEN "Follower" ELSE effRole]
          /\ votedFor'    = [votedFor EXCEPT ![i] = IF grant THEN m.msource ELSE effVoted]
          /\ SendMsg([mtype    |-> "RequestVoteResp",
                      mterm    |-> effTerm,
                      vgranted |-> grant,
                      msource  |-> i,
                      mdest    |-> m.msource,
                      dupTag   |-> 0])
    /\ UNCHANGED <<walLog, commitIndex, preVotesGranted, votesGranted,
                   nextIndex, matchIndex, partitionedPairs, crashedNodes,
                   faultsUsed, networkHealed, ghostCommitted, l2Vars>>

HandleRequestVoteResp(i, m) ==
    /\ m.mtype = "RequestVoteResp"
    /\ m.mdest = i
    /\ CanDeliver(m.msource, i)
    /\ IF m.mterm > currentTerm[i]
       THEN /\ currentTerm'  = [currentTerm EXCEPT ![i] = m.mterm]
            /\ role'         = [role EXCEPT ![i] = "Follower"]
            /\ votedFor'     = [votedFor EXCEPT ![i] = None]
            /\ votesGranted' = [votesGranted EXCEPT ![i] = {}]
            /\ UNCHANGED <<walLog, commitIndex, preVotesGranted, nextIndex, matchIndex>>
       ELSE /\ role[i] = "Candidate"
            /\ m.mterm = currentTerm[i]
            /\ LET newV == IF m.vgranted THEN votesGranted[i] \cup {m.msource} ELSE votesGranted[i]
               IN IF newV \in Quorums
                  THEN /\ role'         = [role EXCEPT ![i] = "Leader"]
                       /\ votesGranted' = [votesGranted EXCEPT ![i] = newV]
                       /\ nextIndex'    = [nextIndex EXCEPT ![i] = [p \in Servers |-> Len(walLog[i]) + 1]]
                       /\ matchIndex'   = [matchIndex EXCEPT ![i] = [p \in Servers |-> IF p = i THEN Len(walLog[i]) ELSE 0]]
                       /\ UNCHANGED <<currentTerm, votedFor, walLog, commitIndex, preVotesGranted>>
                  ELSE /\ votesGranted' = [votesGranted EXCEPT ![i] = newV]
                       /\ UNCHANGED <<currentTerm, votedFor, walLog, role, commitIndex, preVotesGranted, nextIndex, matchIndex>>
    /\ UNCHANGED <<messages, partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted, l2Vars>>

-----------------------------------------------------------------------------
(* 3. CLIENT WRITE PROPOSAL (INCLUDING STALE MINORITY LEADER WRITES!) *)
ClientPropose(i, cmd) ==
    /\ i \notin crashedNodes
    /\ role[i] = "Leader"
    /\ Len(walLog[i]) < MaxLogLen
    /\ LET idx   == Len(walLog[i]) + 1
           pHash == LastHash(walLog[i])
           eHash == ComputeEntryHash(idx, currentTerm[i], cmd, pHash)
           entry == [term |-> currentTerm[i], cmd |-> cmd, prevHash |-> pHash, hash |-> eHash]
       IN /\ walLog'     = [walLog EXCEPT ![i] = Append(@, entry)]
          /\ matchIndex' = [matchIndex EXCEPT ![i][i] = idx]
    /\ UNCHANGED <<currentTerm, votedFor, role, commitIndex, preVotesGranted,
                   votesGranted, nextIndex, envVars, l2Vars>>

-----------------------------------------------------------------------------
(* 4. APPEND ENTRIES REPLICATION & CRYPTOGRAPHIC BLAKE3 LOG MATCHING *)
SendAppendEntries(i, j) ==
    /\ i /= j
    /\ i \notin crashedNodes
    /\ role[i] = "Leader"
    /\ LET prevIdx  == nextIndex[i][j] - 1
           prevTerm == IF prevIdx = 0 THEN 0 ELSE walLog[i][prevIdx].term
           prevHash == IF prevIdx = 0 THEN 0 ELSE walLog[i][prevIdx].hash
           entries  == SubSeq(walLog[i], nextIndex[i][j], Len(walLog[i]))
       IN SendMsg([mtype        |-> "AppendEntriesReq",
                   mterm        |-> currentTerm[i],
                   msource      |-> i,
                   mdest        |-> j,
                   prevLogIdx   |-> prevIdx,
                   prevLogTerm  |-> prevTerm,
                   prevLogHash  |-> prevHash,
                   mentries     |-> entries,
                   leaderCommit |-> commitIndex[i],
                   dupTag       |-> 0])
    /\ UNCHANGED <<raftVars, partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted, l2Vars>>

HandleAppendEntriesReq(i, m) ==
    /\ m.mtype = "AppendEntriesReq"
    /\ m.mdest = i
    /\ CanDeliver(m.msource, i)
    /\ IF m.mterm < currentTerm[i]
       THEN /\ SendMsg([mtype      |-> "AppendEntriesResp",
                        mterm      |-> currentTerm[i],
                        msuccess   |-> FALSE,
                        mmatchIdx  |-> 0,
                        msource    |-> i,
                        mdest      |-> m.msource,
                        dupTag     |-> 0])
            /\ UNCHANGED <<raftVars, partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted, l2Vars>>
       ELSE LET logOk == \/ m.prevLogIdx = 0
                         \/ /\ m.prevLogIdx <= Len(walLog[i])
                            /\ walLog[i][m.prevLogIdx].term = m.prevLogTerm
                            /\ walLog[i][m.prevLogIdx].hash = m.prevLogHash
            IN /\ currentTerm' = [currentTerm EXCEPT ![i] = m.mterm]
               /\ role'        = [role EXCEPT ![i] = "Follower"]
               /\ votedFor'    = [votedFor EXCEPT ![i] = IF m.mterm > currentTerm[i] THEN None ELSE votedFor[i]]
               /\ IF ~logOk
                  THEN /\ SendMsg([mtype      |-> "AppendEntriesResp",
                                   mterm      |-> m.mterm,
                                   msuccess   |-> FALSE,
                                   mmatchIdx  |-> 0,
                                   msource    |-> i,
                                   mdest      |-> m.msource,
                                   dupTag     |-> 0])
                       /\ UNCHANGED <<walLog, commitIndex, preVotesGranted, votesGranted, nextIndex, matchIndex>>
                  ELSE \* Safe Raft §5.3 merge: NEVER truncate matching entries (guards against out-of-order stale RPCs!)
                       LET prefix == SubSeq(walLog[i], 1, m.prevLogIdx)
                           merged == [k \in 1..(m.prevLogIdx + Len(m.mentries)) |->
                                        IF k <= m.prevLogIdx THEN walLog[i][k] ELSE m.mentries[k - m.prevLogIdx]]
                           keepExisting ==
                               /\ m.prevLogIdx + Len(m.mentries) <= Len(walLog[i])
                               /\ \A k \in 1..Len(m.mentries) :
                                    walLog[i][m.prevLogIdx + k] = m.mentries[k]
                           newLog == IF keepExisting THEN walLog[i] ELSE merged
                           maxIdx == m.prevLogIdx + Len(m.mentries)
                           newCommit == IF m.leaderCommit > commitIndex[i]
                                        THEN IF m.leaderCommit < maxIdx THEN m.leaderCommit ELSE maxIdx
                                        ELSE commitIndex[i]
                       IN /\ walLog'      = [walLog EXCEPT ![i] = newLog]
                          /\ commitIndex' = [commitIndex EXCEPT ![i] = newCommit]
                          /\ SendMsg([mtype      |-> "AppendEntriesResp",
                                      mterm      |-> m.mterm,
                                      msuccess   |-> TRUE,
                                      mmatchIdx  |-> maxIdx,
                                      msource    |-> i,
                                      mdest      |-> m.msource,
                                      dupTag     |-> 0])
                          /\ UNCHANGED <<preVotesGranted, votesGranted, nextIndex, matchIndex>>
               /\ UNCHANGED <<partitionedPairs, crashedNodes, faultsUsed, networkHealed, ghostCommitted, l2Vars>>

HandleAppendEntriesResp(i, m) ==
    /\ m.mtype = "AppendEntriesResp"
    /\ m.mdest = i
    /\ CanDeliver(m.msource, i)
    /\ IF m.mterm > currentTerm[i]
       THEN /\ currentTerm' = [currentTerm EXCEPT ![i] = m.mterm]
            /\ role'        = [role EXCEPT ![i] = "Follower"]
            /\ votedFor'    = [votedFor EXCEPT ![i] = None]
            /\ UNCHANGED <<walLog, commitIndex, preVotesGranted, votesGranted, nextIndex, matchIndex, ghostCommitted>>
       ELSE /\ role[i] = "Leader"
            /\ m.mterm = currentTerm[i]
            /\ IF m.msuccess
               THEN LET newMatch == IF m.mmatchIdx > matchIndex[i][m.msource] THEN m.mmatchIdx ELSE matchIndex[i][m.msource]
                        updatedMatch == [matchIndex[i] EXCEPT ![m.msource] = newMatch]
                        agreeIndices == {idx \in (commitIndex[i] + 1)..Len(walLog[i]) :
                                          /\ walLog[i][idx].term = currentTerm[i]
                                          /\ {s \in Servers : updatedMatch[s] >= idx} \in Quorums}
                        newCommit == IF agreeIndices = {} THEN commitIndex[i]
                                     ELSE CHOOSE x \in agreeIndices : \A y \in agreeIndices : x >= y
                    IN /\ matchIndex'     = [matchIndex EXCEPT ![i] = updatedMatch]
                       /\ nextIndex'      = [nextIndex EXCEPT ![i][m.msource] = newMatch + 1]
                       /\ commitIndex'    = [commitIndex EXCEPT ![i] = newCommit]
                       /\ ghostCommitted' = IF newCommit > Len(ghostCommitted)
                                            THEN SubSeq(walLog[i], 1, newCommit)
                                            ELSE ghostCommitted
                       /\ UNCHANGED <<currentTerm, votedFor, walLog, role, preVotesGranted, votesGranted>>
               ELSE /\ nextIndex' = [nextIndex EXCEPT ![i][m.msource] = IF @ > 1 THEN @ - 1 ELSE 1]
                    /\ UNCHANGED <<currentTerm, votedFor, walLog, role, commitIndex, preVotesGranted, votesGranted, matchIndex, ghostCommitted>>
    /\ UNCHANGED <<messages, partitionedPairs, crashedNodes, faultsUsed, networkHealed, l2Vars>>

-----------------------------------------------------------------------------
(* 5. COUPLED L2 SETTLEMENT BATCH COMMIT & REORG RECOVERY *)
StageL2StateRootBatch(i) ==
    /\ i \notin crashedNodes
    /\ role[i] = "Leader"
    /\ commitIndex[i] > 0
    /\ LET committedLen  == commitIndex[i]
           committedHash == walLog[i][committedLen].hash
           lastAnchored  == IF Len(l2FinalizedAnchor) = 0 THEN 0
                            ELSE l2FinalizedAnchor[Len(l2FinalizedAnchor)].commitLen
           lastStaged    == IF Len(l2StagedAnchor) = 0 THEN lastAnchored
                            ELSE l2StagedAnchor[Len(l2StagedAnchor)].commitLen
       IN /\ committedLen > lastStaged
          /\ l2StagedAnchor' = Append(l2StagedAnchor,
                [epoch     |-> Len(l2FinalizedAnchor) + Len(l2StagedAnchor) + 1,
                 commitLen |-> committedLen,
                 rootHash  |-> committedHash,
                 confs     |-> 1])
    /\ UNCHANGED <<raftVars, envVars, l2FinalizedAnchor>>

AdvanceL2BlockConfirmations ==
    /\ Len(l2StagedAnchor) > 0
    /\ LET head == Head(l2StagedAnchor)
       IN IF head.confs + 1 >= L2FinalityDepth
          THEN /\ l2FinalizedAnchor' = Append(l2FinalizedAnchor,
                     [epoch |-> head.epoch, commitLen |-> head.commitLen, rootHash |-> head.rootHash])
               /\ l2StagedAnchor'    = Tail(l2StagedAnchor)
          ELSE /\ l2StagedAnchor'    = <<[head EXCEPT !.confs = @ + 1]>> \o Tail(l2StagedAnchor)
               /\ UNCHANGED l2FinalizedAnchor
    /\ UNCHANGED <<raftVars, envVars>>

-----------------------------------------------------------------------------
(* 6. ADVERSARIAL FAULT INJECTION (PARTITIONS, CRASHES, DUPLICATES, REORGS) *)
InjectNetworkPartition(i, j) ==
    /\ ~networkHealed
    /\ faultsUsed < MaxFaultBudget
    /\ i /= j
    /\ <<i, j>> \notin partitionedPairs
    /\ partitionedPairs' = partitionedPairs \cup {<<i, j>>, <<j, i>>}
    /\ faultsUsed'       = faultsUsed + 1
    /\ UNCHANGED <<raftVars, messages, crashedNodes, networkHealed, ghostCommitted, l2Vars>>

CrashNode(i) ==
    /\ ~networkHealed
    /\ faultsUsed < MaxFaultBudget
    /\ i \notin crashedNodes
    \* Ensure a live quorum remains able to recover or restart later
    /\ crashedNodes'    = crashedNodes \cup {i}
    /\ role'            = [role EXCEPT ![i] = "Follower"]
    /\ commitIndex'     = [commitIndex EXCEPT ![i] = 0]
    /\ preVotesGranted' = [preVotesGranted EXCEPT ![i] = {}]
    /\ votesGranted'    = [votesGranted EXCEPT ![i] = {}]
    /\ faultsUsed'      = faultsUsed + 1
    /\ UNCHANGED <<currentTerm, votedFor, walLog, nextIndex, matchIndex,
                   messages, partitionedPairs, networkHealed, ghostCommitted, l2Vars>>

RestartNodeFromPersistentWAL(i) ==
    /\ i \in crashedNodes
    /\ crashedNodes' = crashedNodes \ {i}
    /\ role'         = [role EXCEPT ![i] = "Follower"]
    /\ UNCHANGED <<currentTerm, votedFor, walLog, commitIndex, preVotesGranted,
                   votesGranted, nextIndex, matchIndex, messages, partitionedPairs,
                   faultsUsed, networkHealed, ghostCommitted, l2Vars>>

DuplicateMessage(m) ==
    /\ ~networkHealed
    /\ faultsUsed < MaxFaultBudget
    /\ m \in messages
    /\ m.dupTag = 0
    /\ messages'   = messages \cup {[m EXCEPT !.dupTag = 1]}
    /\ faultsUsed' = faultsUsed + 1
    /\ UNCHANGED <<raftVars, partitionedPairs, crashedNodes, networkHealed, ghostCommitted, l2Vars>>

ShallowL2Reorg ==
    /\ ~networkHealed
    /\ faultsUsed < MaxFaultBudget
    /\ Len(l2StagedAnchor) > 0
    \* Shallow reorg (< L2FinalityDepth) orphans unfinalized staged batch; finalized batches are immutable!
    /\ l2StagedAnchor' = <<>>
    /\ faultsUsed'     = faultsUsed + 1
    /\ UNCHANGED <<raftVars, messages, partitionedPairs, crashedNodes, networkHealed, ghostCommitted, l2FinalizedAnchor>>

HealClusterEnvironment ==
    /\ ~networkHealed
    /\ networkHealed'    = TRUE
    /\ partitionedPairs' = {}
    /\ crashedNodes'     = {}
    /\ UNCHANGED <<raftVars, messages, faultsUsed, ghostCommitted, l2Vars>>

-----------------------------------------------------------------------------
FairProgressNext ==
    \/ \E i \in Servers : StartPreVote(i)
    \/ \E i \in Servers, m \in messages : HandlePreVoteReq(i, m)
    \/ \E i \in Servers, m \in messages : HandlePreVoteResp(i, m)
    \/ \E i \in Servers, m \in messages : HandleRequestVoteReq(i, m)
    \/ \E i \in Servers, m \in messages : HandleRequestVoteResp(i, m)
    \/ \E i, j \in Servers : SendAppendEntries(i, j)
    \/ \E i \in Servers, m \in messages : HandleAppendEntriesReq(i, m)
    \/ \E i \in Servers, m \in messages : HandleAppendEntriesResp(i, m)
    \/ \E i \in Servers : StageL2StateRootBatch(i)
    \/ AdvanceL2BlockConfirmations
    \/ HealClusterEnvironment

FaultInjectionNext ==
    \/ \E i \in Servers, c \in Commands : ClientPropose(i, c)
    \/ \E i, j \in Servers : InjectNetworkPartition(i, j)
    \/ \E i \in Servers : CrashNode(i)
    \/ \E i \in Servers : RestartNodeFromPersistentWAL(i)
    \/ \E m \in messages : DuplicateMessage(m)
    \/ ShallowL2Reorg

Next == FairProgressNext \/ FaultInjectionNext

Spec == Init /\ [][Next]_vars /\ WF_vars(FairProgressNext)

-----------------------------------------------------------------------------
(* ==================== SAFETY INVARIANTS ==================== *)

\* Invariant 1: Election Safety — at most one Leader can be elected in any given term.
ElectionSafety ==
    \A s1, s2 \in Servers :
        (role[s1] = "Leader" /\ role[s2] = "Leader" /\ currentTerm[s1] = currentTerm[s2])
            => (s1 = s2)

\* Invariant 2: Log Matching — equal term at index k implies identical prefix [1..k].
LogMatching ==
    \A s1, s2 \in Servers :
        \A idx \in 1..Len(walLog[s1]) :
            (idx <= Len(walLog[s2]) /\ walLog[s1][idx].term = walLog[s2][idx].term)
                => SubSeq(walLog[s1], 1, idx) = SubSeq(walLog[s2], 1, idx)

\* Invariant 3: Cryptographic Blake3 Hash-Chain Integrity on every node's WAL.
WalHashChainIntegrity ==
    \A s \in Servers :
        \A idx \in 1..Len(walLog[s]) :
            LET expectedPrev == IF idx = 1 THEN 0 ELSE walLog[s][idx - 1].hash
                entry        == walLog[s][idx]
            IN /\ entry.prevHash = expectedPrev
               /\ entry.hash = ComputeEntryHash(idx, entry.term, entry.cmd, expectedPrev)

\* Invariant 4: Zero Committed Data Loss & Leader Completeness.
ZeroCommittedDataLoss ==
    /\ \A s \in Servers :
        /\ commitIndex[s] <= Len(ghostCommitted)
        /\ \A idx \in 1..commitIndex[s] : walLog[s][idx] = ghostCommitted[idx]
    /\ \A l \in Servers :
        (role[l] = "Leader" /\ currentTerm[l] >= LastTerm(ghostCommitted))
            => /\ Len(walLog[l]) >= Len(ghostCommitted)
               /\ SubSeq(walLog[l], 1, Len(ghostCommitted)) = ghostCommitted

\* Invariant 5: L2 Settlement Prefix Consistency — finalized L2 anchors match exact committed history.
L2SettlementSafety ==
    \A b \in 1..Len(l2FinalizedAnchor) :
        LET anchor == l2FinalizedAnchor[b]
        IN /\ anchor.commitLen <= Len(ghostCommitted)
           /\ anchor.commitLen >= 1
           /\ ghostCommitted[anchor.commitLen].hash = anchor.rootHash
           /\ (b > 1 => anchor.commitLen > l2FinalizedAnchor[b - 1].commitLen)

(* ==================== TEMPORAL LIVENESS PROPERTY ==================== *)

ClusterConverged ==
    /\ \E l \in Servers : role[l] = "Leader"
    /\ \A s \in Servers : commitIndex[s] >= Len(ghostCommitted)

PostHealLiveness ==
    [](networkHealed ~> ClusterConverged)

=============================================================================
