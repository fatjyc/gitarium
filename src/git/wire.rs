/**
 * [INPUT]: 仅依赖 std::io；调用方提供读写流
 * [OUTPUT]: 导出 Pkt 枚举、write_pkt、write_flush、read_pkt
 * [POS]: Git 协议 v0 的线格式层；HTTP 与 SSH 传输都复用
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::io::{BufRead, ErrorKind, Write};

const MAX_PKT: usize = 65520;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pkt {
    Flush,
    Line(Vec<u8>),
    Eof,
}

pub fn write_pkt(out: &mut dyn Write, data: &[u8]) -> Result<(), String> {
    let length = data.len() + 4;
    if length > MAX_PKT {
        return Err(format!("pkt-line 长度 {length} 超过上限 {MAX_PKT}"));
    }
    out.write_all(format!("{length:04x}").as_bytes())
        .map_err(|e| format!("写 pkt-line 长度失败: {e}"))?;
    out.write_all(data)
        .map_err(|e| format!("写 pkt-line 内容失败: {e}"))
}

pub fn write_flush(out: &mut dyn Write) -> Result<(), String> {
    out.write_all(b"0000")
        .map_err(|e| format!("写 flush 失败: {e}"))
}

pub fn read_pkt(input: &mut dyn BufRead) -> Result<Pkt, String> {
    let mut header = [0u8; 4];
    if !read_full(input, &mut header)? {
        return Ok(Pkt::Eof);
    }
    let text = std::str::from_utf8(&header).map_err(|_| "pkt-line 长度不是 ASCII".to_string())?;
    let length =
        usize::from_str_radix(text, 16).map_err(|_| format!("pkt-line 长度无效: {text}"))?;
    if length == 0 {
        return Ok(Pkt::Flush);
    }
    if !(4..=MAX_PKT).contains(&length) {
        return Err(format!("pkt-line 长度越界: {length}"));
    }
    let mut data = vec![0u8; length - 4];
    if !read_full(input, &mut data)? {
        return Err("pkt-line 内容被截断".to_string());
    }
    Ok(Pkt::Line(data))
}

fn read_full(input: &mut dyn BufRead, buffer: &mut [u8]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err("读取 pkt-line 时连接提前结束".to_string());
            }
            Ok(count) => filled += count,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("读取 pkt-line 失败: {error}")),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_is_length_prefixed_with_four_hex_digits() {
        let mut out = Vec::new();
        write_pkt(&mut out, b"done\n").unwrap();
        assert_eq!(out, b"0009done\n".to_vec());
    }

    #[test]
    fn flush_is_four_zero_digits() {
        let mut out = Vec::new();
        write_flush(&mut out).unwrap();
        assert_eq!(out, b"0000".to_vec());
    }

    #[test]
    fn read_roundtrips_line_and_flush() {
        let mut input = b"0009done\n0000".as_slice();
        assert_eq!(read_pkt(&mut input).unwrap(), Pkt::Line(b"done\n".to_vec()));
        assert_eq!(read_pkt(&mut input).unwrap(), Pkt::Flush);
        assert_eq!(read_pkt(&mut input).unwrap(), Pkt::Eof);
    }

    #[test]
    fn invalid_length_prefix_is_rejected() {
        let mut input = b"zzzz".as_slice();
        assert!(read_pkt(&mut input).is_err());
        let mut input = b"0001".as_slice();
        assert!(read_pkt(&mut input).is_err());
    }
}
