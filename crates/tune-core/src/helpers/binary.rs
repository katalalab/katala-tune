//! Structural checks before replacing a sampler. These do not execute the input
//! or establish its publisher, sampler protocol, or target CPU compatibility.

pub(super) fn windows_executable(bytes: &[u8]) -> bool {
    pe(bytes).is_some()
}

pub(super) fn mac_executable(bytes: &[u8]) -> bool {
    macho64(bytes).is_some() || fat(bytes).is_some()
}

fn read<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u16le(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(read(bytes, at)?))
}

fn u32at(bytes: &[u8], at: usize, big: bool) -> Option<u32> {
    let b = read(bytes, at)?;
    Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

fn u64at(bytes: &[u8], at: usize, big: bool) -> Option<u64> {
    let b = read(bytes, at)?;
    Some(if big { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) })
}

fn range(bytes: &[u8], offset: u64, size: u64) -> Option<&[u8]> {
    bytes.get(usize::try_from(offset).ok()?..usize::try_from(offset.checked_add(size)?).ok()?)
}

// PE/COFF headers, optional header and section table, with an entry point in
// executable file-backed data. A DOS stub or a DLL is not a sampler executable.
fn pe(bytes: &[u8]) -> Option<()> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let offset = u32at(bytes, 60, false)? as u64;
    let header = range(bytes, offset, 24)?;
    if offset < 64 || header.get(..4)? != b"PE\0\0" {
        return None;
    }
    let machine = u16le(header, 4)?;
    let count = u16le(header, 6)? as u64;
    let flags = u16le(header, 22)?;
    let optional_size = u16le(header, 20)? as u64;
    let optional = range(bytes, offset + 24, optional_size)?;
    let fixed = match (machine, u16le(optional, 0)?) {
        (0x14c, 0x10b) => 96u64,
        (0x8664 | 0xaa64, 0x20b) => 112u64,
        _ => return None,
    };
    if count == 0 || count > 96 || flags & 2 == 0 || flags & 0x2000 != 0 || optional_size < fixed || !matches!(u16le(optional, 68)?, 2 | 3) {
        return None;
    }
    let directories = u32at(optional, fixed as usize - 4, false)? as u64;
    if fixed + directories * 8 > optional_size {
        return None;
    }
    let table_offset = offset + 24 + optional_size;
    let table = range(bytes, table_offset, count * 40)?;
    let headers = u32at(optional, 60, false)? as u64;
    range(bytes, 0, headers)?;
    if headers < table_offset + count * 40 {
        return None;
    }
    let entry = u32at(optional, 16, false)?;
    let mut executable_entry = false;
    for section in table.as_chunks::<40>().0 {
        let size = u32at(section, 16, false)? as u64;
        let raw = u32at(section, 20, false)? as u64;
        if size > 0 {
            range(bytes, raw, size)?;
            if raw < headers {
                return None;
            }
        }
        let executable = u32at(section, 36, false)? & 0x20000000 != 0;
        executable_entry |= executable && entry > 0 && entry.checked_sub(u32at(section, 12, false)?).is_some_and(|delta| (delta as u64) < size);
    }
    executable_entry.then_some(())
}

// Modern macOS samplers are little-endian 64-bit MH_EXECUTE images. Bound every
// command and segment before examining entry-point or section references.
fn macho64(bytes: &[u8]) -> Option<(u32, u32)> {
    let header = bytes.get(..32)?;
    let cpu = u32at(header, 4, false)?;
    if u32at(header, 0, false)? != 0xfeedfacf || !matches!(cpu, 0x01000007 | 0x0100000c) || u32at(header, 12, false)? != 2 {
        return None;
    }
    let commands = range(bytes, 32, u32at(header, 20, false)? as u64)?;
    let count = u32at(header, 16, false)? as usize;
    if count == 0 || count > commands.len() / 8 {
        return None;
    }
    let (mut at, mut entry, mut thread_entry) = (0usize, None, false);
    let mut executable = Vec::new();
    for _ in 0..count {
        let remaining = commands.get(at..)?;
        let size = u32at(remaining, 4, false)? as usize;
        if size < 8 || !size.is_multiple_of(8) {
            return None;
        }
        let command = remaining.get(..size)?;
        match u32at(command, 0, false)? {
            0x19 => {
                let nsects = u32at(command, 64, false)? as u64;
                if size as u64 != 72 + nsects * 80 {
                    return None;
                }
                let offset = u64at(command, 40, false)?;
                let length = u64at(command, 48, false)?;
                range(bytes, offset, length)?;
                if length > u64at(command, 32, false)? {
                    return None;
                }
                if u32at(command, 60, false)? & 4 != 0 && length > 0 {
                    executable.push((offset, offset.checked_add(length)?));
                }
                for section in command.get(72..)?.as_chunks::<80>().0 {
                    if !matches!(u32at(section, 64, false)? & 0xff, 1 | 0xc | 0x12) {
                        let raw = u32at(section, 48, false)? as u64;
                        let len = u64at(section, 40, false)?;
                        range(bytes, raw, len)?;
                        if raw < offset || raw.checked_add(len)? > offset.checked_add(length)? {
                            return None;
                        }
                    }
                }
            }
            0x80000028 => {
                if size != 24 || entry.is_some() {
                    return None;
                }
                entry = Some(u64at(command, 8, false)?);
            }
            5 => {
                if size < 16 || thread_entry {
                    return None;
                }
                let mut state_at = 8usize;
                while state_at < command.len() {
                    let words = u32at(command, state_at + 4, false)? as u64;
                    if words == 0 {
                        return None;
                    }
                    let state = range(command, (state_at + 8) as u64, words * 4)?;
                    state_at = state_at.checked_add(8 + state.len())?;
                }
                thread_entry = true;
            }
            _ => {}
        }
        at = at.checked_add(size)?;
    }
    if at != commands.len()
        || executable.is_empty()
        || !entry.map_or(thread_entry, |e| e >= 32 + commands.len() as u64 && executable.iter().any(|(start, end)| e >= *start && e < *end))
    {
        return None;
    }
    Some((cpu, u32at(header, 8, false)?))
}

fn fat(bytes: &[u8]) -> Option<()> {
    let (big, wide) = match read::<4>(bytes, 0)? {
        [0xca, 0xfe, 0xba, 0xbe] => (true, false),
        [0xca, 0xfe, 0xba, 0xbf] => (true, true),
        [0xbe, 0xba, 0xfe, 0xca] => (false, false),
        [0xbf, 0xba, 0xfe, 0xca] => (false, true),
        _ => return None,
    };
    let count = u32at(bytes, 4, big)? as u64;
    // Samplers have few CPU slices; bound work on an untrusted table as well.
    if count == 0 || count > 64 {
        return None;
    }
    let stride = if wide { 32 } else { 20 };
    let table = range(bytes, 8, count * stride)?;
    let mut spans = Vec::new();
    let mut cpus = Vec::new();
    for arch in table.chunks_exact(stride as usize) {
        let cpu = (u32at(arch, 0, big)?, u32at(arch, 4, big)?);
        let (offset, size, align) = if wide {
            if u32at(arch, 28, big)? != 0 {
                return None;
            }
            (u64at(arch, 8, big)?, u64at(arch, 16, big)?, u32at(arch, 24, big)?)
        } else {
            (u32at(arch, 8, big)? as u64, u32at(arch, 12, big)? as u64, u32at(arch, 16, big)?)
        };
        if offset < 8 + count * stride
            || !offset.is_multiple_of(1u64.checked_shl(align)?)
            || cpus.contains(&cpu)
            || macho64(range(bytes, offset, size)?)? != cpu
        {
            return None;
        }
        cpus.push(cpu);
        spans.push((offset, offset.checked_add(size)?));
    }
    spans.sort_unstable();
    (!spans.windows(2).any(|pair| pair[0].1 > pair[1].0)).then_some(())
}

#[cfg(test)]
pub(super) mod fixtures {
    pub fn pe() -> Vec<u8> {
        let mut b = vec![0; 1024];
        b[..2].copy_from_slice(b"MZ");
        b[60..64].copy_from_slice(&128u32.to_le_bytes());
        b[128..132].copy_from_slice(b"PE\0\0");
        b[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
        b[134..136].copy_from_slice(&1u16.to_le_bytes());
        b[148..150].copy_from_slice(&240u16.to_le_bytes());
        b[150..152].copy_from_slice(&0x22u16.to_le_bytes());
        b[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
        b[168..172].copy_from_slice(&4096u32.to_le_bytes()); // entry RVA
        b[184..188].copy_from_slice(&4096u32.to_le_bytes());
        b[188..192].copy_from_slice(&512u32.to_le_bytes());
        b[208..212].copy_from_slice(&8192u32.to_le_bytes());
        b[212..216].copy_from_slice(&512u32.to_le_bytes()); // headers
        b[220..222].copy_from_slice(&3u16.to_le_bytes()); // console
        b[260..264].copy_from_slice(&16u32.to_le_bytes()); // directories
        b[392..397].copy_from_slice(b".text");
        b[400..404].copy_from_slice(&512u32.to_le_bytes());
        b[404..408].copy_from_slice(&4096u32.to_le_bytes());
        b[408..412].copy_from_slice(&512u32.to_le_bytes());
        b[412..416].copy_from_slice(&512u32.to_le_bytes());
        b[428..432].copy_from_slice(&0x60000020u32.to_le_bytes());
        b[512] = 0xc3;
        b
    }

    pub fn macho() -> Vec<u8> {
        let mut b = vec![0; 132];
        b[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
        b[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        b[12..16].copy_from_slice(&2u32.to_le_bytes()); // MH_EXECUTE
        b[16..20].copy_from_slice(&2u32.to_le_bytes());
        b[20..24].copy_from_slice(&96u32.to_le_bytes());
        b[32..36].copy_from_slice(&0x19u32.to_le_bytes()); // LC_SEGMENT_64
        b[36..40].copy_from_slice(&72u32.to_le_bytes());
        b[40..46].copy_from_slice(b"__TEXT");
        b[56..64].copy_from_slice(&0x100000000u64.to_le_bytes());
        b[64..72].copy_from_slice(&4096u64.to_le_bytes());
        b[80..88].copy_from_slice(&132u64.to_le_bytes()); // file size
        b[88..92].copy_from_slice(&7u32.to_le_bytes());
        b[92..96].copy_from_slice(&5u32.to_le_bytes()); // read / execute
        b[104..108].copy_from_slice(&0x80000028u32.to_le_bytes()); // LC_MAIN
        b[108..112].copy_from_slice(&24u32.to_le_bytes());
        b[112..120].copy_from_slice(&128u64.to_le_bytes());
        b[128..132].copy_from_slice(&0xd65f03c0u32.to_le_bytes()); // ret
        b
    }

    pub fn fat(wide: bool) -> Vec<u8> {
        let arm = macho();
        let mut intel = macho();
        intel[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
        intel[8..12].copy_from_slice(&3u32.to_le_bytes());
        let (stride, first, second, align) = if wide { (32, 128, 384, 7u32) } else { (20, 64, 256, 6u32) };
        let mut b = vec![0; second + intel.len()];
        b[..4].copy_from_slice(&(if wide { 0xcafebabfu32 } else { 0xcafebabeu32 }).to_be_bytes());
        b[4..8].copy_from_slice(&2u32.to_be_bytes());
        for (i, (cpu, sub, off)) in [(0x0100000cu32, 0u32, first), (0x01000007u32, 3u32, second)].into_iter().enumerate() {
            let at = 8 + i * stride;
            b[at..at + 4].copy_from_slice(&cpu.to_be_bytes());
            b[at + 4..at + 8].copy_from_slice(&sub.to_be_bytes());
            if wide {
                b[at + 8..at + 16].copy_from_slice(&(off as u64).to_be_bytes());
                b[at + 16..at + 24].copy_from_slice(&(arm.len() as u64).to_be_bytes());
                b[at + 24..at + 28].copy_from_slice(&align.to_be_bytes());
            } else {
                b[at + 8..at + 12].copy_from_slice(&(off as u32).to_be_bytes());
                b[at + 12..at + 16].copy_from_slice(&(arm.len() as u32).to_be_bytes());
                b[at + 16..at + 20].copy_from_slice(&align.to_be_bytes());
            }
        }
        b[first..first + arm.len()].copy_from_slice(&arm);
        b[second..second + intel.len()].copy_from_slice(&intel);
        b
    }

    pub fn rejected() -> Vec<(&'static str, &'static str, Vec<u8>)> {
        let mut cases = vec![
            ("windows", "prefix only", b"MZ\x90\0rest".to_vec()),
            ("macos", "prefix only", vec![0xcf, 0xfa, 0xed, 0xfe, 7]),
            ("macos", "fat prefix only", vec![0xca, 0xfe, 0xba, 0xbe, 0]),
        ];
        let mut add = |os, label, mut b: Vec<u8>, at: usize, bytes: &[u8]| {
            b[at..at + bytes.len()].copy_from_slice(bytes);
            cases.push((os, label, b));
        };
        add("windows", "PE outside file", pe(), 60, &u32::MAX.to_le_bytes());
        add("windows", "missing signature", pe(), 128, b"PX\0\0");
        add("windows", "COFF object", pe(), 150, &0u16.to_le_bytes());
        add("windows", "DLL", pe(), 150, &0x2002u16.to_le_bytes());
        add("windows", "missing sections", pe(), 134, &0u16.to_le_bytes());
        add("windows", "truncated optional header", pe(), 148, &2u16.to_le_bytes());
        add("windows", "bad optional magic", pe(), 152, &0u16.to_le_bytes());
        add("windows", "directories outside header", pe(), 260, &u32::MAX.to_le_bytes());
        add("windows", "headers outside file", pe(), 212, &u32::MAX.to_le_bytes());
        add("windows", "section outside file", pe(), 412, &u32::MAX.to_le_bytes());
        add("windows", "no executable entry section", pe(), 428, &0x40000040u32.to_le_bytes());
        add("windows", "entry outside image", pe(), 168, &u32::MAX.to_le_bytes());
        add("macos", "object file", macho(), 12, &1u32.to_le_bytes());
        add("macos", "dylib", macho(), 12, &6u32.to_le_bytes());
        add("macos", "no commands", macho(), 16, &0u32.to_le_bytes());
        add("macos", "commands outside file", macho(), 20, &u32::MAX.to_le_bytes());
        add("macos", "short command", macho(), 36, &4u32.to_le_bytes());
        add("macos", "command outside table", macho(), 36, &128u32.to_le_bytes());
        add("macos", "segment outside file", macho(), 80, &u64::MAX.to_le_bytes());
        add("macos", "section table outside command", macho(), 96, &1u32.to_le_bytes());
        add("macos", "no executable segment", macho(), 92, &1u32.to_le_bytes());
        add("macos", "entry outside file", macho(), 112, &u64::MAX.to_le_bytes());
        add("macos", "entry inside load commands", macho(), 112, &0u64.to_le_bytes());
        add("macos", "no entry command", macho(), 104, &0x1234u32.to_le_bytes());
        let mut thread = macho();
        thread[104..108].copy_from_slice(&5u32.to_le_bytes());
        add("macos", "thread state outside command", thread, 116, &u32::MAX.to_le_bytes());
        add("macos", "empty fat table", fat(false), 4, &0u32.to_be_bytes());
        add("macos", "fat table outside file", fat(false), 4, &u32::MAX.to_be_bytes());
        add("macos", "fat slice overlaps header", fat(false), 16, &0u32.to_be_bytes());
        add("macos", "fat slice outside file", fat(false), 20, &u32::MAX.to_be_bytes());
        add("macos", "fat slice has wrong CPU", fat(false), 8, &0x01000007u32.to_be_bytes());
        add("macos", "fat contains object", fat(false), 64 + 12, &1u32.to_le_bytes());
        add("macos", "fat slices overlap", fat(false), 36, &128u32.to_be_bytes());
        add("macos", "fat64 overflow", fat(true), 16, &u64::MAX.to_be_bytes());
        cases
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::*, mac_executable, windows_executable};

    #[test]
    fn accepts_complete_executable_structures() {
        assert!(windows_executable(&pe()));
        assert!(mac_executable(&macho()));
        assert!(mac_executable(&fat(false)));
        assert!(mac_executable(&fat(true)));
        assert!(!mac_executable(&pe()));
        assert!(!windows_executable(&macho()));
    }

    #[test]
    fn accepts_pe32_and_arm64_pe32plus_and_swapped_fat_tables() {
        let mut arm = pe();
        arm[132..134].copy_from_slice(&0xaa64u16.to_le_bytes());
        assert!(windows_executable(&arm));
        let mut x86 = pe();
        x86[132..134].copy_from_slice(&0x14cu16.to_le_bytes());
        x86[148..150].copy_from_slice(&224u16.to_le_bytes());
        x86[152..154].copy_from_slice(&0x10bu16.to_le_bytes());
        x86[244..248].copy_from_slice(&16u32.to_le_bytes());
        x86.copy_within(392..432, 376);
        assert!(windows_executable(&x86));
        for wide in [false, true] {
            let mut b = fat(wide);
            b[..4].reverse();
            b[4..8].reverse();
            let stride = if wide { 32 } else { 20 };
            for i in 0..2 {
                let at = 8 + i * stride;
                for (offset, size) in
                    if wide { vec![(0, 4), (4, 4), (8, 8), (16, 8), (24, 4), (28, 4)] } else { vec![(0, 4), (4, 4), (8, 4), (12, 4), (16, 4)] }
                {
                    b[at + offset..at + offset + size].reverse();
                }
            }
            assert!(mac_executable(&b));
        }
    }

    #[test]
    fn rejects_nonexecutables_and_out_of_bounds_structures() {
        for (os, label, b) in rejected() {
            assert!(!if os == "windows" { windows_executable(&b) } else { mac_executable(&b) }, "{os}: {label}");
        }
    }

    #[test]
    fn rejects_every_truncated_prefix_of_complete_fixtures() {
        for (windows, b) in [(true, pe()), (false, macho()), (false, fat(false)), (false, fat(true))] {
            for length in 0..b.len() {
                assert!(!if windows { windows_executable(&b[..length]) } else { mac_executable(&b[..length]) }, "truncated at {length}/{}", b.len());
            }
        }
    }
}
