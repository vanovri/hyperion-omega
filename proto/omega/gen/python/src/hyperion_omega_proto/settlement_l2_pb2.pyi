from hyperion_proto import common_pb2 as _common_pb2
from . import zk_solvency_pb2 as _zk_solvency_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class DepositFinalityStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    DEPOSIT_FINALITY_STATUS_UNSPECIFIED: _ClassVar[DepositFinalityStatus]
    DEPOSIT_FINALITY_STATUS_MEMPOOL_SEEN: _ClassVar[DepositFinalityStatus]
    DEPOSIT_FINALITY_STATUS_SOFT_CONFIRMED: _ClassVar[DepositFinalityStatus]
    DEPOSIT_FINALITY_STATUS_FINALIZED_CREDITED: _ClassVar[DepositFinalityStatus]
    DEPOSIT_FINALITY_STATUS_REORG_ORPHANED_REVOKED: _ClassVar[DepositFinalityStatus]

class WithdrawalSettlementStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WITHDRAWAL_SETTLEMENT_STATUS_UNSPECIFIED: _ClassVar[WithdrawalSettlementStatus]
    WITHDRAWAL_SETTLEMENT_STATUS_QUEUED_OUTBOX: _ClassVar[WithdrawalSettlementStatus]
    WITHDRAWAL_SETTLEMENT_STATUS_SUBMITTED_ONCHAIN: _ClassVar[WithdrawalSettlementStatus]
    WITHDRAWAL_SETTLEMENT_STATUS_FINALIZED_SETTLED: _ClassVar[WithdrawalSettlementStatus]
    WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED: _ClassVar[WithdrawalSettlementStatus]
    WITHDRAWAL_SETTLEMENT_STATUS_FORCED_ESCAPE_HATCH_SETTLED: _ClassVar[WithdrawalSettlementStatus]
DEPOSIT_FINALITY_STATUS_UNSPECIFIED: DepositFinalityStatus
DEPOSIT_FINALITY_STATUS_MEMPOOL_SEEN: DepositFinalityStatus
DEPOSIT_FINALITY_STATUS_SOFT_CONFIRMED: DepositFinalityStatus
DEPOSIT_FINALITY_STATUS_FINALIZED_CREDITED: DepositFinalityStatus
DEPOSIT_FINALITY_STATUS_REORG_ORPHANED_REVOKED: DepositFinalityStatus
WITHDRAWAL_SETTLEMENT_STATUS_UNSPECIFIED: WithdrawalSettlementStatus
WITHDRAWAL_SETTLEMENT_STATUS_QUEUED_OUTBOX: WithdrawalSettlementStatus
WITHDRAWAL_SETTLEMENT_STATUS_SUBMITTED_ONCHAIN: WithdrawalSettlementStatus
WITHDRAWAL_SETTLEMENT_STATUS_FINALIZED_SETTLED: WithdrawalSettlementStatus
WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED: WithdrawalSettlementStatus
WITHDRAWAL_SETTLEMENT_STATUS_FORCED_ESCAPE_HATCH_SETTLED: WithdrawalSettlementStatus

class DepositFinalized(_message.Message):
    __slots__ = ("deposit_id", "chain_id", "vault_contract_address", "onchain_deposit_nonce", "account_id", "depositor_evm_address", "token_contract_address", "asset_symbol", "amount_fp", "l2_tx_hash", "log_index", "block_number", "block_hash", "confirmations", "finality_status", "finalized_ts_ns", "trace")
    DEPOSIT_ID_FIELD_NUMBER: _ClassVar[int]
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    VAULT_CONTRACT_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    ONCHAIN_DEPOSIT_NONCE_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    DEPOSITOR_EVM_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    TOKEN_CONTRACT_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    ASSET_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    AMOUNT_FP_FIELD_NUMBER: _ClassVar[int]
    L2_TX_HASH_FIELD_NUMBER: _ClassVar[int]
    LOG_INDEX_FIELD_NUMBER: _ClassVar[int]
    BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    CONFIRMATIONS_FIELD_NUMBER: _ClassVar[int]
    FINALITY_STATUS_FIELD_NUMBER: _ClassVar[int]
    FINALIZED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    deposit_id: str
    chain_id: int
    vault_contract_address: bytes
    onchain_deposit_nonce: int
    account_id: int
    depositor_evm_address: bytes
    token_contract_address: bytes
    asset_symbol: str
    amount_fp: _common_pb2.U128
    l2_tx_hash: bytes
    log_index: int
    block_number: int
    block_hash: bytes
    confirmations: int
    finality_status: DepositFinalityStatus
    finalized_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, deposit_id: _Optional[str] = ..., chain_id: _Optional[int] = ..., vault_contract_address: _Optional[bytes] = ..., onchain_deposit_nonce: _Optional[int] = ..., account_id: _Optional[int] = ..., depositor_evm_address: _Optional[bytes] = ..., token_contract_address: _Optional[bytes] = ..., asset_symbol: _Optional[str] = ..., amount_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., l2_tx_hash: _Optional[bytes] = ..., log_index: _Optional[int] = ..., block_number: _Optional[int] = ..., block_hash: _Optional[bytes] = ..., confirmations: _Optional[int] = ..., finality_status: _Optional[_Union[DepositFinalityStatus, str]] = ..., finalized_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class WithdrawalEIP712Intent(_message.Message):
    __slots__ = ("withdrawal_id", "chain_id", "verifying_vault_address", "account_id", "recipient_evm_address", "token_contract_address", "asset_symbol", "amount_fp", "bridge_fee_fp", "withdrawal_nonce", "epoch_id", "deadline_unix_sec", "eip712_digest", "nullifier_hash", "secp256k1_signature", "ledger_debit_engine_seq", "status", "created_ts_ns", "trace")
    WITHDRAWAL_ID_FIELD_NUMBER: _ClassVar[int]
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    VERIFYING_VAULT_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    RECIPIENT_EVM_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    TOKEN_CONTRACT_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    ASSET_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    AMOUNT_FP_FIELD_NUMBER: _ClassVar[int]
    BRIDGE_FEE_FP_FIELD_NUMBER: _ClassVar[int]
    WITHDRAWAL_NONCE_FIELD_NUMBER: _ClassVar[int]
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    DEADLINE_UNIX_SEC_FIELD_NUMBER: _ClassVar[int]
    EIP712_DIGEST_FIELD_NUMBER: _ClassVar[int]
    NULLIFIER_HASH_FIELD_NUMBER: _ClassVar[int]
    SECP256K1_SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    LEDGER_DEBIT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    CREATED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    withdrawal_id: str
    chain_id: int
    verifying_vault_address: bytes
    account_id: int
    recipient_evm_address: bytes
    token_contract_address: bytes
    asset_symbol: str
    amount_fp: _common_pb2.U128
    bridge_fee_fp: _common_pb2.U128
    withdrawal_nonce: int
    epoch_id: int
    deadline_unix_sec: int
    eip712_digest: bytes
    nullifier_hash: bytes
    secp256k1_signature: bytes
    ledger_debit_engine_seq: int
    status: WithdrawalSettlementStatus
    created_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, withdrawal_id: _Optional[str] = ..., chain_id: _Optional[int] = ..., verifying_vault_address: _Optional[bytes] = ..., account_id: _Optional[int] = ..., recipient_evm_address: _Optional[bytes] = ..., token_contract_address: _Optional[bytes] = ..., asset_symbol: _Optional[str] = ..., amount_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., bridge_fee_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., withdrawal_nonce: _Optional[int] = ..., epoch_id: _Optional[int] = ..., deadline_unix_sec: _Optional[int] = ..., eip712_digest: _Optional[bytes] = ..., nullifier_hash: _Optional[bytes] = ..., secp256k1_signature: _Optional[bytes] = ..., ledger_debit_engine_seq: _Optional[int] = ..., status: _Optional[_Union[WithdrawalSettlementStatus, str]] = ..., created_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class StateRootBatchCommit(_message.Message):
    __slots__ = ("batch_id", "epoch_id", "chain_id", "state_anchor_contract", "start_engine_seq", "end_engine_seq", "raft_commit_index", "prev_merkle_sum_root", "new_merkle_sum_root", "declared_liabilities_fp", "verified_vault_reserves_fp", "zk_solvency_proof_bytes", "l2_commit_tx_hash", "l2_block_number", "l2_block_hash", "max_fee_per_gas_wei", "max_priority_fee_per_gas_wei", "gas_used", "committed_ts_ns", "trace")
    BATCH_ID_FIELD_NUMBER: _ClassVar[int]
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    STATE_ANCHOR_CONTRACT_FIELD_NUMBER: _ClassVar[int]
    START_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    END_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    RAFT_COMMIT_INDEX_FIELD_NUMBER: _ClassVar[int]
    PREV_MERKLE_SUM_ROOT_FIELD_NUMBER: _ClassVar[int]
    NEW_MERKLE_SUM_ROOT_FIELD_NUMBER: _ClassVar[int]
    DECLARED_LIABILITIES_FP_FIELD_NUMBER: _ClassVar[int]
    VERIFIED_VAULT_RESERVES_FP_FIELD_NUMBER: _ClassVar[int]
    ZK_SOLVENCY_PROOF_BYTES_FIELD_NUMBER: _ClassVar[int]
    L2_COMMIT_TX_HASH_FIELD_NUMBER: _ClassVar[int]
    L2_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    L2_BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    MAX_FEE_PER_GAS_WEI_FIELD_NUMBER: _ClassVar[int]
    MAX_PRIORITY_FEE_PER_GAS_WEI_FIELD_NUMBER: _ClassVar[int]
    GAS_USED_FIELD_NUMBER: _ClassVar[int]
    COMMITTED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    batch_id: int
    epoch_id: int
    chain_id: int
    state_anchor_contract: bytes
    start_engine_seq: int
    end_engine_seq: int
    raft_commit_index: int
    prev_merkle_sum_root: bytes
    new_merkle_sum_root: bytes
    declared_liabilities_fp: _common_pb2.U128
    verified_vault_reserves_fp: _common_pb2.U128
    zk_solvency_proof_bytes: bytes
    l2_commit_tx_hash: bytes
    l2_block_number: int
    l2_block_hash: bytes
    max_fee_per_gas_wei: int
    max_priority_fee_per_gas_wei: int
    gas_used: int
    committed_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, batch_id: _Optional[int] = ..., epoch_id: _Optional[int] = ..., chain_id: _Optional[int] = ..., state_anchor_contract: _Optional[bytes] = ..., start_engine_seq: _Optional[int] = ..., end_engine_seq: _Optional[int] = ..., raft_commit_index: _Optional[int] = ..., prev_merkle_sum_root: _Optional[bytes] = ..., new_merkle_sum_root: _Optional[bytes] = ..., declared_liabilities_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., verified_vault_reserves_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., zk_solvency_proof_bytes: _Optional[bytes] = ..., l2_commit_tx_hash: _Optional[bytes] = ..., l2_block_number: _Optional[int] = ..., l2_block_hash: _Optional[bytes] = ..., max_fee_per_gas_wei: _Optional[int] = ..., max_priority_fee_per_gas_wei: _Optional[int] = ..., gas_used: _Optional[int] = ..., committed_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ChainReorgAlert(_message.Message):
    __slots__ = ("reorg_id", "chain_id", "common_ancestor_block_number", "common_ancestor_block_hash", "orphaned_tip_block_number", "orphaned_tip_block_hash", "new_canonical_tip_block_number", "new_canonical_tip_block_hash", "reorg_depth", "revoked_staged_deposit_ids", "requeued_outbox_tx_ids", "detected_ts_ns", "trace")
    REORG_ID_FIELD_NUMBER: _ClassVar[int]
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    COMMON_ANCESTOR_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    COMMON_ANCESTOR_BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    ORPHANED_TIP_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    ORPHANED_TIP_BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    NEW_CANONICAL_TIP_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    NEW_CANONICAL_TIP_BLOCK_HASH_FIELD_NUMBER: _ClassVar[int]
    REORG_DEPTH_FIELD_NUMBER: _ClassVar[int]
    REVOKED_STAGED_DEPOSIT_IDS_FIELD_NUMBER: _ClassVar[int]
    REQUEUED_OUTBOX_TX_IDS_FIELD_NUMBER: _ClassVar[int]
    DETECTED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    reorg_id: str
    chain_id: int
    common_ancestor_block_number: int
    common_ancestor_block_hash: bytes
    orphaned_tip_block_number: int
    orphaned_tip_block_hash: bytes
    new_canonical_tip_block_number: int
    new_canonical_tip_block_hash: bytes
    reorg_depth: int
    revoked_staged_deposit_ids: _containers.RepeatedScalarFieldContainer[str]
    requeued_outbox_tx_ids: _containers.RepeatedScalarFieldContainer[str]
    detected_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, reorg_id: _Optional[str] = ..., chain_id: _Optional[int] = ..., common_ancestor_block_number: _Optional[int] = ..., common_ancestor_block_hash: _Optional[bytes] = ..., orphaned_tip_block_number: _Optional[int] = ..., orphaned_tip_block_hash: _Optional[bytes] = ..., new_canonical_tip_block_number: _Optional[int] = ..., new_canonical_tip_block_hash: _Optional[bytes] = ..., reorg_depth: _Optional[int] = ..., revoked_staged_deposit_ids: _Optional[_Iterable[str]] = ..., requeued_outbox_tx_ids: _Optional[_Iterable[str]] = ..., detected_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class EscapeHatchForcedExit(_message.Message):
    __slots__ = ("exit_id", "chain_id", "account_id", "claimant_evm_address", "anchored_epoch_id", "sequencer_inactivity_blocks", "merkle_inclusion_proof", "l2_exit_tx_hash", "settled_ts_ns")
    EXIT_ID_FIELD_NUMBER: _ClassVar[int]
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    CLAIMANT_EVM_ADDRESS_FIELD_NUMBER: _ClassVar[int]
    ANCHORED_EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    SEQUENCER_INACTIVITY_BLOCKS_FIELD_NUMBER: _ClassVar[int]
    MERKLE_INCLUSION_PROOF_FIELD_NUMBER: _ClassVar[int]
    L2_EXIT_TX_HASH_FIELD_NUMBER: _ClassVar[int]
    SETTLED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    exit_id: str
    chain_id: int
    account_id: int
    claimant_evm_address: bytes
    anchored_epoch_id: int
    sequencer_inactivity_blocks: int
    merkle_inclusion_proof: _zk_solvency_pb2.InclusionSumProof
    l2_exit_tx_hash: bytes
    settled_ts_ns: int
    def __init__(self, exit_id: _Optional[str] = ..., chain_id: _Optional[int] = ..., account_id: _Optional[int] = ..., claimant_evm_address: _Optional[bytes] = ..., anchored_epoch_id: _Optional[int] = ..., sequencer_inactivity_blocks: _Optional[int] = ..., merkle_inclusion_proof: _Optional[_Union[_zk_solvency_pb2.InclusionSumProof, _Mapping]] = ..., l2_exit_tx_hash: _Optional[bytes] = ..., settled_ts_ns: _Optional[int] = ...) -> None: ...

class SubmitWithdrawalIntentResponse(_message.Message):
    __slots__ = ("withdrawal_id", "nullifier_hash", "status", "reject_reason")
    WITHDRAWAL_ID_FIELD_NUMBER: _ClassVar[int]
    NULLIFIER_HASH_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    REJECT_REASON_FIELD_NUMBER: _ClassVar[int]
    withdrawal_id: str
    nullifier_hash: bytes
    status: WithdrawalSettlementStatus
    reject_reason: str
    def __init__(self, withdrawal_id: _Optional[str] = ..., nullifier_hash: _Optional[bytes] = ..., status: _Optional[_Union[WithdrawalSettlementStatus, str]] = ..., reject_reason: _Optional[str] = ...) -> None: ...

class AnchorStateRootBatchResponse(_message.Message):
    __slots__ = ("batch_id", "epoch_id", "anchored", "l2_commit_tx_hash", "l2_block_number", "committed_batch")
    BATCH_ID_FIELD_NUMBER: _ClassVar[int]
    EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    ANCHORED_FIELD_NUMBER: _ClassVar[int]
    L2_COMMIT_TX_HASH_FIELD_NUMBER: _ClassVar[int]
    L2_BLOCK_NUMBER_FIELD_NUMBER: _ClassVar[int]
    COMMITTED_BATCH_FIELD_NUMBER: _ClassVar[int]
    batch_id: int
    epoch_id: int
    anchored: bool
    l2_commit_tx_hash: bytes
    l2_block_number: int
    committed_batch: StateRootBatchCommit
    def __init__(self, batch_id: _Optional[int] = ..., epoch_id: _Optional[int] = ..., anchored: _Optional[bool] = ..., l2_commit_tx_hash: _Optional[bytes] = ..., l2_block_number: _Optional[int] = ..., committed_batch: _Optional[_Union[StateRootBatchCommit, _Mapping]] = ...) -> None: ...

class GetRelayerStatusRequest(_message.Message):
    __slots__ = ("chain_id",)
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    chain_id: int
    def __init__(self, chain_id: _Optional[int] = ...) -> None: ...

class RelayerStatusResponse(_message.Message):
    __slots__ = ("chain_id", "latest_observed_block", "latest_finalized_block", "latest_anchored_epoch_id", "pending_outbox_count", "total_reorgs_handled", "escape_hatch_frozen_mode")
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    LATEST_OBSERVED_BLOCK_FIELD_NUMBER: _ClassVar[int]
    LATEST_FINALIZED_BLOCK_FIELD_NUMBER: _ClassVar[int]
    LATEST_ANCHORED_EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    PENDING_OUTBOX_COUNT_FIELD_NUMBER: _ClassVar[int]
    TOTAL_REORGS_HANDLED_FIELD_NUMBER: _ClassVar[int]
    ESCAPE_HATCH_FROZEN_MODE_FIELD_NUMBER: _ClassVar[int]
    chain_id: int
    latest_observed_block: int
    latest_finalized_block: int
    latest_anchored_epoch_id: int
    pending_outbox_count: int
    total_reorgs_handled: int
    escape_hatch_frozen_mode: bool
    def __init__(self, chain_id: _Optional[int] = ..., latest_observed_block: _Optional[int] = ..., latest_finalized_block: _Optional[int] = ..., latest_anchored_epoch_id: _Optional[int] = ..., pending_outbox_count: _Optional[int] = ..., total_reorgs_handled: _Optional[int] = ..., escape_hatch_frozen_mode: _Optional[bool] = ...) -> None: ...

class StreamSettlementEventsRequest(_message.Message):
    __slots__ = ("chain_id",)
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    chain_id: int
    def __init__(self, chain_id: _Optional[int] = ...) -> None: ...

class SettlementEventEnvelope(_message.Message):
    __slots__ = ("chain_id", "ts_ns", "deposit_finalized", "withdrawal_intent", "batch_commit", "reorg_alert", "forced_exit")
    CHAIN_ID_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    DEPOSIT_FINALIZED_FIELD_NUMBER: _ClassVar[int]
    WITHDRAWAL_INTENT_FIELD_NUMBER: _ClassVar[int]
    BATCH_COMMIT_FIELD_NUMBER: _ClassVar[int]
    REORG_ALERT_FIELD_NUMBER: _ClassVar[int]
    FORCED_EXIT_FIELD_NUMBER: _ClassVar[int]
    chain_id: int
    ts_ns: int
    deposit_finalized: DepositFinalized
    withdrawal_intent: WithdrawalEIP712Intent
    batch_commit: StateRootBatchCommit
    reorg_alert: ChainReorgAlert
    forced_exit: EscapeHatchForcedExit
    def __init__(self, chain_id: _Optional[int] = ..., ts_ns: _Optional[int] = ..., deposit_finalized: _Optional[_Union[DepositFinalized, _Mapping]] = ..., withdrawal_intent: _Optional[_Union[WithdrawalEIP712Intent, _Mapping]] = ..., batch_commit: _Optional[_Union[StateRootBatchCommit, _Mapping]] = ..., reorg_alert: _Optional[_Union[ChainReorgAlert, _Mapping]] = ..., forced_exit: _Optional[_Union[EscapeHatchForcedExit, _Mapping]] = ...) -> None: ...
