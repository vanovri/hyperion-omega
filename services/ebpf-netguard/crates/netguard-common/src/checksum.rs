//! Exact RFC 1071 One's Complement & RFC 1624 Incremental Checksum Engine (`#![no_std]`).

/// Accumulates 16-bit big-endian words from `bytes` into a 32-bit carry accumulator.
#[inline]
pub fn sum_be16_words(mut acc: u32, bytes: &[u8]) -> u32 {
    let mut i = 0;
    let len = bytes.len();
    while i + 1 < len {
        let word = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as u32;
        acc = acc.wrapping_add(word);
        i += 2;
    }
    if i < len {
        let odd_word = u16::from_be_bytes([bytes[i], 0]) as u32;
        acc = acc.wrapping_add(odd_word);
    }
    acc
}

/// Folds a 32-bit one's complement sum down to 16 bits and returns `!sum` (RFC 1071).
#[inline]
pub const fn finalize_rfc1071(mut acc: u32) -> u16 {
    while (acc >> 16) != 0 {
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    !(acc as u16)
}

/// Computes standalone RFC 1071 checksum over `data`.
#[inline]
pub fn rfc1071_checksum(data: &[u8]) -> u16 {
    finalize_rfc1071(sum_be16_words(0, data))
}

/// Computes IPv4 header checksum ignoring bytes `10..12` (existing checksum field).
#[inline]
pub fn ipv4_header_checksum(iphdr: &[u8]) -> u16 {
    if iphdr.len() < 20 {
        return 0;
    }
    let mut acc = 0_u32;
    acc = sum_be16_words(acc, &iphdr[..10]);
    acc = sum_be16_words(acc, &iphdr[12..]);
    finalize_rfc1071(acc)
}

/// Verifies an IPv4 header slice (`ihl_bytes >= 20`) has a valid RFC 1071 checksum (`fold == 0`).
#[inline(always)]
pub fn verify_ipv4_header_checksum(iphdr: &[u8]) -> bool {
    let len = iphdr.len();
    if len == 20 {
        // Branchless unrolled 20-byte standard IPv4 header fast-path (99.9% of Internet traffic)
        let acc = (u16::from_be_bytes([iphdr[0], iphdr[1]]) as u32)
            + (u16::from_be_bytes([iphdr[2], iphdr[3]]) as u32)
            + (u16::from_be_bytes([iphdr[4], iphdr[5]]) as u32)
            + (u16::from_be_bytes([iphdr[6], iphdr[7]]) as u32)
            + (u16::from_be_bytes([iphdr[8], iphdr[9]]) as u32)
            + (u16::from_be_bytes([iphdr[10], iphdr[11]]) as u32)
            + (u16::from_be_bytes([iphdr[12], iphdr[13]]) as u32)
            + (u16::from_be_bytes([iphdr[14], iphdr[15]]) as u32)
            + (u16::from_be_bytes([iphdr[16], iphdr[17]]) as u32)
            + (u16::from_be_bytes([iphdr[18], iphdr[19]]) as u32);
        return finalize_rfc1071(acc) == 0;
    }
    if len < 20 || (len & 1) != 0 {
        return false;
    }
    finalize_rfc1071(sum_be16_words(0, iphdr)) == 0
}

/// Computes TCP/UDP over IPv4 checksum including 12-byte RFC 793 / RFC 768 pseudo-header.
/// For UDP, RFC 768 mandates returning `0xFFFF` if the computed checksum is `0x0000`.
#[inline]
pub fn l4_ipv4_checksum(
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    proto: u8,
    l4_segment: &[u8],
    checksum_offset: usize,
) -> u16 {
    let mut acc = 0_u32;
    acc = sum_be16_words(acc, &src_ip);
    acc = sum_be16_words(acc, &dst_ip);
    acc = acc.wrapping_add(proto as u32);
    acc = acc.wrapping_add(l4_segment.len() as u32);

    if checksum_offset + 2 <= l4_segment.len() {
        acc = sum_be16_words(acc, &l4_segment[..checksum_offset]);
        acc = sum_be16_words(acc, &l4_segment[checksum_offset + 2..]);
    } else {
        acc = sum_be16_words(acc, l4_segment);
    }

    let csum = finalize_rfc1071(acc);
    if proto == 17 && csum == 0 {
        0xFFFF
    } else {
        csum
    }
}

/// Verifies TCP or UDP checksum over IPv4. UDP checksum `0x0000` is optional per RFC 768.
#[inline]
pub fn verify_l4_ipv4_checksum(
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    proto: u8,
    l4_segment: &[u8],
) -> bool {
    if proto == 17 {
        if l4_segment.len() < 8 {
            return false;
        }
        let wire_csum = u16::from_be_bytes([l4_segment[6], l4_segment[7]]);
        if wire_csum == 0 {
            return true;
        }
        return l4_ipv4_checksum(src_ip, dst_ip, 17, l4_segment, 6) == wire_csum;
    }
    if proto == 6 {
        if l4_segment.len() < 20 {
            return false;
        }
        let wire_csum = u16::from_be_bytes([l4_segment[16], l4_segment[17]]);
        return l4_ipv4_checksum(src_ip, dst_ip, 6, l4_segment, 16) == wire_csum;
    }
    true
}

/// Computes TCP/UDP/ICMPv6 over IPv6 checksum including 40-byte RFC 8200 pseudo-header.
#[inline]
pub fn l4_ipv6_checksum(
    src_ip: [u8; 16],
    dst_ip: [u8; 16],
    next_hdr: u8,
    l4_segment: &[u8],
    checksum_offset: usize,
) -> u16 {
    let mut acc = 0_u32;
    acc = sum_be16_words(acc, &src_ip);
    acc = sum_be16_words(acc, &dst_ip);
    let len_be = (l4_segment.len() as u32).to_be_bytes();
    acc = sum_be16_words(acc, &len_be);
    acc = acc.wrapping_add(next_hdr as u32);

    if checksum_offset + 2 <= l4_segment.len() {
        acc = sum_be16_words(acc, &l4_segment[..checksum_offset]);
        acc = sum_be16_words(acc, &l4_segment[checksum_offset + 2..]);
    } else {
        acc = sum_be16_words(acc, l4_segment);
    }

    let csum = finalize_rfc1071(acc);
    if next_hdr == 17 && csum == 0 {
        0xFFFF
    } else {
        csum
    }
}

/// RFC 1624 incremental checksum update `HC' = ~(~HC + ~m + m')` for zero-copy `XDP_TX` rewrites.
#[inline]
pub const fn rfc1624_update_u16(old_check: u16, old_word: u16, new_word: u16) -> u16 {
    let mut sum = (!old_check as u32)
        .wrapping_add(!old_word as u32)
        .wrapping_add(new_word as u32);
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}
