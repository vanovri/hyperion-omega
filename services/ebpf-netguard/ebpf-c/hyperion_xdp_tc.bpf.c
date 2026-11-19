// SPDX-License-Identifier: Dual BSD/GPL
/*
 * HYPERION OMEGA — Channel 16: Kernel eBPF XDP & TC Zero-Trust Shield
 *
 * Compile with:
 *   clang -O2 -g -Wall -Werror -target bpf -D__BPF__ -D__TARGET_ARCH_x86 \
 *         -I. -c hyperion_xdp_tc.bpf.c -o ../artifacts/hyperion_netguard.bpf.o
 *
 * Strict eBPF Verifier Guarantees:
 *   1. Every packet header dereference is dominated by an explicit pointer
 *      comparison `(void *)(hdr + 1) > data_end` or `cursor + N > data_end`.
 *   2. Variable-length headers (IPv4 IHL 20..60B, TCP DOFF 20..60B) validate
 *      both minimum length (`>= 20`) and exact header end against `data_end`.
 *   3. VLAN 802.1Q / 802.1ad tag parsing uses `#pragma unroll` with hard max
 *      depth = 2 (bounded DAG, zero unbounded backward jumps).
 *   4. Zero floating-point instructions; all rate-limiting & HDR latency
 *      histograms use pure 64-bit integer arithmetic & CLZ bitscan.
 */

#include "hyperion_bpf_common.h"

/* Minimal standalone kernel BPF ABI definitions (compiles cleanly even without
 * distro-specific kernel headers installed in minimal build containers). */
#ifndef XDP_ABORTED
enum xdp_action {
    XDP_ABORTED = 0,
    XDP_DROP = 1,
    XDP_PASS = 2,
    XDP_TX = 3,
    XDP_REDIRECT = 4,
};
#endif

#define TC_ACT_OK   0
#define TC_ACT_SHOT 2

#define BPF_MAP_TYPE_HASH            1
#define BPF_MAP_TYPE_PERCPU_ARRAY    6
#define BPF_MAP_TYPE_LRU_HASH        9
#define BPF_MAP_TYPE_LPM_TRIE        11
#define BPF_MAP_TYPE_RINGBUF         27
#define BPF_F_NO_PREALLOC            (1U << 0)
#define BPF_ANY                      0

#define ETH_P_IP    0x0800
#define ETH_P_IPV6  0x86DD
#define ETH_P_8021Q 0x8100
#define ETH_P_8021AD 0x88A8

#define IPPROTO_ICMP 1
#define IPPROTO_TCP  6
#define IPPROTO_UDP  17

#define SEC(name) __attribute__((section(name), used))
#define __always_inline inline __attribute__((always_inline))

#if __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
#define bpf_htons(x) __builtin_bswap16(x)
#define bpf_ntohs(x) __builtin_bswap16(x)
#define bpf_htonl(x) __builtin_bswap32(x)
#define bpf_ntohl(x) __builtin_bswap32(x)
#else
#define bpf_htons(x) (x)
#define bpf_ntohs(x) (x)
#define bpf_htonl(x) (x)
#define bpf_ntohl(x) (x)
#endif

struct xdp_md {
    __u32 data;
    __u32 data_end;
    __u32 data_meta;
    __u32 ingress_ifindex;
    __u32 rx_queue_index;
    __u32 egress_ifindex;
};

struct __sk_buff {
    __u32 len;
    __u32 pkt_type;
    __u32 mark;
    __u32 queue_mapping;
    __u32 protocol;
    __u32 vlan_present;
    __u32 vlan_tci;
    __u32 vlan_proto;
    __u32 priority;
    __u32 ingress_ifindex;
    __u32 ifindex;
    __u32 tc_index;
    __u32 cb[5];
    __u32 hash;
    __u32 tc_classid;
    __u32 data;
    __u32 data_end;
    __u32 napi_id;
    __u32 family;
    __u32 remote_ip4;
    __u32 local_ip4;
    __u32 remote_ip6[4];
    __u32 local_ip6[4];
    __u32 remote_port;
    __u32 local_port;
    __u32 data_meta;
    __u64 tstamp;
};

struct ethhdr {
    __u8  h_dest[6];
    __u8  h_source[6];
    __u16 h_proto;
} __attribute__((packed));

struct vlan_hdr {
    __u16 h_vlan_TCI;
    __u16 h_vlan_encapsulated_proto;
} __attribute__((packed));

struct iphdr {
    __u8  ihl_version;
    __u8  tos;
    __u16 tot_len;
    __u16 id;
    __u16 frag_off;
    __u8  ttl;
    __u8  protocol;
    __u16 check;
    __u32 saddr;
    __u32 daddr;
} __attribute__((packed));

struct ipv6hdr {
    __u32 ver_tc_flow;
    __u16 payload_len;
    __u8  nexthdr;
    __u8  hop_limit;
    __u8  saddr[16];
    __u8  daddr[16];
} __attribute__((packed));

struct tcphdr {
    __u16 source;
    __u16 dest;
    __u32 seq;
    __u32 ack_seq;
    __u8  doff_res;
    __u8  flags;
    __u16 window;
    __u16 check;
    __u16 urg_ptr;
} __attribute__((packed));

struct udphdr {
    __u16 source;
    __u16 dest;
    __u16 len;
    __u16 check;
} __attribute__((packed));

/* BPF Helper Function Pointers */
static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, __u64 flags) = (void *)2;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, __u64 size, __u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, __u64 flags) = (void *)132;

/* BTF-style BPF Map Definition Macro */
#define __uint(name, val) int (*name)[val]
#define __type(name, val) typeof(val) *name

/* 1. IPv4 CIDR Blocklist (LPM Trie, O(prefix) NIC-speed XDP_DROP) */
struct {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 65536);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct ipv4_lpm_key);
    __type(value, struct block_rule_val);
} ipv4_blocklist_lpm SEC(".maps");

/* 2. IPv6 CIDR Blocklist (LPM Trie) */
struct {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 65536);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct ipv6_lpm_key);
    __type(value, struct block_rule_val);
} ipv6_blocklist_lpm SEC(".maps");

/* 3. Per-Source-IP Token Bucket Rate Limiter (SYN Flood & WS Storm Defense) */
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 131072);
    __type(key, __u32);
    __type(value, struct token_bucket_val);
} ipv4_token_buckets SEC(".maps");

/* 4. Established TCP 5-Tuple Conntrack Table (Zero-Disruption Graceful Mitigation) */
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 262144);
    __type(key, struct conntrack_5tuple_key);
    __type(value, struct conntrack_val);
} established_sessions SEC(".maps");

/* 5. Per-CPU Lock-Free Reason/Action Telemetry Counters */
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, REASON_MAX_COUNT);
    __type(key, __u32);
    __type(value, __u64);
} reason_counters_percpu SEC(".maps");

/* 6. Per-CPU Lock-Free TCP RTT Log2-Linear HDR Histogram (512 bins) */
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, HDR_TOTAL_BINS);
    __type(key, __u32);
    __type(value, __u64);
} tcp_rtt_hdr_hist_percpu SEC(".maps");

/* 7. Flow Timestamp Map for Kernel Egress->Ingress TCP RTT Measurement */
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 131072);
    __type(key, struct conntrack_5tuple_key);
    __type(value, __u64);
} flow_tx_tstamp_ns SEC(".maps");

/* 8. Security Drop Event RingBuffer to Userspace Daemon */
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 1 << 20); /* 1 MiB */
} security_events_rb SEC(".maps");

static __always_inline void bump_reason(__u32 reason)
{
    if (reason >= REASON_MAX_COUNT)
        return;
    __u64 *cnt = bpf_map_lookup_elem(&reason_counters_percpu, &reason);
    if (cnt)
        *cnt += 1;
}

static __always_inline __u32 hdr_bin_index(__u64 value_ns)
{
    if (value_ns < 8ULL)
        return (__u32)value_ns;
    __u32 lz = (__u32)__builtin_clzll(value_ns);
    __u32 msb = 63U - lz; /* 3..63 */
    __u32 shift = msb - 3U;
    __u32 sub = (__u32)((value_ns >> shift) & 0x7ULL);
    __u32 bin = ((msb - 2U) * HDR_SUB_BUCKETS) + sub;
    if (bin >= HDR_TOTAL_BINS)
        bin = HDR_TOTAL_BINS - 1U;
    return bin;
}

static __always_inline void swap_eth_mac(struct ethhdr *eth)
{
    __u8 tmp[6];
#pragma unroll
    for (int i = 0; i < 6; i++) {
        tmp[i] = eth->h_source[i];
        eth->h_source[i] = eth->h_dest[i];
        eth->h_dest[i] = tmp[i];
    }
}

static __always_inline int check_hyperion_l7_payload(const __u8 *l7, const void *data_end)
{
    if ((const void *)(l7 + 4) > data_end)
        return REASON_NONE; /* Short ACK / keepalive segment without L7 frame */

    /* Allow standard HTTP/1.1 REST / WebSocket Upgrade & TLS 1.2/1.3 ClientHello */
    if ((l7[0] == 'G' && l7[1] == 'E' && l7[2] == 'T' && l7[3] == ' ') ||
        (l7[0] == 'P' && l7[1] == 'O' && l7[2] == 'S' && l7[3] == 'T') ||
        (l7[0] == 'D' && l7[1] == 'E' && l7[2] == 'L' && l7[3] == 'E') ||
        (l7[0] == 0x16 && l7[1] == 0x03)) {
        return REASON_NONE;
    }

    /* Case A: RFC 6455 Binary WebSocket Frame (FIN|OP_BINARY = 0x82) */
    if (l7[0] == 0x82) {
        __u8 b1 = l7[1];
        __u8 masked = b1 & 0x80;
        __u8 len7 = b1 & 0x7F;
        const __u8 *hdr_ptr = l7 + 2;
        if (len7 == 126)
            hdr_ptr = l7 + 4;
        else if (len7 == 127)
            hdr_ptr = l7 + 10;

        if (masked) {
            if ((const void *)(hdr_ptr + 4 + HYPERION_WIRE_HEADER_LEN) > data_end)
                return REASON_HYPERION_BAD_MAGIC;
            const __u8 *mask = hdr_ptr;
            const __u8 *p = hdr_ptr + 4;
            __u8 u0 = p[0] ^ mask[0];
            __u8 u1 = p[1] ^ mask[1];
            __u8 ver = p[2] ^ mask[2];
            __u8 msg_type = p[3] ^ mask[3];
            __u8 rsv = p[5] ^ mask[1];
            __u16 magic = (__u16)u0 | ((__u16)u1 << 8);
            if (magic != HYPERION_WIRE_MAGIC_LE)
                return REASON_HYPERION_BAD_MAGIC;
            if (ver != HYPERION_WIRE_VERSION)
                return REASON_HYPERION_BAD_VERSION;
            if (rsv != 0)
                return REASON_HYPERION_NONZERO_RESERVED;
            if (!((msg_type >= 0x01 && msg_type <= 0x08) ||
                  (msg_type >= 0x81 && msg_type <= 0x83)))
                return REASON_HYPERION_INVALID_MSG_TYPE;
            return REASON_NONE;
        } else {
            if ((const void *)(hdr_ptr + HYPERION_WIRE_HEADER_LEN) > data_end)
                return REASON_HYPERION_BAD_MAGIC;
            l7 = hdr_ptr;
        }
    }

    /* Case B: Direct Raw HYPERION 32-Byte Binary Frame */
    if ((const void *)(l7 + HYPERION_WIRE_HEADER_LEN) > data_end)
        return REASON_HYPERION_BAD_MAGIC;

    __u16 magic = (__u16)l7[0] | ((__u16)l7[1] << 8);
    if (magic != HYPERION_WIRE_MAGIC_LE)
        return REASON_HYPERION_BAD_MAGIC;
    if (l7[2] != HYPERION_WIRE_VERSION)
        return REASON_HYPERION_BAD_VERSION;
    if (l7[5] != 0)
        return REASON_HYPERION_NONZERO_RESERVED;

    __u8 msg_type = l7[3];
    if (!((msg_type >= 0x01 && msg_type <= 0x08) ||
          (msg_type >= 0x81 && msg_type <= 0x83)))
        return REASON_HYPERION_INVALID_MSG_TYPE;

    __u32 payload_len = (__u32)l7[8] | ((__u32)l7[9] << 8) |
                        ((__u32)l7[10] << 16) | ((__u32)l7[11] << 24);
    if (payload_len > HYPERION_MAX_PAYLOAD_BYTES)
        return REASON_HYPERION_PAYLOAD_OVERSIZE;

    return REASON_NONE;
}

SEC("xdp")
int hyperion_xdp_ingress(struct xdp_md *ctx)
{
    void *data = (void *)(long)ctx->data;
    void *data_end = (void *)(long)ctx->data_end;

    struct ethhdr *eth = data;
    if ((void *)(eth + 1) > data_end) {
        bump_reason(REASON_TRUNCATED_ETH);
        return XDP_DROP;
    }

    __u16 h_proto = bpf_ntohs(eth->h_proto);
    void *cursor = (void *)(eth + 1);

#pragma unroll
    for (int i = 0; i < 2; i++) {
        if (h_proto == ETH_P_8021Q || h_proto == ETH_P_8021AD) {
            struct vlan_hdr *vh = cursor;
            if ((void *)(vh + 1) > data_end) {
                bump_reason(REASON_TRUNCATED_VLAN);
                return XDP_DROP;
            }
            h_proto = bpf_ntohs(vh->h_vlan_encapsulated_proto);
            cursor = (void *)(vh + 1);
        }
    }

    __u64 now_ns = bpf_ktime_get_ns();
    __u8 l4_proto = 0;
    __u32 src_v4_be = 0;
    __u32 dst_v4_be = 0;
    __u32 rule_action = RULE_ACTION_ALLOW;
    __u32 rate_override = 0;
    struct conntrack_5tuple_key ct_key = {};

    if (h_proto == ETH_P_IP) {
        struct iphdr *iph = cursor;
        if ((void *)(iph + 1) > data_end) {
            bump_reason(REASON_TRUNCATED_IPV4);
            return XDP_DROP;
        }
        __u8 ver = iph->ihl_version >> 4;
        __u8 ihl = iph->ihl_version & 0x0F;
        if (ver != 4 || ihl < 5) {
            bump_reason(REASON_INVALID_IPV4_IHL_OR_VER);
            return XDP_DROP;
        }
        __u32 ihl_bytes = (__u32)ihl * 4U;
        if (cursor + ihl_bytes > data_end) {
            bump_reason(REASON_TRUNCATED_IPV4);
            return XDP_DROP;
        }
        __u16 tot_len = bpf_ntohs(iph->tot_len);
        if (tot_len < ihl_bytes) {
            bump_reason(REASON_INVALID_IPV4_TOT_LEN);
            return XDP_DROP;
        }
        /* Reject malformed / tiny / overlapping IPv4 fragments */
        __u16 frag = bpf_ntohs(iph->frag_off);
        __u16 frag_offset_bytes = (frag & 0x1FFFU) * 8U;
        __u8 more_frags = (frag & 0x2000U) != 0;
        if (frag_offset_bytes > 0 || (more_frags && (tot_len - ihl_bytes) < 8U) ||
            ((__u32)frag_offset_bytes + (__u32)(tot_len - ihl_bytes) > 65535U)) {
            bump_reason(REASON_MALFORMED_IPV4_FRAGMENT);
            return XDP_DROP;
        }

        src_v4_be = iph->saddr;
        dst_v4_be = iph->daddr;
        l4_proto = iph->protocol;
        __builtin_memcpy(&ct_key.src_ip[12], &src_v4_be, 4);
        __builtin_memcpy(&ct_key.dst_ip[12], &dst_v4_be, 4);
        ct_key.proto = l4_proto;

        struct ipv4_lpm_key lpm_k = { .prefixlen = 32, .addr_be = src_v4_be };
        struct block_rule_val *rule = bpf_map_lookup_elem(&ipv4_blocklist_lpm, &lpm_k);
        if (rule && (rule->expires_ts_ns == 0 || now_ns < rule->expires_ts_ns)) {
            rule_action = rule->action;
            rate_override = rule->rate_override;
            if (rule_action == RULE_ACTION_HARD_DROP) {
                bump_reason(REASON_IPV4_BLOCKLIST_MATCH);
                return XDP_DROP;
            }
        }
        cursor = cursor + ihl_bytes;
    } else if (h_proto == ETH_P_IPV6) {
        struct ipv6hdr *ip6 = cursor;
        if ((void *)(ip6 + 1) > data_end) {
            bump_reason(REASON_TRUNCATED_IPV6);
            return XDP_DROP;
        }
        if ((bpf_ntohl(ip6->ver_tc_flow) >> 28) != 6U) {
            bump_reason(REASON_INVALID_IPV6_VER_OR_LEN);
            return XDP_DROP;
        }
        l4_proto = ip6->nexthdr;
        __builtin_memcpy(ct_key.src_ip, ip6->saddr, 16);
        __builtin_memcpy(ct_key.dst_ip, ip6->daddr, 16);
        ct_key.proto = l4_proto;

        struct ipv6_lpm_key lpm6 = { .prefixlen = 128 };
        __builtin_memcpy(lpm6.addr, ip6->saddr, 16);
        struct block_rule_val *rule6 = bpf_map_lookup_elem(&ipv6_blocklist_lpm, &lpm6);
        if (rule6 && (rule6->expires_ts_ns == 0 || now_ns < rule6->expires_ts_ns)) {
            rule_action = rule6->action;
            rate_override = rule6->rate_override;
            if (rule_action == RULE_ACTION_HARD_DROP) {
                bump_reason(REASON_IPV6_BLOCKLIST_MATCH);
                return XDP_DROP;
            }
        }
        cursor = (void *)(ip6 + 1);
    } else {
        bump_reason(REASON_NON_IP_PASS);
        return XDP_PASS;
    }

    if (l4_proto == IPPROTO_TCP) {
        struct tcphdr *tcph = cursor;
        if ((void *)(tcph + 1) > data_end) {
            bump_reason(REASON_TRUNCATED_TCP);
            return XDP_DROP;
        }
        __u8 doff = tcph->doff_res >> 4;
        if (doff < 5) {
            bump_reason(REASON_INVALID_TCP_DOFF);
            return XDP_DROP;
        }
        __u32 tcp_hdr_bytes = (__u32)doff * 4U;
        if (cursor + tcp_hdr_bytes > data_end) {
            bump_reason(REASON_TRUNCATED_TCP);
            return XDP_DROP;
        }

        __u8 flags = tcph->flags;
        __u8 syn = (flags & 0x02) != 0;
        __u8 ack = (flags & 0x10) != 0;
        __u8 fin = (flags & 0x01) != 0;
        __u8 rst = (flags & 0x04) != 0;
        __u8 urg = (flags & 0x20) != 0;
        __u8 psh = (flags & 0x08) != 0;

        /* Reject Null scan, Xmas scan (FIN+URG+PSH), SYN+FIN, SYN+RST */
        if (flags == 0 || (syn && fin) || (syn && rst) || (fin && urg && psh && !ack)) {
            bump_reason(REASON_INVALID_TCP_FLAGS);
            return XDP_DROP;
        }
        if (src_v4_be != 0 && src_v4_be == dst_v4_be && tcph->source == tcph->dest) {
            bump_reason(REASON_LAND_ATTACK);
            return XDP_DROP;
        }

        ct_key.src_port = tcph->source;
        ct_key.dst_port = tcph->dest;

        /* Graceful Stateful Conntrack Preservation: active established sessions
         * are never severed by subnet-level SYN quarantine updates. */
        if (!syn && ack) {
            struct conntrack_val *ct = bpf_map_lookup_elem(&established_sessions, &ct_key);
            if (ct && ct->state == 1) {
                ct->last_seen_ts_ns = now_ns;
                if (rule_action == RULE_ACTION_DROP_NEW_SYN_ONLY) {
                    bump_reason(REASON_ESTABLISHED_CONNTRACK_PASS);
                    return XDP_PASS;
                }
            }
        } else if (syn && !ack && rule_action == RULE_ACTION_DROP_NEW_SYN_ONLY) {
            bump_reason(REASON_IPV4_BLOCKLIST_MATCH);
            return XDP_DROP;
        }

        /* Token-Bucket Rate Limiter per Source IPv4 */
        if (src_v4_be != 0 && (syn || rule_action == RULE_ACTION_THROTTLE_RPS)) {
            struct token_bucket_val *tb = bpf_map_lookup_elem(&ipv4_token_buckets, &src_v4_be);
            __u64 cost_scaled = syn ? 5000000ULL : 1000000ULL;
            if (!tb) {
                struct token_bucket_val init_tb = {
                    .tokens_scaled = 50000000ULL,
                    .last_refill_ns = now_ns,
                    .rate_per_sec = rate_override ? rate_override : 500ULL,
                    .burst_capacity = 50ULL,
                    .dropped_packets = 0ULL,
                };
                bpf_map_update_elem(&ipv4_token_buckets, &src_v4_be, &init_tb, BPF_ANY);
            } else {
                __u64 elapsed_ns = now_ns > tb->last_refill_ns ? (now_ns - tb->last_refill_ns) : 0ULL;
                __u64 rate = rate_override ? (__u64)rate_override : tb->rate_per_sec;
                __u64 add_scaled = (elapsed_ns * rate) / 1000ULL; /* rate * 1e6 / 1e9 */
                __u64 cap_scaled = tb->burst_capacity * 1000000ULL;
                __u64 avail = tb->tokens_scaled + add_scaled;
                if (avail > cap_scaled)
                    avail = cap_scaled;
                tb->last_refill_ns = now_ns;
                if (avail < cost_scaled) {
                    tb->tokens_scaled = avail;
                    tb->dropped_packets += 1;
                    bump_reason(REASON_SYN_FLOOD_RATE_LIMITED);
                    return XDP_DROP;
                }
                tb->tokens_scaled = avail - cost_scaled;
            }
        }

        /* Inspect HYPERION Binary Wire Protocol Header on Gateway Port 8080 */
        __u16 dport = bpf_ntohs(tcph->dest);
        const __u8 *l7 = (const __u8 *)cursor + tcp_hdr_bytes;
        if (dport == HYPERION_GATEWAY_PORT_HTTP_WS && (const void *)l7 < data_end) {
            int l7_err = check_hyperion_l7_payload(l7, data_end);
            if (l7_err != REASON_NONE) {
                bump_reason((__u32)l7_err);
                return XDP_DROP;
            }
        }

        /* Record ingress RTT sample if matching egress timestamp exists */
        struct conntrack_5tuple_key rev_key = {};
        __builtin_memcpy(rev_key.src_ip, ct_key.dst_ip, 16);
        __builtin_memcpy(rev_key.dst_ip, ct_key.src_ip, 16);
        rev_key.src_port = ct_key.dst_port;
        rev_key.dst_port = ct_key.src_port;
        rev_key.proto = IPPROTO_TCP;
        __u64 *tx_ts = bpf_map_lookup_elem(&flow_tx_tstamp_ns, &rev_key);
        if (tx_ts && now_ns >= *tx_ts) {
            __u32 bin = hdr_bin_index(now_ns - *tx_ts);
            __u64 *hist_slot = bpf_map_lookup_elem(&tcp_rtt_hdr_hist_percpu, &bin);
            if (hist_slot)
                *hist_slot += 1;
        }

        bump_reason(REASON_NONE);
        return XDP_PASS;
    } else if (l4_proto == IPPROTO_UDP) {
        struct udphdr *udph = cursor;
        if ((void *)(udph + 1) > data_end) {
            bump_reason(REASON_TRUNCATED_UDP);
            return XDP_DROP;
        }
        __u16 ulen = bpf_ntohs(udph->len);
        if (ulen < sizeof(struct udphdr)) {
            bump_reason(REASON_INVALID_UDP_LEN);
            return XDP_DROP;
        }

        /* Wire-Speed Hairpin Health Probe Responder (`XDP_TX`) */
        if (h_proto == ETH_P_IP && bpf_ntohs(udph->dest) == HYPERION_XDP_PROBE_UDP_PORT) {
            __u32 *probe = (__u32 *)(udph + 1);
            if ((void *)(probe + 2) <= data_end &&
                probe[0] == HYPERION_XDP_PROBE_MAGIC_HI &&
                probe[1] == HYPERION_XDP_PROBE_MAGIC_LO) {
                struct iphdr *iph = (struct iphdr *)((void *)(eth + 1));
                if ((void *)(iph + 1) <= data_end) {
                    swap_eth_mac(eth);
                    __u32 tmp_ip = iph->saddr;
                    iph->saddr = iph->daddr;
                    iph->daddr = tmp_ip;
                    __u16 tmp_port = udph->source;
                    udph->source = udph->dest;
                    udph->dest = tmp_port;
                    udph->check = 0; /* Valid optional UDPv4 checksum = 0 */
                    bump_reason(REASON_XDP_TX_PROBE_REPLY);
                    return XDP_TX;
                }
            }
        }

        bump_reason(REASON_NONE);
        return XDP_PASS;
    }

    bump_reason(REASON_NONE);
    return XDP_PASS;
}

SEC("tc")
int hyperion_tc_egress_rtt(struct __sk_buff *skb)
{
    void *data = (void *)(long)skb->data;
    void *data_end = (void *)(long)skb->data_end;

    struct ethhdr *eth = data;
    if ((void *)(eth + 1) > data_end)
        return TC_ACT_OK;

    if (bpf_ntohs(eth->h_proto) != ETH_P_IP)
        return TC_ACT_OK;

    struct iphdr *iph = (void *)(eth + 1);
    if ((void *)(iph + 1) > data_end || iph->protocol != IPPROTO_TCP)
        return TC_ACT_OK;

    __u32 ihl_bytes = (__u32)(iph->ihl_version & 0x0F) * 4U;
    if (ihl_bytes < 20U || (void *)iph + ihl_bytes > data_end)
        return TC_ACT_OK;

    struct tcphdr *tcph = (void *)iph + ihl_bytes;
    if ((void *)(tcph + 1) > data_end)
        return TC_ACT_OK;

    struct conntrack_5tuple_key key = {};
    __builtin_memcpy(&key.src_ip[12], &iph->saddr, 4);
    __builtin_memcpy(&key.dst_ip[12], &iph->daddr, 4);
    key.src_port = tcph->source;
    key.dst_port = tcph->dest;
    key.proto = IPPROTO_TCP;

    __u64 now_ns = bpf_ktime_get_ns();
    bpf_map_update_elem(&flow_tx_tstamp_ns, &key, &now_ns, BPF_ANY);
    return TC_ACT_OK;
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
