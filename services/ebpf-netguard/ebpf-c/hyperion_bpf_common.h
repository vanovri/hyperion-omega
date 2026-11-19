/* SPDX-License-Identifier: Dual BSD/GPL */
/*
 * HYPERION OMEGA — Channel 16: eBPF/XDP Zero-Trust Packet Shield & Telemetry
 * Shared ABI definitions between Linux Kernel eBPF (XDP/TC/Tracepoint) and
 * Rust Userspace Control Daemon (netguard-daemon / netguard-common).
 */
#ifndef HYPERION_BPF_COMMON_H
#define HYPERION_BPF_COMMON_H

#ifdef __BPF__
#include <linux/types.h>
#else
#include <stdint.h>
typedef uint8_t  __u8;
typedef uint16_t __u16;
typedef uint32_t __u32;
typedef uint64_t __u64;
typedef int32_t  __s32;
typedef int64_t  __s64;
#endif

/* HYPERION v1 Binary Wire Protocol Constants (Little-Endian on wire) */
#define HYPERION_WIRE_MAGIC_LE      0x4859U /* Wire bytes: [0x59 ('Y'), 0x48 ('H')] */
#define HYPERION_WIRE_VERSION       0x01U
#define HYPERION_WIRE_HEADER_LEN    32U
#define HYPERION_MAX_INSTRUMENT_ID  64U
#define HYPERION_MAX_PAYLOAD_BYTES  (4U * 1024U * 1024U)

/* NetGuard Hairpin XDP_TX Wire-Speed Probe Magic ("HYPPING1") */
#define HYPERION_XDP_PROBE_MAGIC_HI 0x50505948U /* "HYPP" LE */
#define HYPERION_XDP_PROBE_MAGIC_LO 0x31474E49U /* "ING1" LE */
#define HYPERION_XDP_PROBE_UDP_PORT 18095U

/* Protected Go Edge Gateway Default Ports */
#define HYPERION_GATEWAY_PORT_HTTP_WS 8080U
#define HYPERION_GATEWAY_PORT_METRICS 8081U

/* Rule Policy Flags */
#define RULE_ACTION_ALLOW             0U
#define RULE_ACTION_HARD_DROP         1U
#define RULE_ACTION_DROP_NEW_SYN_ONLY 2U
#define RULE_ACTION_THROTTLE_RPS      3U
#define RULE_ACTION_SYN_COOKIE_TX     4U

/* Drop / Classification Reason Codes (mirrored 1:1 in Rust DropReason) */
#define REASON_NONE                         0U
#define REASON_TRUNCATED_ETH                1U
#define REASON_TRUNCATED_VLAN               2U
#define REASON_NON_IP_PASS                  3U
#define REASON_TRUNCATED_IPV4               4U
#define REASON_INVALID_IPV4_IHL_OR_VER      5U
#define REASON_INVALID_IPV4_TOT_LEN         6U
#define REASON_MALFORMED_IPV4_FRAGMENT      7U
#define REASON_LAND_ATTACK                  8U
#define REASON_TRUNCATED_IPV6               9U
#define REASON_INVALID_IPV6_VER_OR_LEN      10U
#define REASON_IPV4_BLOCKLIST_MATCH         11U
#define REASON_IPV6_BLOCKLIST_MATCH         12U
#define REASON_TRUNCATED_TCP                13U
#define REASON_INVALID_TCP_DOFF             14U
#define REASON_INVALID_TCP_FLAGS            15U
#define REASON_TRUNCATED_UDP                16U
#define REASON_INVALID_UDP_LEN              17U
#define REASON_SYN_FLOOD_RATE_LIMITED       18U
#define REASON_WS_STORM_RATE_LIMITED        19U
#define REASON_HYPERION_BAD_MAGIC           20U
#define REASON_HYPERION_BAD_VERSION         21U
#define REASON_HYPERION_NONZERO_RESERVED    22U
#define REASON_HYPERION_INVALID_MSG_TYPE    23U
#define REASON_HYPERION_PAYLOAD_OVERSIZE    24U
#define REASON_ACCOUNT_FROZEN_MITIGATION    25U
#define REASON_XDP_TX_PROBE_REPLY           26U
#define REASON_XDP_TX_SYN_COOKIE            27U
#define REASON_ESTABLISHED_CONNTRACK_PASS   28U
#define REASON_MAX_COUNT                    32U

/* HDR Log2-Linear Histogram Configuration:
 * 64 coarse log2 buckets x 8 linear sub-buckets = 512 bins (4 KiB per CPU).
 * Pure integer nanosecond resolution with <= 6.25% max quantization error.
 */
#define HDR_COARSE_BUCKETS 64U
#define HDR_SUB_BUCKETS    8U
#define HDR_TOTAL_BINS     (HDR_COARSE_BUCKETS * HDR_SUB_BUCKETS)

struct ipv4_lpm_key {
    __u32 prefixlen; /* 0..32 */
    __u32 addr_be;   /* Network byte order IPv4 */
};

struct ipv6_lpm_key {
    __u32 prefixlen;  /* 0..128 */
    __u8  addr[16];   /* Network byte order IPv6 */
};

struct block_rule_val {
    __u32 action;          /* RULE_ACTION_* */
    __u32 rate_override;   /* Enforced RPS when action == THROTTLE_RPS */
    __u64 expires_ts_ns;   /* 0 = permanent until revoked */
    __u64 rule_generation; /* Monotonic RCU rule epoch */
};

struct token_bucket_val {
    __u64 tokens_scaled;   /* Scaled by 1,000,000 (micro-tokens) */
    __u64 last_refill_ns;  /* Monotonic ktime_ns */
    __u64 rate_per_sec;    /* Allowed packets/sec */
    __u64 burst_capacity;  /* Max burst packets */
    __u64 dropped_packets; /* Cumulative drops for this source */
};

struct conntrack_5tuple_key {
    __u8  src_ip[16]; /* IPv4 mapped in [12..16] or full IPv6 */
    __u8  dst_ip[16];
    __u16 src_port;
    __u16 dst_port;
    __u8  proto;
    __u8  pad[3];
};

struct conntrack_val {
    __u64 established_ts_ns;
    __u64 last_seen_ts_ns;
    __u64 account_id;
    __u8  state; /* 1 = ESTABLISHED, 2 = CLOSING */
    __u8  pad[7];
};

struct netguard_ringbuf_event {
    __u64 ts_ns;
    __u32 src_ipv4_be;
    __u16 src_port;
    __u16 dst_port;
    __u8  xdp_action;
    __u8  reason_code;
    __u8  ip_version;
    __u8  l4_proto;
    __u32 pkt_len;
};

#endif /* HYPERION_BPF_COMMON_H */
