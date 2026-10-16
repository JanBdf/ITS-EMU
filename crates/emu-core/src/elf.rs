//! Loader for 32-bit little-endian ARM ELF images (`.axf` from Arm Compiler 6,
//! `.elf` from GCC/LLVM).

use object::elf;
use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};
use object::{Endianness, Object, ObjectSymbol};

#[derive(Debug, thiserror::Error)]
pub enum ElfError {
    #[error("not a valid 32-bit ELF file: {0}")]
    Parse(#[from] object::Error),
    #[error("ELF file is not for ARM (e_machine = {0})")]
    NotArm(u16),
    #[error("program header points outside of the file")]
    BadSegment,
}

/// A contiguous block of bytes to be placed at its load (physical) address.
#[derive(Debug, Clone)]
pub struct Segment {
    pub addr: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub addr: u32,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct ElfImage {
    pub entry: u32,
    pub segments: Vec<Segment>,
    pub symbols: Vec<Symbol>,
}

impl ElfImage {
    pub fn parse(data: &[u8]) -> Result<Self, ElfError> {
        let file = ElfFile32::<Endianness>::parse(data)?;
        let endian = file.endian();
        let header = file.elf_header();
        let machine = header.e_machine(endian);
        if machine != elf::EM_ARM {
            return Err(ElfError::NotArm(machine.0));
        }

        let mut segments = Vec::new();
        for ph in file.elf_program_headers() {
            if ph.p_type(endian) != elf::PT_LOAD || ph.p_filesz(endian) == 0 {
                continue;
            }
            // Use the physical (load) address: initialised RW data is stored in
            // flash and copied to RAM by the C runtime at startup.
            let bytes = ph.data(endian, data).map_err(|()| ElfError::BadSegment)?;
            segments.push(Segment { addr: ph.p_paddr(endian), data: bytes.to_vec() });
        }

        let mut symbols = Vec::new();
        for sym in file.symbols() {
            let Ok(name) = sym.name() else { continue };
            if name.is_empty() || name.starts_with('$') {
                continue;
            }
            let raw = sym.elf_symbol();
            let ty = raw.st_type();
            if ty != elf::STT_FUNC && ty != elf::STT_OBJECT && ty != elf::STT_NOTYPE {
                continue;
            }
            symbols.push(Symbol { name: name.to_owned(), addr: sym.address() as u32, size: sym.size() as u32 });
        }

        Ok(Self { entry: header.e_entry(endian), segments, symbols })
    }

    pub fn symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name == name)
    }

    /// Best matching function/object symbol containing `addr` (Thumb bit ignored).
    pub fn symbolize(&self, addr: u32) -> Option<(&Symbol, u32)> {
        let addr = addr & !1;
        self.symbols
            .iter()
            .filter(|s| {
                let start = s.addr & !1;
                addr >= start && (addr < start + s.size.max(1))
            })
            .min_by_key(|s| s.size)
            .map(|s| (s, addr - (s.addr & !1)))
    }
}
