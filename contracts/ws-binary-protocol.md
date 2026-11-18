# HYPERION — SUB-MILLISECOND BINARY WEBSOCKET WIRE PROTOCOL (`v1`)

- **Endpoint**: `ws://localhost:8080/ws` (TLS Production: `wss://<host>/ws`)
- **Byte Order**: **Strict Little-Endian (`LE`)** across all multi-byte integers (zero byte-swapping on x86_64/ARM64 WebAssembly & DataView `true` flag).
- **Floating-Point Ban**: **0 floats/doubles**. Every price, quantity, volume, and fee is fixed-point integer scaled by $10^8$ (`FP_SCALE = 100_000_000`).
- **Negotiation**:
  - Default binary mode: `Sec-WebSocket-Protocol: hyperion.bin.v1` (or query `?format=binary`, default).
  - Optional debug/fallback JSON text mode: `?format=json` (`Sec-WebSocket-Protocol: hyperion.json.v1`).

---

## 1. UNIVERSAL 32-BYTE FRAME HEADER (CACHE-LINE HALF-ALIGNED)

Every binary WebSocket message (`opcode = 0x2`) starts with an exact **32-byte fixed header**:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          magic (0x4859)       | version (0x01)|   msg_type    |  Bytes 0..3
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     flags     |   reserved    |         instrument_id         |  Bytes 4..7
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          payload_len                          |  Bytes 8..11
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         crc32c_digest                         |  Bytes 12..15
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                          engine_seq                           +  Bytes 16..23
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                         timestamp_ns                          +  Bytes 24..31
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### Header Field Layout

| Byte Offset | Size | Type (LE) | Field | Description |
|---|---|---|---|---|
| `0..2` | `2 B` | `u16` | `magic` | Constant `0x4859` (ASCII `'Y'`, `'H'` in LE; `DataView.getUint16(0, true) === 0x4859`) |
| `2` | `1 B` | `u8` | `version` | Protocol version = `0x01` |
| `3` | `1 B` | `u8` | `msg_type` | Frame type discriminator (`0x01`..`0x7F` server->client, `0x80`..`0xFE` client->server) |
| `4` | `1 B` | `u8` | `flags` | Bitfield: `bit0 (0x01)` = `CRC32C_PRESENT`, `bit1 (0x02)` = `SNAPSHOT_RESYNC`, `bit2 (0x04)` = `SLOW_CONSUMER_DROPPED_PRIOR`, `bit3 (0x08)` = `REPLAY_TIMELINE` |
| `5` | `1 B` | `u8` | `reserved` | Must be `0x00` |
| `6..8` | `2 B` | `u16` | `instrument_id` | `0` = global/system, `1` = `BTC-USDT`, `2` = `ETH-USDT`, `3` = `SOL-USDT`, `4` = `HYPE-USDT` |
| `8..12` | `4 B` | `u32` | `payload_len` | Exact byte length of payload immediately following byte `31` |
| `12..16` | `4 B` | `u32` | `crc32c_digest` | CRC-32C of payload bytes (`0` if `flags & 0x01 == 0`) |
| `16..24` | `8 B` | `u64` | `engine_seq` | Deterministic monotonic matching-engine sequence ID (`0` for pure heartbeats/client subs) |
| `24..32` | `8 B` | `u64` | `timestamp_ns` | UTC epoch nanoseconds |

Total frame byte length = `32 + payload_len`. If a frame fails `magic === 0x4859`, `version === 1`, or `byteLength === 32 + payload_len`, the receiver MUST close connection with WebSocket status `1002 Protocol Error`.

---

## 2. MESSAGE TYPE REGISTRY (`msg_type`)

### Server -> Client (`0x01` – `0x08`)
| `msg_type` | Constant Name | Channel Criticality | Payload Description |
|---|---|---|---|
| `0x01` | `MSG_HEARTBEAT` | Control | 16-byte server heartbeat & gateway queue pressure telemetry |
| `0x02` | `MSG_L2_SNAPSHOT` | Resync / State | Full top-N Level-2 order book snapshot (bids + asks) |
| `0x03` | `MSG_L3_DELTA_BATCH` | Droppable w/ Flag | Packed batch of 1..N Level-3 book mutations + `prev_engine_seq` |
| `0x04` | `MSG_EXECUTION_FILL` | Critical | Public trade print or private account execution fill |
| `0x05` | `MSG_CANDLE_MICRO` | Droppable | OHLCV candlestick tick + real-time OFI / VWAP / Imbalance metrics |
| `0x06` | `MSG_FRAUD_ALERT` | Critical | Compact binary header + UTF-8/Protobuf alert summary for Force Graph UI |
| `0x07` | `MSG_ORDER_ACK_PRIVATE` | Critical (Never Drop) | Private order lifecycle update (`ACCEPTED`/`FILLED`/`CANCELED`/`REJECTED`) |
| `0x08` | `MSG_GAP_RESYNC_NOTICE` | Control | Sent before an automatic L2 snapshot resync after sequence gap / slow-consumer drop |

### Client -> Server (`0x81` – `0x83`)
| `msg_type` | Constant Name | Payload Description |
|---|---|---|
| `0x81` | `CMD_SUBSCRIBE` | Subscribe to channels + instrument bitmap |
| `0x82` | `CMD_UNSUBSCRIBE` | Unsubscribe from channels + instrument bitmap |
| `0x83` | `CMD_RESYNC_REQUEST` | Client detected `prev_engine_seq != local_seq`; requests immediate `MSG_L2_SNAPSHOT` |

*(Note: For developer convenience, `gateway-go` ALSO accepts JSON text frames `{"op":"subscribe","channels":["l3","fills","candles","fraud"],"symbols":["BTC-USDT"]}` on the same WebSocket and still streams binary frames back when in binary mode).*

---

## 3. EXACT BINARY PAYLOAD LAYOUTS (LITTLE-ENDIAN)

### 3.1 `0x01` — `MSG_HEARTBEAT` (`payload_len = 16`)
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..8` | `8 B` | `u64` | `gateway_monotonic_ns` |
| `8..12` | `4 B` | `u32` | `connected_clients` |
| `12..16` | `4 B` | `u32` | `client_outbound_queue_depth` |

### 3.2 `0x02` — `MSG_L2_SNAPSHOT` (`payload_len = 48 + (bid_count + ask_count) * 20`)
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..8` | `8 B` | `u64` | `best_bid_fp` (scale $10^8$) |
| `8..16` | `8 B` | `u64` | `best_ask_fp` (scale $10^8$) |
| `16..18` | `2 B` | `u16` | `bid_count` ($N_b$) |
| `18..20` | `2 B` | `u16` | `ask_count` ($N_a$) |
| `20..24` | `4 B` | `u32` | `reserved_zero` |
| `24..48` | `24 B` | `[u8; 24]` | First 24 bytes of `state_root_blake3` (truncated state fingerprint; full 32B via gRPC/REST) — *wait: use full 32B below!* |

Correction — **Full 32-byte `state_root_blake3` (`payload_len = 56 + (bid_count + ask_count) * 20`)**:
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..8` | `8 B` | `u64` | `best_bid_fp` (scale $10^8$) |
| `8..16` | `8 B` | `u64` | `best_ask_fp` (scale $10^8$) |
| `16..18` | `2 B` | `u16` | `bid_count` ($N_b$, sorted descending) |
| `18..20` | `2 B` | `u16` | `ask_count` ($N_a$, sorted ascending) |
| `20..24` | `4 B` | `u32` | `reserved_zero` (`0`) |
| `24..56` | `32 B` | `[u8; 32]` | `state_root_blake3` (full 256-bit Blake3 Merkle root) |
| `56..` | `20 B * (N_b + N_a)` | `LevelRecord[]` | `N_b` bid levels followed by `N_a` ask levels |

Each **`LevelRecord` (`20 bytes`)**:
- `0..8`: `price_fp` (`u64`, scale $10^8$)
- `8..16`: `total_qty_fp` (`u64`, scale $10^8$)
- `16..20`: `order_count` (`u32`)

### 3.3 `0x03` — `MSG_L3_DELTA_BATCH` (`payload_len = 16 + count * 40`)
Enables zero-gap client order-book maintenance in the dedicated WebWorker:
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..8` | `8 B` | `u64` | `prev_engine_seq` (sequence immediately preceding the first delta in this batch) |
| `8..10` | `2 B` | `u16` | `delta_count` ($K \ge 1$) |
| `10..16` | `6 B` | `[u8; 6]` | `reserved_zero` |
| `16..` | `40 B * K` | `L3DeltaRecord[]` | Packed array of $K$ deltas in ascending `engine_seq` order |

Each **`L3DeltaRecord` (`40 bytes`)**:
- `0..8`: `item_engine_seq` (`u64`)
- `8..16`: `order_id` (`u64`)
- `16..24`: `price_fp` (`u64`, scale $10^8$)
- `24..32`: `remaining_qty_fp` (`u64`, scale $10^8$)
- `32..40`: packed metadata (`8 B`):
  - byte `32`: `op` (`u8`: `1=ADD`, `2=MODIFY`, `3=CANCEL`, `4=FILL`, `5=CLEAR`)
  - byte `33`: `side` (`u8`: `1=BUY`, `2=SELL`)
  - bytes `34..36`: `reserved` (`u16 = 0`)
  - bytes `36..40`: `delta_qty_sign_and_flags` (`i32` reserved / tick offset `0`)

**Sequence Continuity Rule**:
Client WebWorker maintains `local_seq` per `instrument_id`. On receiving `MSG_L3_DELTA_BATCH`:
- If `prev_engine_seq == local_seq`, apply all $K$ records and advance `local_seq = last_record.item_engine_seq`.
- If `prev_engine_seq < local_seq`, ignore records with `item_engine_seq <= local_seq` (idempotent replay overlap).
- If `prev_engine_seq > local_seq` OR `(header.flags & 0x04) != 0`, client emits `CMD_RESYNC_REQUEST` (`0x83`) and buffers incoming deltas until `MSG_L2_SNAPSHOT` (`0x02`) arrives.

### 3.4 `0x04` — `MSG_EXECUTION_FILL` (`payload_len = 56`)
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..8` | `8 B` | `u64` | `fill_id` |
| `8..16` | `8 B` | `u64` | `maker_order_id` |
| `16..24` | `8 B` | `u64` | `taker_order_id` |
| `24..32` | `8 B` | `u64` | `maker_account_id` (`0` on anonymized public feed if configured; populated on forensic/admin feed) |
| `32..40` | `8 B` | `u64` | `taker_account_id` |
| `40..48` | `8 B` | `u64` | `price_fp` (scale $10^8$) |
| `48..55` | `7 B` | — | *Aligned to 64B below for clean 64-bit reads!* |

*(Aligned 64-byte `MSG_EXECUTION_FILL` payload, `payload_len = 64`)*:
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..8` | `8 B` | `u64` | `fill_id` |
| `8..16` | `8 B` | `u64` | `maker_order_id` |
| `16..24` | `8 B` | `u64` | `taker_order_id` |
| `24..32` | `8 B` | `u64` | `maker_account_id` |
| `32..40` | `8 B` | `u64` | `taker_account_id` |
| `40..48` | `8 B` | `u64` | `price_fp` (scale $10^8$) |
| `48..56` | `8 B` | `u64` | `qty_fp` (scale $10^8$) |
| `56` | `1 B` | `u8` | `aggressor_side` (`1=BUY`, `2=SELL`) |
| `57..64` | `7 B` | `[u8; 7]` | `reserved_zero` |

### 3.5 `0x05` — `MSG_CANDLE_MICRO` (`payload_len = 96`)
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0` | `1 B` | `u8` | `interval` (`1=1s`, `2=5s`, `3=1m`, `4=5m`, `5=15m`, `6=1h`) |
| `1` | `1 B` | `u8` | `is_closed` (`0` or `1`) |
| `2..8` | `6 B` | `[u8; 6]` | `reserved_zero` |
| `8..16` | `8 B` | `u64` | `bucket_open_ts_ns` |
| `16..24` | `8 B` | `u64` | `open_fp` (scale $10^8$) |
| `24..32` | `8 B` | `u64` | `high_fp` (scale $10^8$) |
| `32..40` | `8 B` | `u64` | `low_fp` (scale $10^8$) |
| `40..48` | `8 B` | `u64` | `close_fp` (scale $10^8$) |
| `48..56` | `8 B` | `u64` | `base_volume_lo_fp` (low 64 bits of `U128`, scale $10^8$) |
| `56..64` | `8 B` | `u64` | `vwap_fp` (scale $10^8$) |
| `64..72` | `8 B` | `i64` | `ofi_lo_fp` (signed Order Flow Imbalance, scale $10^8$) |
| `72..80` | `8 B` | `i64` | `book_imbalance_top5_ppm` (`[-1_000_000, +1_000_000]`) |
| `80..88` | `8 B` | `i64` | `kyles_lambda_fp` (scale $10^8$) |
| `88..96` | `8 B` | `u64` | `trade_count` |

### 3.6 `0x06` — `MSG_FRAUD_ALERT` (Variable Length)
Payload contains exact serialized Protobuf bytes of `hyperion.v1.AnomalyAlert` (`payload_len` bytes), allowing full fidelity access to `WashTradingCycle` participant rings, `SpoofingLayering` evidence, and `SHAPFeatureAttribution` vectors without lossy truncation.

### 3.7 `0x07` — `MSG_ORDER_ACK_PRIVATE` (Variable Length)
Payload contains exact serialized Protobuf bytes of `hyperion.v1.OrderAck` (`payload_len` bytes).

### 3.8 `0x81` / `0x82` / `0x83` — Client Control Frames (`payload_len = 16`)
| Offset (rel) | Size | Type | Field |
|---|---|---|---|
| `0..4` | `4 B` | `u32` | `channel_mask` (`0x01=L2`, `0x02=L3`, `0x04=FILLS`, `0x08=CANDLES`, `0x10=FRAUD`, `0x20=PRIVATE_ORDERS`) |
| `4..8` | `4 B` | `u32` | `instrument_mask` (`bit1=BTC-USDT`, `bit2=ETH-USDT`, `bit3=SOL-USDT`, `bit4=HYPE-USDT`, `0xFFFFFFFF=ALL`) |
| `8..16` | `8 B` | `u64` | `last_known_engine_seq` (used by `0x83 CMD_RESYNC_REQUEST`) |
