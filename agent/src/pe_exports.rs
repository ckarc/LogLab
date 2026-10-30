// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Reading a Windows DLL's machine type and the names it exports, to check
//! MMSSTV's PTT plugin before the Agent carries it (see `build.rs`): it must
//! be 32-bit, as MMSSTV is, and export the five names MMSSTV looks up.
//!
//! The layout is Microsoft's "PE Format" (Microsoft Learn): the `PE\0\0`
//! signature at the offset held at 0x3C; the COFF header's `Machine`
//! (0x14C = Intel 386); the optional header, whose first data directory is
//! the export table; the section table, to turn addresses into file
//! offsets; and the export directory's `NumberOfNames` (offset 24) and
//! `AddressOfNames` (offset 32), a list of addresses of NUL-ended names.

/// `IMAGE_FILE_MACHINE_I386`: a 32-bit x86 file.
pub const I386: u16 = 0x14C;

/// A DLL's machine type and exported names.
#[derive(Debug, PartialEq, Eq)]
pub struct Exports {
    /// The COFF header's `Machine`.
    pub machine: u16,
    /// The exported names, in the file's order.
    pub names: Vec<String>,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<usize> {
    let value = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
    usize::try_from(value).ok()
}

/// The machine type and exported names of the DLL `bytes`, or `None` if
/// they aren't a PE file this can read.
#[must_use]
pub fn read(bytes: &[u8]) -> Option<Exports> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(bytes, 0x3C)?;
    if bytes.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let machine = u16_at(bytes, coff)?;
    let sections = usize::from(u16_at(bytes, coff + 2)?);
    let optional_size = usize::from(u16_at(bytes, coff + 16)?);
    let optional = coff + 20;
    // The data directories start at 96 in a PE32 optional header, 112 in
    // a PE32+ one.
    let directories = match u16_at(bytes, optional)? {
        0x10B => optional + 96,
        0x20B => optional + 112,
        _ => return None,
    };
    let export_rva = u32_at(bytes, directories)?;
    if export_rva == 0 {
        return Some(Exports {
            machine,
            names: Vec::new(),
        });
    }
    let table = optional + optional_size;
    let offset = |rva: usize| -> Option<usize> {
        (0..sections).find_map(|index| {
            let header = table + index * 40;
            let size = u32_at(bytes, header + 8)?.max(u32_at(bytes, header + 16)?);
            let address = u32_at(bytes, header + 12)?;
            let raw = u32_at(bytes, header + 20)?;
            (address..address.checked_add(size)?)
                .contains(&rva)
                .then(|| raw + (rva - address))
        })
    };
    let directory = offset(export_rva)?;
    let count = u32_at(bytes, directory + 24)?;
    let names_at = offset(u32_at(bytes, directory + 32)?)?;
    let names = (0..count)
        .map(|index| {
            let start = offset(u32_at(bytes, names_at + index * 4)?)?;
            let rest = bytes.get(start..)?;
            let end = rest.iter().position(|&byte| byte == 0)?;
            String::from_utf8(rest[..end].to_vec()).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Exports { machine, names })
}

#[cfg(test)]
mod tests {
    use super::{Exports, I386, read};

    /// A smallest PE32 file: one section at address 0x1000, file offset
    /// 0x200, holding an export directory naming `names`.
    fn dll(machine: u16, names: &[&str]) -> Vec<u8> {
        let mut bytes = vec![0_u8; 0x400];
        let put16 = |bytes: &mut Vec<u8>, at: usize, value: u16| {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        };
        let put32 = |bytes: &mut Vec<u8>, at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        bytes[..2].copy_from_slice(b"MZ");
        put32(&mut bytes, 0x3C, 0x80);
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        put16(&mut bytes, 0x84, machine);
        put16(&mut bytes, 0x86, 1);
        put16(&mut bytes, 0x94, 224);
        put16(&mut bytes, 0x98, 0x10B);
        // Export directory: address 0x1000.
        put32(&mut bytes, 0x98 + 96, 0x1000);
        let section = 0x98 + 224;
        put32(&mut bytes, section + 8, 0x200);
        put32(&mut bytes, section + 12, 0x1000);
        put32(&mut bytes, section + 16, 0x200);
        put32(&mut bytes, section + 20, 0x200);
        // NumberOfNames, AddressOfNames (0x1040); names from 0x1080.
        put32(
            &mut bytes,
            0x200 + 24,
            u32::try_from(names.len()).unwrap_or(0),
        );
        put32(&mut bytes, 0x200 + 32, 0x1040);
        let mut at = 0x80;
        for (index, name) in names.iter().enumerate() {
            put32(
                &mut bytes,
                0x240 + index * 4,
                0x1000 + u32::try_from(at).unwrap_or(0),
            );
            bytes[0x200 + at..0x200 + at + name.len()].copy_from_slice(name.as_bytes());
            at += name.len() + 1;
        }
        bytes
    }

    #[test]
    fn reads_the_machine_and_names() {
        assert_eq!(
            read(&dll(I386, &["extfskOpen", "extfskSetPTT"])),
            Some(Exports {
                machine: I386,
                names: vec!["extfskOpen".to_owned(), "extfskSetPTT".to_owned()],
            })
        );
    }

    #[test]
    fn refuses_what_isnt_a_pe_file() {
        assert_eq!(read(b"not a dll"), None);
        let mut broken = dll(I386, &["a"]);
        broken[0x80] = b'X';
        assert_eq!(read(&broken), None);
        assert_eq!(read(&dll(I386, &["a"])[..0x100]), None);
    }
}
