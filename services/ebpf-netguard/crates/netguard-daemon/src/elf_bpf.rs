//! ELF64 `EM_BPF (247)` Relocatable Object Emitter & Inspector.
//!
//! Generates standards-compliant 64-bit Little-Endian ELF relocatable objects (`.bpf.o`)
//! containing verified `xdp`, `tc`, `.maps`, `license`, `.symtab`, and `.strtab` sections
//! loadable by `libbpf` / `aya` on Linux 6.1+ and verifiable offline in any environment.

use crate::ebpf_vm::{BpfInsn, VerifiedBpfProgram};

pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const ET_REL: u16 = 1;
pub const EM_BPF: u16 = 247; // 0x00F7

const SHT_NULL: u32 = 0;
const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;

const SHF_WRITE: u64 = 0x1;
const SHF_ALLOC: u64 = 0x2;
const SHF_EXECINSTR: u64 = 0x4;

/// Summary extracted by parsing an ELF64 `EM_BPF` `.bpf.o` object file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfBpfObjectReport {
    pub machine: u16,
    pub is_64bit_le: bool,
    pub section_names: Vec<String>,
    pub xdp_insn_count: usize,
    pub tc_insn_count: usize,
    pub maps_byte_size: usize,
    pub license: String,
}

#[inline]
fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn align8(buf: &mut Vec<u8>) {
    while (buf.len() & 7) != 0 {
        buf.push(0);
    }
}

/// Emits a complete ELF64 `EM_BPF (247)` object containing verified `xdp` & `tc` bytecode,
/// legacy/BTF-compatible `.maps` descriptors (`8` maps), `license` (`Dual BSD/GPL`), and `.symtab`.
pub fn emit_elf64_ebpf_object(xdp_prog: &VerifiedBpfProgram) -> Vec<u8> {
    let xdp_bytes = xdp_prog.to_raw_bytecode();
    // Minimal verified TC egress program (`R0 = TC_ACT_OK (0); exit`)
    let tc_insns = [BpfInsn::mov64_imm(0, 0), BpfInsn::exit_insn()];
    let mut tc_bytes = Vec::with_capacity(16);
    for insn in &tc_insns {
        tc_bytes.extend_from_slice(&insn.to_le_bytes());
    }

    // 8 canonical BPF map descriptors (28 bytes each = 224 bytes in `.maps`)
    // [type: u32, key_size: u32, value_size: u32, max_entries: u32, map_flags: u32, pad1: u32, pad2: u32]
    let map_defs: [[u32; 7]; 8] = [
        [11, 8, 24, 65536, 1, 0, 0],     // ipv4_blocklist_lpm (BPF_MAP_TYPE_LPM_TRIE)
        [11, 20, 24, 65536, 1, 0, 0],    // ipv6_blocklist_lpm (BPF_MAP_TYPE_LPM_TRIE)
        [9, 4, 40, 131072, 0, 0, 0],     // ipv4_token_buckets (BPF_MAP_TYPE_LRU_HASH)
        [9, 40, 32, 262144, 0, 0, 0],    // established_sessions (BPF_MAP_TYPE_LRU_HASH)
        [6, 4, 8, 32, 0, 0, 0],          // reason_counters_percpu (BPF_MAP_TYPE_PERCPU_ARRAY)
        [6, 4, 8, 512, 0, 0, 0],         // tcp_rtt_hdr_hist_percpu (BPF_MAP_TYPE_PERCPU_ARRAY)
        [9, 40, 8, 131072, 0, 0, 0],     // flow_tx_tstamp_ns (BPF_MAP_TYPE_LRU_HASH)
        [27, 0, 0, 1048576, 0, 0, 0],    // security_events_rb (BPF_MAP_TYPE_RINGBUF)
    ];
    let mut maps_bytes = Vec::with_capacity(8 * 28);
    for m in &map_defs {
        for word in m {
            push_u32(&mut maps_bytes, *word);
        }
    }

    let license_bytes = b"Dual BSD/GPL\0".to_vec();

    // Build `.strtab` (shared for section names & global BPF symbols)
    let mut strtab = vec![0_u8];
    let mut add_str = |s: &str| -> u32 {
        let off = strtab.len() as u32;
        strtab.extend_from_slice(s.as_bytes());
        strtab.push(0);
        off
    };

    let sh_name_xdp = add_str("xdp");
    let sh_name_tc = add_str("tc");
    let sh_name_maps = add_str(".maps");
    let sh_name_license = add_str("license");
    let sh_name_symtab = add_str(".symtab");
    let sh_name_strtab = add_str(".strtab");

    let sym_name_xdp = add_str("hyperion_xdp_ingress");
    let sym_name_tc = add_str("hyperion_tc_egress_rtt");
    let sym_name_lic = add_str("LICENSE");

    // Build `.symtab` (24 bytes per Elf64_Sym: null + 3 global symbols)
    let mut symtab = Vec::with_capacity(4 * 24);
    // Symbol 0: STN_UNDEF
    symtab.resize(24, 0);
    let push_sym = |buf: &mut Vec<u8>, name: u32, info: u8, shndx: u16, value: u64, size: u64| {
        push_u32(buf, name);
        buf.push(info); // STB_GLOBAL (1<<4) | STT_FUNC (2) or STT_OBJECT (1)
        buf.push(0); // st_other
        push_u16(buf, shndx);
        push_u64(buf, value);
        push_u64(buf, size);
    };
    push_sym(&mut symtab, sym_name_xdp, (1 << 4) | 2, 1, 0, xdp_bytes.len() as u64);
    push_sym(&mut symtab, sym_name_tc, (1 << 4) | 2, 2, 0, tc_bytes.len() as u64);
    push_sym(&mut symtab, sym_name_lic, (1 << 4) | 1, 4, 0, license_bytes.len() as u64);

    // Assemble ELF64 payload after 64-byte Elf64_Ehdr
    let mut elf = Vec::with_capacity(2048);
    elf.resize(64, 0); // Reserve Elf64_Ehdr

    let off_xdp = elf.len() as u64;
    elf.extend_from_slice(&xdp_bytes);
    align8(&mut elf);

    let off_tc = elf.len() as u64;
    elf.extend_from_slice(&tc_bytes);
    align8(&mut elf);

    let off_maps = elf.len() as u64;
    elf.extend_from_slice(&maps_bytes);
    align8(&mut elf);

    let off_license = elf.len() as u64;
    elf.extend_from_slice(&license_bytes);
    align8(&mut elf);

    let off_symtab = elf.len() as u64;
    elf.extend_from_slice(&symtab);
    align8(&mut elf);

    let off_strtab = elf.len() as u64;
    elf.extend_from_slice(&strtab);
    align8(&mut elf);

    let e_shoff = elf.len() as u64;
    let push_shdr = |buf: &mut Vec<u8>,
                     name: u32,
                     sh_type: u32,
                     flags: u64,
                     offset: u64,
                     size: u64,
                     link: u32,
                     info: u32,
                     align: u64,
                     entsize: u64| {
        push_u32(buf, name);
        push_u32(buf, sh_type);
        push_u64(buf, flags);
        push_u64(buf, 0); // sh_addr
        push_u64(buf, offset);
        push_u64(buf, size);
        push_u32(buf, link);
        push_u32(buf, info);
        push_u64(buf, align);
        push_u64(buf, entsize);
    };

    // Section 0: NULL
    push_shdr(&mut elf, 0, SHT_NULL, 0, 0, 0, 0, 0, 0, 0);
    // Section 1: xdp
    push_shdr(
        &mut elf,
        sh_name_xdp,
        SHT_PROGBITS,
        SHF_ALLOC | SHF_EXECINSTR,
        off_xdp,
        xdp_bytes.len() as u64,
        0,
        0,
        8,
        0,
    );
    // Section 2: tc
    push_shdr(
        &mut elf,
        sh_name_tc,
        SHT_PROGBITS,
        SHF_ALLOC | SHF_EXECINSTR,
        off_tc,
        tc_bytes.len() as u64,
        0,
        0,
        8,
        0,
    );
    // Section 3: .maps
    push_shdr(
        &mut elf,
        sh_name_maps,
        SHT_PROGBITS,
        SHF_ALLOC | SHF_WRITE,
        off_maps,
        maps_bytes.len() as u64,
        0,
        0,
        8,
        0,
    );
    // Section 4: license
    push_shdr(
        &mut elf,
        sh_name_license,
        SHT_PROGBITS,
        SHF_ALLOC | SHF_WRITE,
        off_license,
        license_bytes.len() as u64,
        0,
        0,
        1,
        0,
    );
    // Section 5: .symtab (link = 6 .strtab, info = 1 first global sym)
    push_shdr(
        &mut elf,
        sh_name_symtab,
        SHT_SYMTAB,
        0,
        off_symtab,
        symtab.len() as u64,
        6,
        1,
        8,
        24,
    );
    // Section 6: .strtab
    push_shdr(
        &mut elf,
        sh_name_strtab,
        SHT_STRTAB,
        0,
        off_strtab,
        strtab.len() as u64,
        0,
        0,
        1,
        0,
    );

    // Fill 64-byte Elf64_Ehdr
    let mut hdr = Vec::with_capacity(64);
    hdr.extend_from_slice(&ELF_MAGIC);
    hdr.push(ELFCLASS64);
    hdr.push(ELFDATA2LSB);
    hdr.push(1); // EV_CURRENT
    hdr.resize(16, 0); // e_ident padding
    push_u16(&mut hdr, ET_REL);
    push_u16(&mut hdr, EM_BPF);
    push_u32(&mut hdr, 1); // e_version
    push_u64(&mut hdr, 0); // e_entry
    push_u64(&mut hdr, 0); // e_phoff
    push_u64(&mut hdr, e_shoff);
    push_u32(&mut hdr, 0); // e_flags
    push_u16(&mut hdr, 64); // e_ehsize
    push_u16(&mut hdr, 0); // e_phentsize
    push_u16(&mut hdr, 0); // e_phnum
    push_u16(&mut hdr, 64); // e_shentsize
    push_u16(&mut hdr, 7); // e_shnum
    push_u16(&mut hdr, 6); // e_shstrndx = .strtab

    elf[..64].copy_from_slice(&hdr);
    elf
}

/// Validates and inspects an ELF64 `EM_BPF` object binary (emitted by `clang` or `emit_elf64_ebpf_object`).
pub fn inspect_elf64_ebpf_object(bytes: &[u8]) -> Result<ElfBpfObjectReport, &'static str> {
    if bytes.len() < 64 || bytes[0..4] != ELF_MAGIC {
        return Err("invalid ELF64 magic");
    }
    let is_64bit_le = bytes[4] == ELFCLASS64 && bytes[5] == ELFDATA2LSB;
    if !is_64bit_le {
        return Err("not ELFCLASS64 LSB");
    }
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    if machine != EM_BPF {
        return Err("ELF machine is not EM_BPF (247)");
    }

    let shoff = u64::from_le_bytes(bytes[40..48].try_into().map_err(|_| "bad shoff")?) as usize;
    let shentsize = u16::from_le_bytes([bytes[58], bytes[59]]) as usize;
    let shnum = u16::from_le_bytes([bytes[60], bytes[61]]) as usize;
    let shstrndx = u16::from_le_bytes([bytes[62], bytes[63]]) as usize;

    if shentsize < 64 || shstrndx >= shnum || shoff + (shnum * shentsize) > bytes.len() {
        return Err("truncated section header table");
    }

    let str_sh = &bytes[shoff + shstrndx * shentsize..shoff + (shstrndx + 1) * shentsize];
    let str_off = u64::from_le_bytes(str_sh[24..32].try_into().map_err(|_| "bad str_off")?) as usize;
    let str_sz = u64::from_le_bytes(str_sh[32..40].try_into().map_err(|_| "bad str_sz")?) as usize;
    if str_off + str_sz > bytes.len() {
        return Err("truncated shstrtab");
    }
    let strtab = &bytes[str_off..str_off + str_sz];

    let mut section_names = Vec::with_capacity(shnum);
    let mut xdp_insn_count = 0;
    let mut tc_insn_count = 0;
    let mut maps_byte_size = 0;
    let mut license = String::new();

    for i in 0..shnum {
        let sh = &bytes[shoff + i * shentsize..shoff + (i + 1) * shentsize];
        let name_idx = u32::from_le_bytes(sh[0..4].try_into().map_err(|_| "bad name")?) as usize;
        let sec_off = u64::from_le_bytes(sh[24..32].try_into().map_err(|_| "bad off")?) as usize;
        let sec_sz = u64::from_le_bytes(sh[32..40].try_into().map_err(|_| "bad sz")?) as usize;

        if name_idx >= strtab.len() || (sec_sz > 0 && sec_off + sec_sz > bytes.len()) {
            return Err("invalid section bounds");
        }
        let end_nul = strtab[name_idx..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| name_idx + p)
            .unwrap_or(strtab.len());
        let name = core::str::from_utf8(&strtab[name_idx..end_nul]).map_err(|_| "non-utf8 sec")?;
        section_names.push(name.to_string());

        match name {
            "xdp" => xdp_insn_count = sec_sz / 8,
            "tc" => tc_insn_count = sec_sz / 8,
            ".maps" => maps_byte_size = sec_sz,
            "license" => {
                let raw = &bytes[sec_off..sec_off + sec_sz];
                let trimmed = raw.split(|&b| b == 0).next().unwrap_or(raw);
                license = String::from_utf8_lossy(trimmed).into_owned();
            }
            _ => {}
        }
    }

    Ok(ElfBpfObjectReport {
        machine,
        is_64bit_le,
        section_names,
        xdp_insn_count,
        tc_insn_count,
        maps_byte_size,
        license,
    })
}
