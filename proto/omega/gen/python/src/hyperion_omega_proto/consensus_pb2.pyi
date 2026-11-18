from hyperion_proto import common_pb2 as _common_pb2
from hyperion_proto import orderbook_pb2 as _orderbook_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class RaftNodeRole(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    RAFT_NODE_ROLE_UNSPECIFIED: _ClassVar[RaftNodeRole]
    RAFT_NODE_ROLE_FOLLOWER: _ClassVar[RaftNodeRole]
    RAFT_NODE_ROLE_PRE_CANDIDATE: _ClassVar[RaftNodeRole]
    RAFT_NODE_ROLE_CANDIDATE: _ClassVar[RaftNodeRole]
    RAFT_NODE_ROLE_LEADER: _ClassVar[RaftNodeRole]
    RAFT_NODE_ROLE_LEARNER: _ClassVar[RaftNodeRole]

class RaftEntryKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    RAFT_ENTRY_KIND_UNSPECIFIED: _ClassVar[RaftEntryKind]
    RAFT_ENTRY_KIND_NOOP_BARRIER: _ClassVar[RaftEntryKind]
    RAFT_ENTRY_KIND_LEDGER_EVENT: _ClassVar[RaftEntryKind]
    RAFT_ENTRY_KIND_JOINT_CONFIG_ENTER: _ClassVar[RaftEntryKind]
    RAFT_ENTRY_KIND_JOINT_CONFIG_COMMIT: _ClassVar[RaftEntryKind]
RAFT_NODE_ROLE_UNSPECIFIED: RaftNodeRole
RAFT_NODE_ROLE_FOLLOWER: RaftNodeRole
RAFT_NODE_ROLE_PRE_CANDIDATE: RaftNodeRole
RAFT_NODE_ROLE_CANDIDATE: RaftNodeRole
RAFT_NODE_ROLE_LEADER: RaftNodeRole
RAFT_NODE_ROLE_LEARNER: RaftNodeRole
RAFT_ENTRY_KIND_UNSPECIFIED: RaftEntryKind
RAFT_ENTRY_KIND_NOOP_BARRIER: RaftEntryKind
RAFT_ENTRY_KIND_LEDGER_EVENT: RaftEntryKind
RAFT_ENTRY_KIND_JOINT_CONFIG_ENTER: RaftEntryKind
RAFT_ENTRY_KIND_JOINT_CONFIG_COMMIT: RaftEntryKind

class RaftNodePeer(_message.Message):
    __slots__ = ("node_id", "peer_addr", "is_voting_voter")
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    PEER_ADDR_FIELD_NUMBER: _ClassVar[int]
    IS_VOTING_VOTER_FIELD_NUMBER: _ClassVar[int]
    node_id: int
    peer_addr: str
    is_voting_voter: bool
    def __init__(self, node_id: _Optional[int] = ..., peer_addr: _Optional[str] = ..., is_voting_voter: _Optional[bool] = ...) -> None: ...

class JointConsensusConfig(_message.Message):
    __slots__ = ("config_index", "current_voters", "previous_voters", "learners")
    CONFIG_INDEX_FIELD_NUMBER: _ClassVar[int]
    CURRENT_VOTERS_FIELD_NUMBER: _ClassVar[int]
    PREVIOUS_VOTERS_FIELD_NUMBER: _ClassVar[int]
    LEARNERS_FIELD_NUMBER: _ClassVar[int]
    config_index: int
    current_voters: _containers.RepeatedCompositeFieldContainer[RaftNodePeer]
    previous_voters: _containers.RepeatedCompositeFieldContainer[RaftNodePeer]
    learners: _containers.RepeatedCompositeFieldContainer[RaftNodePeer]
    def __init__(self, config_index: _Optional[int] = ..., current_voters: _Optional[_Iterable[_Union[RaftNodePeer, _Mapping]]] = ..., previous_voters: _Optional[_Iterable[_Union[RaftNodePeer, _Mapping]]] = ..., learners: _Optional[_Iterable[_Union[RaftNodePeer, _Mapping]]] = ...) -> None: ...

class RaftLogEntry(_message.Message):
    __slots__ = ("term", "index", "engine_seq", "ts_ns", "kind", "crc32c_checksum", "prev_blake3_hash", "entry_blake3_hash", "ledger_envelope", "membership_change", "raw_command_bytes")
    TERM_FIELD_NUMBER: _ClassVar[int]
    INDEX_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    CRC32C_CHECKSUM_FIELD_NUMBER: _ClassVar[int]
    PREV_BLAKE3_HASH_FIELD_NUMBER: _ClassVar[int]
    ENTRY_BLAKE3_HASH_FIELD_NUMBER: _ClassVar[int]
    LEDGER_ENVELOPE_FIELD_NUMBER: _ClassVar[int]
    MEMBERSHIP_CHANGE_FIELD_NUMBER: _ClassVar[int]
    RAW_COMMAND_BYTES_FIELD_NUMBER: _ClassVar[int]
    term: int
    index: int
    engine_seq: int
    ts_ns: int
    kind: RaftEntryKind
    crc32c_checksum: int
    prev_blake3_hash: bytes
    entry_blake3_hash: bytes
    ledger_envelope: _orderbook_pb2.MarketEventEnvelope
    membership_change: JointConsensusConfig
    raw_command_bytes: bytes
    def __init__(self, term: _Optional[int] = ..., index: _Optional[int] = ..., engine_seq: _Optional[int] = ..., ts_ns: _Optional[int] = ..., kind: _Optional[_Union[RaftEntryKind, str]] = ..., crc32c_checksum: _Optional[int] = ..., prev_blake3_hash: _Optional[bytes] = ..., entry_blake3_hash: _Optional[bytes] = ..., ledger_envelope: _Optional[_Union[_orderbook_pb2.MarketEventEnvelope, _Mapping]] = ..., membership_change: _Optional[_Union[JointConsensusConfig, _Mapping]] = ..., raw_command_bytes: _Optional[bytes] = ...) -> None: ...

class AppendEntries(_message.Message):
    __slots__ = ("cluster_id", "term", "leader_id", "prev_log_index", "prev_log_term", "prev_log_blake3", "entries", "leader_commit_index", "pipeline_seq", "leader_lease_until_ns", "trace")
    CLUSTER_ID_FIELD_NUMBER: _ClassVar[int]
    TERM_FIELD_NUMBER: _ClassVar[int]
    LEADER_ID_FIELD_NUMBER: _ClassVar[int]
    PREV_LOG_INDEX_FIELD_NUMBER: _ClassVar[int]
    PREV_LOG_TERM_FIELD_NUMBER: _ClassVar[int]
    PREV_LOG_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    ENTRIES_FIELD_NUMBER: _ClassVar[int]
    LEADER_COMMIT_INDEX_FIELD_NUMBER: _ClassVar[int]
    PIPELINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    LEADER_LEASE_UNTIL_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    cluster_id: int
    term: int
    leader_id: int
    prev_log_index: int
    prev_log_term: int
    prev_log_blake3: bytes
    entries: _containers.RepeatedCompositeFieldContainer[RaftLogEntry]
    leader_commit_index: int
    pipeline_seq: int
    leader_lease_until_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, cluster_id: _Optional[int] = ..., term: _Optional[int] = ..., leader_id: _Optional[int] = ..., prev_log_index: _Optional[int] = ..., prev_log_term: _Optional[int] = ..., prev_log_blake3: _Optional[bytes] = ..., entries: _Optional[_Iterable[_Union[RaftLogEntry, _Mapping]]] = ..., leader_commit_index: _Optional[int] = ..., pipeline_seq: _Optional[int] = ..., leader_lease_until_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class AppendEntriesResponse(_message.Message):
    __slots__ = ("term", "follower_id", "success", "match_index", "conflict_index", "conflict_term", "pipeline_seq", "bit_rot_detected")
    TERM_FIELD_NUMBER: _ClassVar[int]
    FOLLOWER_ID_FIELD_NUMBER: _ClassVar[int]
    SUCCESS_FIELD_NUMBER: _ClassVar[int]
    MATCH_INDEX_FIELD_NUMBER: _ClassVar[int]
    CONFLICT_INDEX_FIELD_NUMBER: _ClassVar[int]
    CONFLICT_TERM_FIELD_NUMBER: _ClassVar[int]
    PIPELINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    BIT_ROT_DETECTED_FIELD_NUMBER: _ClassVar[int]
    term: int
    follower_id: int
    success: bool
    match_index: int
    conflict_index: int
    conflict_term: int
    pipeline_seq: int
    bit_rot_detected: bool
    def __init__(self, term: _Optional[int] = ..., follower_id: _Optional[int] = ..., success: _Optional[bool] = ..., match_index: _Optional[int] = ..., conflict_index: _Optional[int] = ..., conflict_term: _Optional[int] = ..., pipeline_seq: _Optional[int] = ..., bit_rot_detected: _Optional[bool] = ...) -> None: ...

class RequestVote(_message.Message):
    __slots__ = ("cluster_id", "term", "candidate_id", "last_log_index", "last_log_term", "last_log_blake3", "trace")
    CLUSTER_ID_FIELD_NUMBER: _ClassVar[int]
    TERM_FIELD_NUMBER: _ClassVar[int]
    CANDIDATE_ID_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_INDEX_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_TERM_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    cluster_id: int
    term: int
    candidate_id: int
    last_log_index: int
    last_log_term: int
    last_log_blake3: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, cluster_id: _Optional[int] = ..., term: _Optional[int] = ..., candidate_id: _Optional[int] = ..., last_log_index: _Optional[int] = ..., last_log_term: _Optional[int] = ..., last_log_blake3: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class RequestVoteResponse(_message.Message):
    __slots__ = ("term", "voter_id", "vote_granted", "reject_reason")
    TERM_FIELD_NUMBER: _ClassVar[int]
    VOTER_ID_FIELD_NUMBER: _ClassVar[int]
    VOTE_GRANTED_FIELD_NUMBER: _ClassVar[int]
    REJECT_REASON_FIELD_NUMBER: _ClassVar[int]
    term: int
    voter_id: int
    vote_granted: bool
    reject_reason: str
    def __init__(self, term: _Optional[int] = ..., voter_id: _Optional[int] = ..., vote_granted: _Optional[bool] = ..., reject_reason: _Optional[str] = ...) -> None: ...

class PreVote(_message.Message):
    __slots__ = ("cluster_id", "prospective_term", "candidate_id", "last_log_index", "last_log_term", "last_log_blake3", "trace")
    CLUSTER_ID_FIELD_NUMBER: _ClassVar[int]
    PROSPECTIVE_TERM_FIELD_NUMBER: _ClassVar[int]
    CANDIDATE_ID_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_INDEX_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_TERM_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    cluster_id: int
    prospective_term: int
    candidate_id: int
    last_log_index: int
    last_log_term: int
    last_log_blake3: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, cluster_id: _Optional[int] = ..., prospective_term: _Optional[int] = ..., candidate_id: _Optional[int] = ..., last_log_index: _Optional[int] = ..., last_log_term: _Optional[int] = ..., last_log_blake3: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class PreVoteResponse(_message.Message):
    __slots__ = ("current_term", "voter_id", "vote_granted", "leader_lease_active")
    CURRENT_TERM_FIELD_NUMBER: _ClassVar[int]
    VOTER_ID_FIELD_NUMBER: _ClassVar[int]
    VOTE_GRANTED_FIELD_NUMBER: _ClassVar[int]
    LEADER_LEASE_ACTIVE_FIELD_NUMBER: _ClassVar[int]
    current_term: int
    voter_id: int
    vote_granted: bool
    leader_lease_active: bool
    def __init__(self, current_term: _Optional[int] = ..., voter_id: _Optional[int] = ..., vote_granted: _Optional[bool] = ..., leader_lease_active: _Optional[bool] = ...) -> None: ...

class InstallSnapshot(_message.Message):
    __slots__ = ("cluster_id", "term", "leader_id", "last_included_index", "last_included_term", "last_included_engine_seq", "state_root_blake3", "cluster_config", "byte_offset", "chunk_data", "chunk_crc32c", "done", "trace")
    CLUSTER_ID_FIELD_NUMBER: _ClassVar[int]
    TERM_FIELD_NUMBER: _ClassVar[int]
    LEADER_ID_FIELD_NUMBER: _ClassVar[int]
    LAST_INCLUDED_INDEX_FIELD_NUMBER: _ClassVar[int]
    LAST_INCLUDED_TERM_FIELD_NUMBER: _ClassVar[int]
    LAST_INCLUDED_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    CLUSTER_CONFIG_FIELD_NUMBER: _ClassVar[int]
    BYTE_OFFSET_FIELD_NUMBER: _ClassVar[int]
    CHUNK_DATA_FIELD_NUMBER: _ClassVar[int]
    CHUNK_CRC32C_FIELD_NUMBER: _ClassVar[int]
    DONE_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    cluster_id: int
    term: int
    leader_id: int
    last_included_index: int
    last_included_term: int
    last_included_engine_seq: int
    state_root_blake3: bytes
    cluster_config: JointConsensusConfig
    byte_offset: int
    chunk_data: bytes
    chunk_crc32c: int
    done: bool
    trace: _common_pb2.TraceMetadata
    def __init__(self, cluster_id: _Optional[int] = ..., term: _Optional[int] = ..., leader_id: _Optional[int] = ..., last_included_index: _Optional[int] = ..., last_included_term: _Optional[int] = ..., last_included_engine_seq: _Optional[int] = ..., state_root_blake3: _Optional[bytes] = ..., cluster_config: _Optional[_Union[JointConsensusConfig, _Mapping]] = ..., byte_offset: _Optional[int] = ..., chunk_data: _Optional[bytes] = ..., chunk_crc32c: _Optional[int] = ..., done: _Optional[bool] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class InstallSnapshotResponse(_message.Message):
    __slots__ = ("term", "follower_id", "bytes_stored", "checksum_valid")
    TERM_FIELD_NUMBER: _ClassVar[int]
    FOLLOWER_ID_FIELD_NUMBER: _ClassVar[int]
    BYTES_STORED_FIELD_NUMBER: _ClassVar[int]
    CHECKSUM_VALID_FIELD_NUMBER: _ClassVar[int]
    term: int
    follower_id: int
    bytes_stored: int
    checksum_valid: bool
    def __init__(self, term: _Optional[int] = ..., follower_id: _Optional[int] = ..., bytes_stored: _Optional[int] = ..., checksum_valid: _Optional[bool] = ...) -> None: ...

class ReadIndexBarrier(_message.Message):
    __slots__ = ("read_request_id", "term", "leader_id", "required_read_index", "quorum_ack_mask", "served_via_leader_lease", "barrier_latency_ns", "committed_state_root_blake3", "trace")
    READ_REQUEST_ID_FIELD_NUMBER: _ClassVar[int]
    TERM_FIELD_NUMBER: _ClassVar[int]
    LEADER_ID_FIELD_NUMBER: _ClassVar[int]
    REQUIRED_READ_INDEX_FIELD_NUMBER: _ClassVar[int]
    QUORUM_ACK_MASK_FIELD_NUMBER: _ClassVar[int]
    SERVED_VIA_LEADER_LEASE_FIELD_NUMBER: _ClassVar[int]
    BARRIER_LATENCY_NS_FIELD_NUMBER: _ClassVar[int]
    COMMITTED_STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    read_request_id: str
    term: int
    leader_id: int
    required_read_index: int
    quorum_ack_mask: int
    served_via_leader_lease: bool
    barrier_latency_ns: int
    committed_state_root_blake3: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, read_request_id: _Optional[str] = ..., term: _Optional[int] = ..., leader_id: _Optional[int] = ..., required_read_index: _Optional[int] = ..., quorum_ack_mask: _Optional[int] = ..., served_via_leader_lease: _Optional[bool] = ..., barrier_latency_ns: _Optional[int] = ..., committed_state_root_blake3: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ReadIndexBarrierRequest(_message.Message):
    __slots__ = ("read_request_id", "allow_leader_lease_fast_path", "trace")
    READ_REQUEST_ID_FIELD_NUMBER: _ClassVar[int]
    ALLOW_LEADER_LEASE_FAST_PATH_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    read_request_id: str
    allow_leader_lease_fast_path: bool
    trace: _common_pb2.TraceMetadata
    def __init__(self, read_request_id: _Optional[str] = ..., allow_leader_lease_fast_path: _Optional[bool] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ProposeLedgerEntryRequest(_message.Message):
    __slots__ = ("idempotency_key", "envelope", "trace")
    IDEMPOTENCY_KEY_FIELD_NUMBER: _ClassVar[int]
    ENVELOPE_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    idempotency_key: str
    envelope: _orderbook_pb2.MarketEventEnvelope
    trace: _common_pb2.TraceMetadata
    def __init__(self, idempotency_key: _Optional[str] = ..., envelope: _Optional[_Union[_orderbook_pb2.MarketEventEnvelope, _Mapping]] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ProposeLedgerEntryResponse(_message.Message):
    __slots__ = ("committed", "term", "log_index", "leader_id", "entry_blake3_hash", "error_detail")
    COMMITTED_FIELD_NUMBER: _ClassVar[int]
    TERM_FIELD_NUMBER: _ClassVar[int]
    LOG_INDEX_FIELD_NUMBER: _ClassVar[int]
    LEADER_ID_FIELD_NUMBER: _ClassVar[int]
    ENTRY_BLAKE3_HASH_FIELD_NUMBER: _ClassVar[int]
    ERROR_DETAIL_FIELD_NUMBER: _ClassVar[int]
    committed: bool
    term: int
    log_index: int
    leader_id: int
    entry_blake3_hash: bytes
    error_detail: str
    def __init__(self, committed: _Optional[bool] = ..., term: _Optional[int] = ..., log_index: _Optional[int] = ..., leader_id: _Optional[int] = ..., entry_blake3_hash: _Optional[bytes] = ..., error_detail: _Optional[str] = ...) -> None: ...

class GetRaftClusterStatusRequest(_message.Message):
    __slots__ = ("node_id",)
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    node_id: int
    def __init__(self, node_id: _Optional[int] = ...) -> None: ...

class RaftClusterStatus(_message.Message):
    __slots__ = ("cluster_id", "node_id", "role", "current_term", "voted_for", "leader_id", "commit_index", "last_applied_index", "last_log_index", "last_log_term", "last_committed_blake3", "active_config")
    CLUSTER_ID_FIELD_NUMBER: _ClassVar[int]
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    CURRENT_TERM_FIELD_NUMBER: _ClassVar[int]
    VOTED_FOR_FIELD_NUMBER: _ClassVar[int]
    LEADER_ID_FIELD_NUMBER: _ClassVar[int]
    COMMIT_INDEX_FIELD_NUMBER: _ClassVar[int]
    LAST_APPLIED_INDEX_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_INDEX_FIELD_NUMBER: _ClassVar[int]
    LAST_LOG_TERM_FIELD_NUMBER: _ClassVar[int]
    LAST_COMMITTED_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_CONFIG_FIELD_NUMBER: _ClassVar[int]
    cluster_id: int
    node_id: int
    role: RaftNodeRole
    current_term: int
    voted_for: int
    leader_id: int
    commit_index: int
    last_applied_index: int
    last_log_index: int
    last_log_term: int
    last_committed_blake3: bytes
    active_config: JointConsensusConfig
    def __init__(self, cluster_id: _Optional[int] = ..., node_id: _Optional[int] = ..., role: _Optional[_Union[RaftNodeRole, str]] = ..., current_term: _Optional[int] = ..., voted_for: _Optional[int] = ..., leader_id: _Optional[int] = ..., commit_index: _Optional[int] = ..., last_applied_index: _Optional[int] = ..., last_log_index: _Optional[int] = ..., last_log_term: _Optional[int] = ..., last_committed_blake3: _Optional[bytes] = ..., active_config: _Optional[_Union[JointConsensusConfig, _Mapping]] = ...) -> None: ...
