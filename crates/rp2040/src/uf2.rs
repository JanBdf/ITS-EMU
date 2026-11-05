//! UF2 images (the files dropped onto the Pico's boot drive).

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Uf2Error {
    #[error("not a UF2 file (block {0} has no UF2 magic)")]
    Magic(usize),
    #[error("the UF2 file contains no RP2040 blocks")]
    NoRp2040Blocks,
    #[error("UF2 block {block} at {addr:#010x} is outside the Pico's flash")]
    OutOfFlash { block: usize, addr: u32 },
}

pub const RP2040_FAMILY: u32 = 0xE48B_FF56;
const FLAG_NOT_MAIN_FLASH: u32 = 0x1;
const FLAG_FAMILY_ID: u32 = 0x2000;

/// The flash contents a UF2 file programs: (address, bytes) per block.
pub fn parse(data: &[u8], flash_base: u32, flash_size: u32) -> Result<Vec<(u32, Vec<u8>)>, Uf2Error> {
    let word = |b: &[u8], i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    let mut out = Vec::new();
    for (n, block) in data.chunks(512).enumerate() {
        if block.len() < 512
            || word(block, 0) != 0x0A32_4655
            || word(block, 1) != 0x9E5D_5157
            || u32::from_le_bytes(block[508..512].try_into().unwrap()) != 0x0AB1_6F30
        {
            return Err(Uf2Error::Magic(n));
        }
        let (flags, addr, size, family) = (word(block, 2), word(block, 3), word(block, 4), word(block, 7));
        if flags & FLAG_NOT_MAIN_FLASH != 0 || (flags & FLAG_FAMILY_ID != 0 && family != RP2040_FAMILY) {
            continue;
        }
        let size = size.min(476);
        if addr < flash_base || u64::from(addr) + u64::from(size) > u64::from(flash_base) + u64::from(flash_size) {
            return Err(Uf2Error::OutOfFlash { block: n, addr });
        }
        out.push((addr, block[32..32 + size as usize].to_vec()));
    }
    if out.is_empty() {
        return Err(Uf2Error::NoRp2040Blocks);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(addr: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        for (i, w) in [0x0A32_4655, 0x9E5D_5157, FLAG_FAMILY_ID, addr, payload.len() as u32, 0, 1, RP2040_FAMILY]
            .into_iter()
            .enumerate()
        {
            b[i * 4..i * 4 + 4].copy_from_slice(&u32::to_le_bytes(w));
        }
        b[32..32 + payload.len()].copy_from_slice(payload);
        b[508..].copy_from_slice(&0x0AB1_6F30u32.to_le_bytes());
        b
    }

    #[test]
    fn blocks_to_flash_contents() {
        let file = [block(0x1000_0000, &[1, 2, 3]), block(0x1000_0100, &[4])].concat();
        let parsed = parse(&file, 0x1000_0000, 0x20_0000).unwrap();
        assert_eq!(parsed, vec![(0x1000_0000, vec![1, 2, 3]), (0x1000_0100, vec![4])]);
        assert_eq!(parse(&file[..100], 0x1000_0000, 0x20_0000), Err(Uf2Error::Magic(0)));
        assert!(matches!(parse(&block(0x2000_0000, &[1]), 0x1000_0000, 0x20_0000), Err(Uf2Error::OutOfFlash { .. })));
    }
}
