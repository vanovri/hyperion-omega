//! RFC 9669 64-Bit eBPF Instruction Set Architecture (ISA), Abstract-Interpretation
//! Static Bounds Verifier (`BpfStaticVerifier`), and Deterministic Userspace Packet-VM (`BpfVm`).
//!
//! Enables 100% deterministic verification and execution of eBPF XDP bytecode in rootless
//! containers, CI environments, and non-Linux developer hosts without `CAP_BPF` / `CAP_NET_ADMIN`.

use netguard_common::parser::{ParserConfig, ETH_HDR_LEN, ETH_P_8021AD, ETH_P_8021Q};
use netguard_common::{DropReason, XdpAction};
use netguard_ebpf::{hyperion_xdp_filter_frame, KernelBpfMapCallbacks, XdpVerdict};

pub const BPF_MAX_INSNS: usize = 4096;
pub const BPF_STACK_SIZE: usize = 512;

// Instruction classes (bits 0..=2)
pub const BPF_LDX: u8 = 0x01;
pub const BPF_STX: u8 = 0x03;
pub const BPF_JMP: u8 = 0x05;
pub const BPF_ALU64: u8 = 0x07;

// Memory sizes & modes
pub const BPF_W: u8 = 0x00; // 32-bit
pub const BPF_H: u8 = 0x08; // 16-bit
pub const BPF_B: u8 = 0x10; // 8-bit
pub const BPF_DW: u8 = 0x18; // 64-bit
pub const BPF_MEM: u8 = 0x60;

// Source operand modifier
pub const BPF_K: u8 = 0x00; // 32-bit immediate
pub const BPF_X: u8 = 0x08; // source register

// ALU64 / JMP operations (bits 4..=7)
pub const BPF_ADD: u8 = 0x00;
pub const BPF_SUB: u8 = 0x10;
pub const BPF_AND: u8 = 0x50;
pub const BPF_LSH: u8 = 0x60;
pub const BPF_RSH: u8 = 0x70;
pub const BPF_MOV: u8 = 0xB0;

pub const BPF_JA: u8 = 0x00;
pub const BPF_JEQ: u8 = 0x10;
pub const BPF_JGT: u8 = 0x20;
pub const BPF_JGE: u8 = 0x30;
pub const BPF_JNE: u8 = 0x50;
pub const BPF_CALL: u8 = 0x80;
pub const BPF_EXIT: u8 = 0x90;

// Canonical XDP Helper / Kfunc IDs supported by Userspace BPF VM
pub const BPF_HELPER_KTIME_GET_NS: i32 = 5;
pub const BPF_KFUNC_HYPERION_XDP_PIPELINE: i32 = 101;

/// Canonical 8-byte Linux eBPF instruction (`struct bpf_insn`, Little-Endian).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BpfInsn {
    pub code: u8,
    pub regs: u8,
    pub off: i16,
    pub imm: i32,
}

impl BpfInsn {
    #[inline]
    pub const fn new(code: u8, dst_reg: u8, src_reg: u8, off: i16, imm: i32) -> Self {
        Self {
            code,
            regs: (dst_reg & 0x0F) | ((src_reg & 0x0F) << 4),
            off,
            imm,
        }
    }

    #[inline]
    pub const fn dst_reg(self) -> usize {
        (self.regs & 0x0F) as usize
    }

    #[inline]
    pub const fn src_reg(self) -> usize {
        (self.regs >> 4) as usize
    }

    #[inline]
    pub const fn to_le_bytes(self) -> [u8; 8] {
        let off_b = self.off.to_le_bytes();
        let imm_b = self.imm.to_le_bytes();
        [
            self.code, self.regs, off_b[0], off_b[1], imm_b[0], imm_b[1], imm_b[2], imm_b[3],
        ]
    }

    #[inline]
    pub const fn from_le_bytes(b: [u8; 8]) -> Self {
        Self {
            code: b[0],
            regs: b[1],
            off: i16::from_le_bytes([b[2], b[3]]),
            imm: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        }
    }

    // Ergonomic constructors
    pub const fn mov64_imm(dst: u8, imm: i32) -> Self {
        Self::new(BPF_ALU64 | BPF_MOV | BPF_K, dst, 0, 0, imm)
    }
    pub const fn mov64_reg(dst: u8, src: u8) -> Self {
        Self::new(BPF_ALU64 | BPF_MOV | BPF_X, dst, src, 0, 0)
    }
    pub const fn add64_imm(dst: u8, imm: i32) -> Self {
        Self::new(BPF_ALU64 | BPF_ADD | BPF_K, dst, 0, 0, imm)
    }
    pub const fn rsh64_imm(dst: u8, imm: i32) -> Self {
        Self::new(BPF_ALU64 | BPF_RSH | BPF_K, dst, 0, 0, imm)
    }
    pub const fn lsh64_imm(dst: u8, imm: i32) -> Self {
        Self::new(BPF_ALU64 | BPF_LSH | BPF_K, dst, 0, 0, imm)
    }
    pub const fn and64_imm(dst: u8, imm: i32) -> Self {
        Self::new(BPF_ALU64 | BPF_AND | BPF_K, dst, 0, 0, imm)
    }
    pub const fn ldx_w(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_LDX | BPF_MEM | BPF_W, dst, src, off, 0)
    }
    pub const fn ldx_h(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_LDX | BPF_MEM | BPF_H, dst, src, off, 0)
    }
    pub const fn ldx_b(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_LDX | BPF_MEM | BPF_B, dst, src, off, 0)
    }
    pub const fn ldx_dw(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_LDX | BPF_MEM | BPF_DW, dst, src, off, 0)
    }
    pub const fn stx_dw(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_STX | BPF_MEM | BPF_DW, dst, src, off, 0)
    }
    pub const fn jgt_reg(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_JMP | BPF_JGT | BPF_X, dst, src, off, 0)
    }
    pub const fn jge_reg(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BPF_JMP | BPF_JGE | BPF_X, dst, src, off, 0)
    }
    pub const fn jeq_imm(dst: u8, imm: i32, off: i16) -> Self {
        Self::new(BPF_JMP | BPF_JEQ | BPF_K, dst, 0, off, imm)
    }
    pub const fn jne_imm(dst: u8, imm: i32, off: i16) -> Self {
        Self::new(BPF_JMP | BPF_JNE | BPF_K, dst, 0, off, imm)
    }
    pub const fn ja(off: i16) -> Self {
        Self::new(BPF_JMP | BPF_JA | BPF_K, 0, 0, off, 0)
    }
    pub const fn call_helper(helper_id: i32) -> Self {
        Self::new(BPF_JMP | BPF_CALL | BPF_K, 0, 0, 0, helper_id)
    }
    pub const fn exit_insn() -> Self {
        Self::new(BPF_JMP | BPF_EXIT | BPF_K, 0, 0, 0, 0)
    }
}

/// Abstract register lattice tracked by `BpfStaticVerifier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegType {
    Uninitialized,
    CtxPtr,
    FramePtr,
    PacketData { fixed_off: usize },
    PacketEnd,
    Scalar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifierPathState {
    regs: [RegType; 11],
    verified_pkt_len: usize,
    stack_slot_init: [bool; BPF_STACK_SIZE / 8],
}

impl Default for VerifierPathState {
    fn default() -> Self {
        let mut regs = [RegType::Uninitialized; 11];
        regs[1] = RegType::CtxPtr;
        regs[10] = RegType::FramePtr;
        Self {
            regs,
            verified_pkt_len: 0,
            stack_slot_init: [false; BPF_STACK_SIZE / 8],
        }
    }
}

impl VerifierPathState {
    fn merge_with(&mut self, incoming: &Self) {
        for r in 0..11 {
            if self.regs[r] != incoming.regs[r] {
                self.regs[r] = match (self.regs[r], incoming.regs[r]) {
                    (RegType::Uninitialized, _) | (_, RegType::Uninitialized) => {
                        RegType::Uninitialized
                    }
                    _ => RegType::Scalar,
                };
            }
        }
        if incoming.verified_pkt_len < self.verified_pkt_len {
            self.verified_pkt_len = incoming.verified_pkt_len;
        }
        for s in 0..self.stack_slot_init.len() {
            self.stack_slot_init[s] &= incoming.stack_slot_init[s];
        }
    }
}

/// Static verification failure reasons mirroring Linux `kernel/bpf/verifier.c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierError {
    EmptyOrOversizeProgram { len: usize },
    InvalidRegister { pc: usize, reg: usize },
    WriteToReadOnlyFramePtr { pc: usize },
    UninitializedRegister { pc: usize, reg: usize },
    BackwardOrOutOfBoundsJump { pc: usize, off: i16 },
    InvalidCtxAccess { pc: usize, off: i16, size: usize },
    UnboundedPacketRead {
        pc: usize,
        needed_end: usize,
        verified_len: usize,
    },
    InvalidStackAccess { pc: usize, off: i16, size: usize },
    NegativePacketOffset { pc: usize },
    UnknownHelperCall { pc: usize, helper_id: i32 },
    UnsupportedOpcode { pc: usize, code: u8 },
    FallthroughExit { pc: usize },
    UnreachableInstruction { pc: usize },
}

/// Verified immutable eBPF program guaranteed free of loops, uninitialized reads, and packet over-reads.
#[derive(Debug, Clone)]
pub struct VerifiedBpfProgram {
    insns: Vec<BpfInsn>,
    pub max_verified_pkt_guard: usize,
}

impl VerifiedBpfProgram {
    #[inline]
    pub fn insns(&self) -> &[BpfInsn] {
        &self.insns
    }

    pub fn to_raw_bytecode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.insns.len() * 8);
        for insn in &self.insns {
            out.extend_from_slice(&insn.to_le_bytes());
        }
        out
    }
}

/// Static abstract-interpretation verifier for 64-bit eBPF XDP programs.
pub struct BpfStaticVerifier;

impl BpfStaticVerifier {
    pub fn verify(insns: &[BpfInsn]) -> Result<VerifiedBpfProgram, VerifierError> {
        let len = insns.len();
        if len == 0 || len > BPF_MAX_INSNS {
            return Err(VerifierError::EmptyOrOversizeProgram { len });
        }

        let mut states: Vec<Option<VerifierPathState>> = vec![None; len];
        states[0] = Some(VerifierPathState::default());
        let mut max_guard_seen = 0_usize;

        let propagate = |target_pc: usize,
                         next_state: VerifierPathState,
                         states: &mut [Option<VerifierPathState>]| {
            match &mut states[target_pc] {
                Some(existing) => existing.merge_with(&next_state),
                None => states[target_pc] = Some(next_state),
            }
        };

        for pc in 0..len {
            let Some(cur) = states[pc].clone() else {
                return Err(VerifierError::UnreachableInstruction { pc });
            };
            let insn = insns[pc];
            let dst = insn.dst_reg();
            let src = insn.src_reg();
            if dst > 10 {
                return Err(VerifierError::InvalidRegister { pc, reg: dst });
            }
            if src > 10 {
                return Err(VerifierError::InvalidRegister { pc, reg: src });
            }

            let class = insn.code & 0x07;
            match class {
                BPF_ALU64 => {
                    if dst == 10 {
                        return Err(VerifierError::WriteToReadOnlyFramePtr { pc });
                    }
                    let op = insn.code & 0xF0;
                    let is_reg_src = (insn.code & 0x08) == BPF_X;
                    if is_reg_src && cur.regs[src] == RegType::Uninitialized {
                        return Err(VerifierError::UninitializedRegister { pc, reg: src });
                    }

                    let mut next = cur;
                    match (op, is_reg_src) {
                        (BPF_MOV, false) => {
                            next.regs[dst] = RegType::Scalar;
                        }
                        (BPF_MOV, true) => {
                            next.regs[dst] = next.regs[src];
                        }
                        (BPF_ADD, false) => {
                            if next.regs[dst] == RegType::Uninitialized {
                                return Err(VerifierError::UninitializedRegister { pc, reg: dst });
                            }
                            if let RegType::PacketData { fixed_off } = next.regs[dst] {
                                if insn.imm < 0 {
                                    return Err(VerifierError::NegativePacketOffset { pc });
                                }
                                next.regs[dst] = RegType::PacketData {
                                    fixed_off: fixed_off.saturating_add(insn.imm as usize),
                                };
                            } else {
                                next.regs[dst] = RegType::Scalar;
                            }
                        }
                        (BPF_SUB | BPF_AND | BPF_LSH | BPF_RSH, false)
                        | (BPF_ADD | BPF_SUB | BPF_AND | BPF_LSH | BPF_RSH, true) => {
                            if next.regs[dst] == RegType::Uninitialized {
                                return Err(VerifierError::UninitializedRegister { pc, reg: dst });
                            }
                            next.regs[dst] = RegType::Scalar;
                        }
                        _ => {
                            return Err(VerifierError::UnsupportedOpcode {
                                pc,
                                code: insn.code,
                            });
                        }
                    }
                    if pc + 1 >= len {
                        return Err(VerifierError::FallthroughExit { pc });
                    }
                    propagate(pc + 1, next, &mut states);
                }
                BPF_LDX => {
                    if (insn.code & 0xE0) != BPF_MEM {
                        return Err(VerifierError::UnsupportedOpcode {
                            pc,
                            code: insn.code,
                        });
                    }
                    if dst == 10 {
                        return Err(VerifierError::WriteToReadOnlyFramePtr { pc });
                    }
                    let size = match insn.code & 0x18 {
                        BPF_B => 1,
                        BPF_H => 2,
                        BPF_W => 4,
                        BPF_DW => 8,
                        _ => {
                            return Err(VerifierError::UnsupportedOpcode {
                                pc,
                                code: insn.code,
                            })
                        }
                    };

                    let mut next = cur;
                    match next.regs[src] {
                        RegType::Uninitialized => {
                            return Err(VerifierError::UninitializedRegister { pc, reg: src });
                        }
                        RegType::CtxPtr => {
                            if size != 4 || (insn.off != 0 && insn.off != 4) {
                                return Err(VerifierError::InvalidCtxAccess {
                                    pc,
                                    off: insn.off,
                                    size,
                                });
                            }
                            next.regs[dst] = if insn.off == 0 {
                                RegType::PacketData { fixed_off: 0 }
                            } else {
                                RegType::PacketEnd
                            };
                        }
                        RegType::PacketData { fixed_off } => {
                            if insn.off < 0 {
                                return Err(VerifierError::NegativePacketOffset { pc });
                            }
                            let needed_end = fixed_off
                                .saturating_add(insn.off as usize)
                                .saturating_add(size);
                            if needed_end > next.verified_pkt_len {
                                return Err(VerifierError::UnboundedPacketRead {
                                    pc,
                                    needed_end,
                                    verified_len: next.verified_pkt_len,
                                });
                            }
                            next.regs[dst] = RegType::Scalar;
                        }
                        RegType::FramePtr => {
                            if size != 8 || insn.off >= 0 || insn.off < -(BPF_STACK_SIZE as i16) || (insn.off % 8) != 0 {
                                return Err(VerifierError::InvalidStackAccess {
                                    pc,
                                    off: insn.off,
                                    size,
                                });
                            }
                            let slot = ((-insn.off as usize) / 8) - 1;
                            if !next.stack_slot_init[slot] {
                                return Err(VerifierError::InvalidStackAccess {
                                    pc,
                                    off: insn.off,
                                    size,
                                });
                            }
                            next.regs[dst] = RegType::Scalar;
                        }
                        RegType::PacketEnd | RegType::Scalar => {
                            return Err(VerifierError::UnboundedPacketRead {
                                pc,
                                needed_end: size,
                                verified_len: 0,
                            });
                        }
                    }

                    if pc + 1 >= len {
                        return Err(VerifierError::FallthroughExit { pc });
                    }
                    propagate(pc + 1, next, &mut states);
                }
                BPF_STX => {
                    if (insn.code & 0xE0) != BPF_MEM || (insn.code & 0x18) != BPF_DW {
                        return Err(VerifierError::UnsupportedOpcode {
                            pc,
                            code: insn.code,
                        });
                    }
                    if cur.regs[src] == RegType::Uninitialized {
                        return Err(VerifierError::UninitializedRegister { pc, reg: src });
                    }
                    if cur.regs[dst] != RegType::FramePtr
                        || insn.off >= 0
                        || insn.off < -(BPF_STACK_SIZE as i16)
                        || (insn.off % 8) != 0
                    {
                        return Err(VerifierError::InvalidStackAccess {
                            pc,
                            off: insn.off,
                            size: 8,
                        });
                    }
                    let mut next = cur;
                    let slot = ((-insn.off as usize) / 8) - 1;
                    next.stack_slot_init[slot] = true;
                    if pc + 1 >= len {
                        return Err(VerifierError::FallthroughExit { pc });
                    }
                    propagate(pc + 1, next, &mut states);
                }
                BPF_JMP => {
                    let op = insn.code & 0xF0;
                    let is_reg_src = (insn.code & 0x08) == BPF_X;
                    match op {
                        BPF_EXIT => {
                            if cur.regs[0] != RegType::Scalar {
                                return Err(VerifierError::UninitializedRegister { pc, reg: 0 });
                            }
                        }
                        BPF_CALL => {
                            match insn.imm {
                                BPF_HELPER_KTIME_GET_NS => {}
                                BPF_KFUNC_HYPERION_XDP_PIPELINE => {
                                    if cur.regs[1] != RegType::CtxPtr {
                                        return Err(VerifierError::UninitializedRegister {
                                            pc,
                                            reg: 1,
                                        });
                                    }
                                }
                                _ => {
                                    return Err(VerifierError::UnknownHelperCall {
                                        pc,
                                        helper_id: insn.imm,
                                    });
                                }
                            }
                            let mut next = cur;
                            next.regs[0] = RegType::Scalar;
                            // R1..R5 are caller-saved scratch registers per eBPF ABI
                            for scratch in 1..=5 {
                                next.regs[scratch] = RegType::Uninitialized;
                            }
                            if pc + 1 >= len {
                                return Err(VerifierError::FallthroughExit { pc });
                            }
                            propagate(pc + 1, next, &mut states);
                        }
                        BPF_JA => {
                            if insn.off < 0 || (pc + 1 + insn.off as usize) >= len {
                                return Err(VerifierError::BackwardOrOutOfBoundsJump {
                                    pc,
                                    off: insn.off,
                                });
                            }
                            propagate(pc + 1 + insn.off as usize, cur, &mut states);
                        }
                        BPF_JEQ | BPF_JNE | BPF_JGT | BPF_JGE => {
                            if cur.regs[dst] == RegType::Uninitialized {
                                return Err(VerifierError::UninitializedRegister { pc, reg: dst });
                            }
                            if is_reg_src && cur.regs[src] == RegType::Uninitialized {
                                return Err(VerifierError::UninitializedRegister { pc, reg: src });
                            }
                            if insn.off < 0
                                || (pc + 1) >= len
                                || (pc + 1 + insn.off as usize) >= len
                            {
                                return Err(VerifierError::BackwardOrOutOfBoundsJump {
                                    pc,
                                    off: insn.off,
                                });
                            }

                            let mut fallthrough = cur.clone();
                            let mut taken = cur;

                            // Recognize canonical eBPF packet bounds proofs:
                            // Pattern 1: `if r_pkt_guard > r_data_end goto drop` -> fallthrough proves `r_pkt_guard <= r_data_end`
                            if op == BPF_JGT
                                && is_reg_src
                                && taken.regs[src] == RegType::PacketEnd
                            {
                                if let RegType::PacketData { fixed_off } = fallthrough.regs[dst] {
                                    if fixed_off > fallthrough.verified_pkt_len {
                                        fallthrough.verified_pkt_len = fixed_off;
                                    }
                                    if fixed_off > max_guard_seen {
                                        max_guard_seen = fixed_off;
                                    }
                                }
                            }
                            // Pattern 2: `if r_data_end >= r_pkt_guard goto ok` -> taken proves `r_pkt_guard <= r_data_end`
                            if op == BPF_JGE
                                && is_reg_src
                                && taken.regs[dst] == RegType::PacketEnd
                            {
                                if let RegType::PacketData { fixed_off } = taken.regs[src] {
                                    if fixed_off > taken.verified_pkt_len {
                                        taken.verified_pkt_len = fixed_off;
                                    }
                                    if fixed_off > max_guard_seen {
                                        max_guard_seen = fixed_off;
                                    }
                                }
                            }

                            propagate(pc + 1, fallthrough, &mut states);
                            propagate(pc + 1 + insn.off as usize, taken, &mut states);
                        }
                        _ => {
                            return Err(VerifierError::UnsupportedOpcode {
                                pc,
                                code: insn.code,
                            });
                        }
                    }
                }
                _ => {
                    return Err(VerifierError::UnsupportedOpcode {
                        pc,
                        code: insn.code,
                    });
                }
            }
        }

        Ok(VerifiedBpfProgram {
            insns: insns.to_vec(),
            max_verified_pkt_guard: max_guard_seen,
        })
    }
}

/// Builds the canonical verifier-proven eBPF XDP ingress program for HYPERION NetGuard.
///
/// The bytecode performs explicit `xdp_md` `data`/`data_end` pointer bounds checks for
/// Ethernet II + optional 802.1Q/802.1ad VLAN headers before invoking the `#![no_std]`
/// zero-copy dissector & BPF map pipeline kfunc, guaranteeing 100% parity with native XDP.
pub fn build_canonical_xdp_program() -> VerifiedBpfProgram {
    // Exact PC-indexed verifier-proven XDP instruction sequence:
    let prog = [
        /* 00 */ BpfInsn::mov64_reg(6, 1),                      // R6 = ctx
        /* 01 */ BpfInsn::ldx_w(2, 6, 0),                       // R2 = data
        /* 02 */ BpfInsn::ldx_w(3, 6, 4),                       // R3 = data_end
        /* 03 */ BpfInsn::mov64_reg(4, 2),                      // R4 = data
        /* 04 */ BpfInsn::add64_imm(4, ETH_HDR_LEN as i32),     // R4 = data + 14
        /* 05 */ BpfInsn::jgt_reg(4, 3, 9),                     // if R4 > R3 goto pc 15 (drop_truncated)
        /* 06 */ BpfInsn::ldx_h(5, 2, 12),                      // R5 = *(u16 *)(data + 12) (verified <= 14!)
        /* 07 */ BpfInsn::jeq_imm(5, (ETH_P_8021Q.to_be()) as i32, 1),  // if 802.1Q goto pc 09
        /* 08 */ BpfInsn::jne_imm(5, (ETH_P_8021AD.to_be()) as i32, 3), // if !802.1ad goto pc 12 (invoke_pipeline)
        /* 09 */ BpfInsn::mov64_reg(4, 2),                      // R4 = data
        /* 10 */ BpfInsn::add64_imm(4, 18),                     // R4 = data + 18 (Eth + VLAN)
        /* 11 */ BpfInsn::jgt_reg(4, 3, 3),                     // if R4 > R3 goto pc 15 (drop_truncated)
        /* 12 */ BpfInsn::mov64_reg(1, 6),                      // R1 = ctx
        /* 13 */ BpfInsn::call_helper(BPF_KFUNC_HYPERION_XDP_PIPELINE), // R0 = xdp_action
        /* 14 */ BpfInsn::exit_insn(),                          // return R0
        /* 15 */ BpfInsn::mov64_reg(1, 6),                      // drop_truncated: still account via pipeline
        /* 16 */ BpfInsn::call_helper(BPF_KFUNC_HYPERION_XDP_PIPELINE),
        /* 17 */ BpfInsn::exit_insn(),
    ];

    BpfStaticVerifier::verify(&prog).expect("canonical HYPERION XDP bytecode must pass static verifier")
}

/// Userspace eBPF Virtual Machine executing `VerifiedBpfProgram` deterministically.
pub struct BpfVm;

impl BpfVm {
    /// Executes a verified eBPF program over `frame` using synthetic packet pointers
    /// `[BASE .. BASE + frame.len()]` so real pointer comparisons (`data + N > data_end`)
    /// execute identically to kernel `struct xdp_md`.
    #[inline]
    pub fn execute_xdp<M: KernelBpfMapCallbacks>(
        prog: &VerifiedBpfProgram,
        frame: &mut [u8],
        cfg: &ParserConfig,
        maps: &mut M,
    ) -> XdpVerdict {
        const PKT_SYNTH_BASE: u64 = 0x1000_0000;
        const CTX_SYNTH_PTR: u64 = 0x2000_0000;

        let pkt_len = frame.len();
        let data_start = PKT_SYNTH_BASE;
        let data_end = PKT_SYNTH_BASE + (pkt_len as u64);

        let mut regs = [0_u64; 11];
        regs[1] = CTX_SYNTH_PTR;
        let mut stack = [0_u64; BPF_STACK_SIZE / 8];
        let mut last_verdict: Option<XdpVerdict> = None;

        let insns = prog.insns();
        let mut pc = 0_usize;

        while pc < insns.len() {
            let insn = insns[pc];
            let dst = insn.dst_reg();
            let src = insn.src_reg();
            let class = insn.code & 0x07;

            match class {
                BPF_ALU64 => {
                    let op = insn.code & 0xF0;
                    let rhs = if (insn.code & 0x08) == BPF_X {
                        regs[src]
                    } else {
                        insn.imm as i64 as u64
                    };
                    match op {
                        BPF_MOV => regs[dst] = rhs,
                        BPF_ADD => regs[dst] = regs[dst].wrapping_add(rhs),
                        BPF_SUB => regs[dst] = regs[dst].wrapping_sub(rhs),
                        BPF_AND => regs[dst] &= rhs,
                        BPF_LSH => regs[dst] = regs[dst].wrapping_shl(rhs as u32),
                        BPF_RSH => regs[dst] = regs[dst].wrapping_shr(rhs as u32),
                        _ => unreachable!("verified opcode"),
                    }
                    pc += 1;
                }
                BPF_LDX => {
                    let addr = regs[src].wrapping_add(insn.off as i64 as u64);
                    if regs[src] == CTX_SYNTH_PTR {
                        regs[dst] = if insn.off == 0 { data_start } else { data_end };
                    } else if addr >= data_start && addr < data_end {
                        let off = (addr - data_start) as usize;
                        regs[dst] = match insn.code & 0x18 {
                            BPF_B => frame[off] as u64,
                            BPF_H => u16::from_ne_bytes([frame[off], frame[off + 1]]) as u64,
                            BPF_W => u32::from_ne_bytes([
                                frame[off],
                                frame[off + 1],
                                frame[off + 2],
                                frame[off + 3],
                            ]) as u64,
                            BPF_DW => u64::from_ne_bytes([
                                frame[off],
                                frame[off + 1],
                                frame[off + 2],
                                frame[off + 3],
                                frame[off + 4],
                                frame[off + 5],
                                frame[off + 6],
                                frame[off + 7],
                            ]),
                            _ => 0,
                        };
                    } else {
                        let slot = ((-insn.off as usize) / 8) - 1;
                        regs[dst] = stack[slot];
                    }
                    pc += 1;
                }
                BPF_STX => {
                    let slot = ((-insn.off as usize) / 8) - 1;
                    stack[slot] = regs[src];
                    pc += 1;
                }
                BPF_JMP => {
                    let op = insn.code & 0xF0;
                    match op {
                        BPF_EXIT => {
                            let action = XdpAction::from_u32(regs[0] as u32);
                            if let Some(mut v) = last_verdict {
                                v.action = action;
                                return v;
                            }
                            let reason = if action == XdpAction::Drop {
                                DropReason::TruncatedEth
                            } else {
                                DropReason::None
                            };
                            maps.bump_reason_counter(action, reason);
                            return XdpVerdict {
                                action,
                                reason,
                                parsed: None,
                            };
                        }
                        BPF_CALL => {
                            match insn.imm {
                                BPF_HELPER_KTIME_GET_NS => {
                                    regs[0] = maps.ktime_get_ns();
                                }
                                BPF_KFUNC_HYPERION_XDP_PIPELINE => {
                                    let v = hyperion_xdp_filter_frame(frame, cfg, maps);
                                    regs[0] = v.action as u64;
                                    last_verdict = Some(v);
                                }
                                _ => {
                                    regs[0] = XdpAction::Aborted as u64;
                                }
                            }
                            pc += 1;
                        }
                        BPF_JA => {
                            pc = pc + 1 + (insn.off as usize);
                        }
                        BPF_JEQ | BPF_JNE | BPF_JGT | BPF_JGE => {
                            let rhs = if (insn.code & 0x08) == BPF_X {
                                regs[src]
                            } else {
                                insn.imm as i64 as u64
                            };
                            let cond = match op {
                                BPF_JEQ => regs[dst] == rhs,
                                BPF_JNE => regs[dst] != rhs,
                                BPF_JGT => regs[dst] > rhs,
                                BPF_JGE => regs[dst] >= rhs,
                                _ => false,
                            };
                            if cond {
                                pc = pc + 1 + (insn.off as usize);
                            } else {
                                pc += 1;
                            }
                        }
                        _ => unreachable!("verified jump"),
                    }
                }
                _ => unreachable!("verified class"),
            }
        }

        XdpVerdict {
            action: XdpAction::Aborted,
            reason: DropReason::TruncatedEth,
            parsed: None,
        }
    }
}
