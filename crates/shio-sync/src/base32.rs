//! Crockford Base32，用于 32 字节配对 secret 的文本表示。

use shio_core::{Error, Result};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 把 32 字节 secret 编码为 52 个 Crockford Base32 字符。
pub fn encode_secret(secret: &[u8; 32]) -> String {
    let mut out = String::with_capacity(52);
    let mut buffer: u32 = 0;
    let mut bits = 0u8;
    for byte in secret {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0x1f) as usize;
            out.push(ALPHABET[index] as char);
        }
    }
    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[index] as char);
    }
    out
}

/// 解码 Crockford Base32；接受大小写、`-` 与空格，拒绝非法字符和多余位。
pub fn decode_secret(text: &str) -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    let mut buffer: u32 = 0;
    let mut bits = 0u8;
    let mut written = 0usize;
    let mut chars = 0usize;

    for raw in text.chars() {
        if raw == '-' || raw.is_whitespace() {
            continue;
        }
        let upper = raw.to_ascii_uppercase();
        let value = match upper {
            '0' | 'O' => 0u8,
            '1' | 'I' | 'L' => 1,
            '2'..='9' => upper as u8 - b'0',
            'A'..='H' => upper as u8 - b'A' + 10,
            'J' | 'K' => 18 + (upper as u8 - b'J'),
            'M' | 'N' => 20 + (upper as u8 - b'M'),
            'P'..='T' => 22 + (upper as u8 - b'P'),
            'V'..='Z' => 27 + (upper as u8 - b'V'),
            _ => return Err(Error::PairingInvalid),
        };
        chars += 1;
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            if written >= bytes.len() {
                return Err(Error::PairingInvalid);
            }
            bytes[written] = ((buffer >> bits) & 0xff) as u8;
            written += 1;
        }
    }

    if written != bytes.len() || chars != 52 {
        return Err(Error::PairingInvalid);
    }
    if bits > 0 && (buffer & ((1u32 << bits) - 1)) != 0 {
        return Err(Error::PairingInvalid);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let secret = [7u8; 32];
        let text = encode_secret(&secret);
        assert_eq!(text.len(), 52);
        assert_eq!(decode_secret(&text).unwrap(), secret);
        assert_eq!(decode_secret(&text.to_ascii_lowercase()).unwrap(), secret);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode_secret("").is_err());
        assert!(decode_secret("U").is_err());
        assert!(decode_secret(&"0".repeat(51)).is_err());
    }
}
