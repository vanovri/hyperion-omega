from hyperion_proto import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ZkProofSystem(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ZK_PROOF_SYSTEM_UNSPECIFIED: _ClassVar[ZkProofSystem]
    ZK_PROOF_SYSTEM_GROTH16_BN254: _ClassVar[ZkProofSystem]
    ZK_PROOF_SYSTEM_PLONK_BN254: _ClassVar[ZkProofSystem]
    ZK_PROOF_SYSTEM_HALO2_PASTA: _ClassVar[ZkProofSystem]
    ZK_PROOF_SYSTEM_BULLETPROOFS_RISTRETTO: _ClassVar[ZkProofSystem]

class MsstHashScheme(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    MSST_HASH_SCHEME_UNSPECIFIED: _ClassVar[MsstHashScheme]
    MSST_HASH_SCHEME_POSEIDON_BN254: _ClassVar[MsstHashScheme]
    MSST_HASH_SCHEME_POSEIDON_BLS12_381: _ClassVar[MsstHashScheme]
    MSST_HASH_SCHEME_HYBRID_POSEIDON_BLAKE3: _ClassVar[MsstHashScheme]
ZK_PROOF_SYSTEM_UNSPECIFIED: ZkProofSystem
ZK_PROOF_SYSTEM_GROTH16_BN254: ZkProofSystem
ZK_PROOF_SYSTEM_PLONK_BN254: ZkProofSystem
ZK_PROOF_SYSTEM_HALO2_PASTA: ZkProofSystem
ZK_PROOF_SYSTEM_BULLETPROOFS_RISTRETTO: ZkProofSystem
MSST_HASH_SCHEME_UNSPECIFIED: MsstHashScheme
MSST_HASH_SCHEME_POSEIDON_BN254: MsstHashScheme
MSST_HASH_SCHEME_POSEIDON_BLS12_381: MsstHashScheme
MSST_HASH_SCHEME_HYBRID_POSEIDON_BLAKE3: MsstHashScheme

class AssetSolvencySummary(_message.Message):
    __slots__ = ("asset", "total_liabilities_fp", "verified_reserves_fp", "surplus_equity_fp", "reserve_ratio_ppm")
    ASSET_FIELD_NUMBER: _ClassVar[int]
    TOTAL_LIABILITIES_FP_FIELD_NUMBER: _ClassVar[int]
    VERIFIED_RESERVES_FP_FIELD_NUMBER: _ClassVar[int]
    SURPLUS_EQUITY_FP_FIELD_NUMBER: _ClassVar[int]
    RESERVE_RATIO_PPM_FIELD_NUMBER: _ClassVar[int]
    asset: str
    total_liabilities_fp: _common_pb2.U128
    verified_reserves_fp: _common_pb2.U128
    surplus_equity_fp: _common_pb2.U128
    reserve_ratio_ppm: int
    def __init__(self, asset: _Optional[str] = ..., total_liabilities_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., verified_reserves_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., surplus_equity_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., reserve_ratio_ppm: _Optional[int] = ...) -> None: ...

class MerkleSumRootAnnouncement(_message.Message):
    __slots__ = ("epoch_id", "snapshot_engine_seq", "snapshot_raft_index", "asset", "tree_height", "active_account_count", "hash_scheme", "root_hash", "root_liability_sum_fp", "onchain_reserve_sum_fp", "l2_reference_block_number", "l2_reference_block_hash", "announced_ts_ns", "trace")
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_RAFT_INDEX_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    TREE_HEIGHT_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_ACCOUNT_COUNT_FIELD_NUMBER: _ClassVar[int]
    HASH_SCHEME_FIELD_NUMBER: _ClassVar[int]
    ROOT_HASH_FIELD_NUMBER: _ClassVar[int]
    ROOT_LIABILITY_SUM_FP_FIELD_NUMBER: _ClassVar[int]
    ONCHAIN_RESERVE_SUM_FP_FIELD_NUMBER: _ClassVar[int]
    L2_REFERENCE_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    L2_REFERENCE_BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    ANNOUNCED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    epoch_id: int
    snapshot_engine_seq: int
    snapshot_raft_index: int
    asset: str
    tree_height: int
    active_account_count: int
    hash_scheme: MsstHashScheme
    root_hash: bytes
    root_liability_sum_fp: _common_pb2.U128
    onchain_reserve_sum_fp: _common_pb2.U128
    l2_reference_block_number: int
    l2_reference_block_hash: bytes
    announced_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, epoch_id: _Optional[int] = ..., snapshot_engine_seq: _Optional[int] = ..., snapshot_raft_index: _Optional[int] = ..., asset: _Optional[str] = ..., tree_height: _Optional[int] = ..., active_account_count: _Optional[int] = ..., hash_scheme: _Optional[_Union[MsstHashScheme, str]] = ..., root_hash: _Optional[bytes] = ..., root_liability_sum_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., onchain_reserve_sum_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., l2_reference_block_number: _Optional[int] = ..., l2_reference_block_hash: _Optional[bytes] = ..., announced_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class MerkleSumSiblingNode(_message.Message):
    __slots__ = ("level", "sibling_is_left", "sibling_hash", "sibling_sum_fp")
    LEVEL_FIELD_NUMBER: _ClassVar[int]
    SIBLING_IS_LEFT_FIELD_NUMBER: _ClassVar[int]
    SIBLING_HASH_FIELD_NUMBER: _ClassVar[int]
    SIBLING_SUM_FP_FIELD_NUMBER: _ClassVar[int]
    level: int
    sibling_is_left: bool
    sibling_hash: bytes
    sibling_sum_fp: _common_pb2.U128
    def __init__(self, level: _Optional[int] = ..., sibling_is_left: _Optional[bool] = ..., sibling_hash: _Optional[bytes] = ..., sibling_sum_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ...) -> None: ...

class InclusionSumProof(_message.Message):
    __slots__ = ("epoch_id", "snapshot_engine_seq", "asset", "blinded_account_leaf_id", "user_balance_fp", "leaf_salt_commitment", "leaf_hash", "siblings", "expected_root_hash", "expected_root_sum_fp", "hash_scheme", "generated_ts_ns")
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    BLINDED_ACCOUNT_LEAF_ID_FIELD_NUMBER: _ClassVar[int]
    USER_BALANCE_FP_FIELD_NUMBER: _ClassVar[int]
    LEAF_SALT_COMMITMENT_FIELD_NUMBER: _ClassVar[int]
    LEAF_HASH_FIELD_NUMBER: _ClassVar[int]
    SIBLINGS_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_ROOT_HASH_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_ROOT_SUM_FP_FIELD_NUMBER: _ClassVar[int]
    HASH_SCHEME_FIELD_NUMBER: _ClassVar[int]
    GENERATED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    epoch_id: int
    snapshot_engine_seq: int
    asset: str
    blinded_account_leaf_id: bytes
    user_balance_fp: int
    leaf_salt_commitment: bytes
    leaf_hash: bytes
    siblings: _containers.RepeatedCompositeFieldContainer[MerkleSumSiblingNode]
    expected_root_hash: bytes
    expected_root_sum_fp: _common_pb2.U128
    hash_scheme: MsstHashScheme
    generated_ts_ns: int
    def __init__(self, epoch_id: _Optional[int] = ..., snapshot_engine_seq: _Optional[int] = ..., asset: _Optional[str] = ..., blinded_account_leaf_id: _Optional[bytes] = ..., user_balance_fp: _Optional[int] = ..., leaf_salt_commitment: _Optional[bytes] = ..., leaf_hash: _Optional[bytes] = ..., siblings: _Optional[_Iterable[_Union[MerkleSumSiblingNode, _Mapping]]] = ..., expected_root_hash: _Optional[bytes] = ..., expected_root_sum_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., hash_scheme: _Optional[_Union[MsstHashScheme, str]] = ..., generated_ts_ns: _Optional[int] = ...) -> None: ...

class ZKSolvencyReceipt(_message.Message):
    __slots__ = ("receipt_id", "epoch_id", "snapshot_engine_seq", "proof_system", "hash_scheme", "root_announcement", "asset_summaries", "zk_proof_bytes", "public_inputs_le", "verifying_key_digest", "proving_latency_ms", "proof_size_bytes", "self_verified_ok", "created_ts_ns", "trace")
    RECEIPT_ID_FIELD_NUMBER: _ClassVar[int]
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    PROOF_SYSTEM_FIELD_NUMBER: _ClassVar[int]
    HASH_SCHEME_FIELD_NUMBER: _ClassVar[int]
    ROOT_ANNOUNCEMENT_FIELD_NUMBER: _ClassVar[int]
    ASSET_SUMMARIES_FIELD_NUMBER: _ClassVar[int]
    ZK_PROOF_BYTES_FIELD_NUMBER: _ClassVar[int]
    PUBLIC_INPUTS_LE_FIELD_NUMBER: _ClassVar[int]
    VERIFYING_KEY_DIGEST_FIELD_NUMBER: _ClassVar[int]
    PROVING_LATENCY_MS_FIELD_NUMBER: _ClassVar[int]
    PROOF_SIZE_BYTES_FIELD_NUMBER: _ClassVar[int]
    SELF_VERIFIED_OK_FIELD_NUMBER: _ClassVar[int]
    CREATED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    receipt_id: str
    epoch_id: int
    snapshot_engine_seq: int
    proof_system: ZkProofSystem
    hash_scheme: MsstHashScheme
    root_announcement: MerkleSumRootAnnouncement
    asset_summaries: _containers.RepeatedCompositeFieldContainer[AssetSolvencySummary]
    zk_proof_bytes: bytes
    public_inputs_le: _containers.RepeatedScalarFieldContainer[bytes]
    verifying_key_digest: bytes
    proving_latency_ms: int
    proof_size_bytes: int
    self_verified_ok: bool
    created_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, receipt_id: _Optional[str] = ..., epoch_id: _Optional[int] = ..., snapshot_engine_seq: _Optional[int] = ..., proof_system: _Optional[_Union[ZkProofSystem, str]] = ..., hash_scheme: _Optional[_Union[MsstHashScheme, str]] = ..., root_announcement: _Optional[_Union[MerkleSumRootAnnouncement, _Mapping]] = ..., asset_summaries: _Optional[_Iterable[_Union[AssetSolvencySummary, _Mapping]]] = ..., zk_proof_bytes: _Optional[bytes] = ..., public_inputs_le: _Optional[_Iterable[bytes]] = ..., verifying_key_digest: _Optional[bytes] = ..., proving_latency_ms: _Optional[int] = ..., proof_size_bytes: _Optional[int] = ..., self_verified_ok: _Optional[bool] = ..., created_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class GenerateEpochSolvencyRequest(_message.Message):
    __slots__ = ("epoch_id", "snapshot_engine_seq", "asset", "verified_onchain_reserves_fp", "l2_block_number", "l2_block_hash", "trace")
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    VERIFIED_ONCHAIN_RESERVES_FP_FIELD_NUMBER: _ClassVar[int]
    L2_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    L2_BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    epoch_id: int
    snapshot_engine_seq: int
    asset: str
    verified_onchain_reserves_fp: _common_pb2.U128
    l2_block_number: int
    l2_block_hash: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, epoch_id: _Optional[int] = ..., snapshot_engine_seq: _Optional[int] = ..., asset: _Optional[str] = ..., verified_onchain_reserves_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., l2_block_number: _Optional[int] = ..., l2_block_hash: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class GetInclusionSumProofRequest(_message.Message):
    __slots__ = ("epoch_id", "account_id", "asset")
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    epoch_id: int
    account_id: int
    asset: str
    def __init__(self, epoch_id: _Optional[int] = ..., account_id: _Optional[int] = ..., asset: _Optional[str] = ...) -> None: ...

class VerifyZKSolvencyReceiptRequest(_message.Message):
    __slots__ = ("receipt",)
    RECEIPT_FIELD_NUMBER: _ClassVar[int]
    receipt: ZKSolvencyReceipt
    def __init__(self, receipt: _Optional[_Union[ZKSolvencyReceipt, _Mapping]] = ...) -> None: ...

class VerifyZKSolvencyReceiptResponse(_message.Message):
    __slots__ = ("valid", "verification_latency_us", "failure_reason")
    VALID_FIELD_NUMBER: _ClassVar[int]
    VERIFICATION_LATENCY_US_FIELD_NUMBER: _ClassVar[int]
    FAILURE_REASON_FIELD_NUMBER: _ClassVar[int]
    valid: bool
    verification_latency_us: int
    failure_reason: str
    def __init__(self, valid: _Optional[bool] = ..., verification_latency_us: _Optional[int] = ..., failure_reason: _Optional[str] = ...) -> None: ...
