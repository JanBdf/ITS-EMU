//! External devices as the command line and the GUI's settings name them:
//! `encoder@PA6,PA7`, `ds18b20@PG2`, `pico=FIRMWARE.uf2[@GP0=PF0,...]`.

use std::fmt;

use crate::pico::PicoSpec;

/// A device on the header pins.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceSpec {
    /// Rotary encoder with its contacts A, B on two pins.
    Encoder((char, u32), (char, u32)),
    /// DS18B20 temperature sensor on a 1-Wire pin.
    Ds18b20((char, u32)),
    /// Raspberry Pi Pico running a firmware.
    Pico(PicoSpec),
}

impl DeviceSpec {
    pub fn parse(s: &str) -> Result<Self, String> {
        if let Some(spec) = s.trim().strip_prefix("pico=") {
            return PicoSpec::parse(spec).map(Self::Pico);
        }
        let (kind, pins) = s.split_once('@').ok_or_else(|| format!("expected DEVICE@PINS, got {s:?}"))?;
        let pins = pins.split(',').map(parse_pin).collect::<Result<Vec<_>, _>>()?;
        match (kind.trim().to_ascii_lowercase().as_str(), pins.as_slice()) {
            ("encoder", &[a, b]) => Ok(Self::Encoder(a, b)),
            ("encoder", _) => Err("an encoder needs two pins: encoder@PA,PB".to_owned()),
            ("ds18b20", &[pin]) => Ok(Self::Ds18b20(pin)),
            ("ds18b20", _) => Err("a DS18B20 needs one pin: ds18b20@PG2".to_owned()),
            _ => Err(format!("unknown device {kind:?} (known: encoder, ds18b20, pico=FILE.uf2)")),
        }
    }

    /// The board pins the device is connected to.
    pub fn board_pins(&self) -> Vec<(char, u32)> {
        match self {
            Self::Encoder(a, b) => vec![*a, *b],
            Self::Ds18b20(pin) => vec![*pin],
            Self::Pico(spec) => spec.wiring.iter().map(|&(_, pin)| pin).collect(),
        }
    }
}

impl fmt::Display for DeviceSpec {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Encoder(a, b) => write!(f, "encoder@{},{}", pin_name(*a), pin_name(*b)),
            Self::Ds18b20(pin) => write!(f, "ds18b20@{}", pin_name(*pin)),
            Self::Pico(spec) => write!(f, "pico={spec}"),
        }
    }
}

/// `PC6` -> ('C', 6)
pub fn parse_pin(s: &str) -> Result<(char, u32), String> {
    let s = s.trim().to_ascii_uppercase();
    let b = s.as_bytes();
    if b.len() >= 3
        && b[0] == b'P'
        && (b'A'..=b'K').contains(&b[1])
        && let Some(n) = s[2..].parse::<u32>().ok().filter(|&n| n < 16)
    {
        return Ok((b[1] as char, n));
    }
    Err(format!("expected a pin such as PC6, got {s:?}"))
}

/// ('C', 6) -> `PC6`
pub fn pin_name((port, pin): (char, u32)) -> String {
    format!("P{port}{pin}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_read_back_what_they_write() {
        for s in ["encoder@PA6,PA7", "ds18b20@PG2", "pico=/a/gen.uf2@GP0=PF0,GP5=PF1"] {
            assert_eq!(DeviceSpec::parse(s).unwrap().to_string(), s);
        }
        assert_eq!(DeviceSpec::parse("Encoder@pa6, pb7").unwrap(), DeviceSpec::Encoder(('A', 6), ('B', 7)));
        assert_eq!(DeviceSpec::parse("pico=gen.uf2").unwrap().to_string(), "pico=gen.uf2@GP0=PF0,GP5=PF1");
        assert!(DeviceSpec::parse("encoder@PA6").is_err());
        assert!(DeviceSpec::parse("ds18b20@PZ1").is_err());
        assert!(DeviceSpec::parse("lamp@PA1").is_err());
    }
}
