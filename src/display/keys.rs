//! RFB keysyms to the PC set-1 scancodes used by SPICE inputs.

pub(super) fn scancode(symbol: u32) -> Option<u32> {
    if let Ok(ascii) = u8::try_from(symbol) {
        let ascii = ascii.to_ascii_lowercase();
        for (first, keys) in [
            (0x02, b"1234567890-=".as_slice()),
            (0x10, b"qwertyuiop[]".as_slice()),
            (0x1e, b"asdfghjkl;'\x60".as_slice()),
            (0x2b, b"\\zxcvbnm,./".as_slice()),
        ] {
            if let Some(index) = keys.iter().position(|key| *key == ascii) {
                return Some(first + index as u32);
            }
        }
        if let Some(index) = b"!@#$%^&*()_+".iter().position(|key| *key == ascii) {
            return Some(2 + index as u32);
        }
        return Some(match ascii {
            b' ' => 0x39,
            b'{' => 0x1a,
            b'}' => 0x1b,
            b':' => 0x27,
            b'"' => 0x28,
            b'~' => 0x29,
            b'|' => 0x2b,
            b'<' => 0x33,
            b'>' => 0x34,
            b'?' => 0x35,
            _ => return None,
        });
    }
    Some(match symbol {
        0xff08 => 0x0e, // Backspace
        0xff09 | 0xfe20 => 0x0f,
        0xff0d => 0x1c,
        0xff1b => 0x01,
        0xff50 => 0x147, // Home
        0xff51 => 0x14b,
        0xff52 => 0x148,
        0xff53 => 0x14d,
        0xff54 => 0x150,
        0xff55 => 0x149,
        0xff56 => 0x151,
        0xff57 => 0x14f,
        0xff61 => 0x137,
        0xff63 => 0x152,
        0xffbe..=0xffc7 => 0x3b + symbol - 0xffbe,
        0xffc8 => 0x57,
        0xffc9 => 0x58,
        0xffe1 => 0x2a,
        0xffe2 => 0x36,
        0xffe3 => 0x1d,
        0xffe4 => 0x11d,
        0xffe5 => 0x3a,
        0xffe9 => 0x38,
        0xffea => 0x138,
        0xffeb => 0x15b,
        0xffec => 0x15c,
        0xffff => 0x153,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_spice_keys_and_modifiers() {
        assert_eq!(scancode('A'.into()), Some(0x1e));
        assert_eq!(scancode('!'.into()), Some(0x02));
        assert_eq!(scancode(0xffe3), Some(0x1d));
        assert_eq!(scancode(0xffff), Some(0x153));
    }
}
