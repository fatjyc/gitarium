/**
 * [INPUT]: 仅依赖字节切片；不依赖数据库与传输
 * [OUTPUT]: 导出 apply_delta，把 Git delta 指令应用到基对象内容
 * [POS]: 推送解码的最后一步；薄包与包内差量都经这里还原成完整对象
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let mut position = 0usize;
    let base_size = read_varint(delta, &mut position)?;
    if base_size as usize != base.len() {
        return Err(format!(
            "差量声明的基对象长度 {base_size} 与实际 {} 不符",
            base.len()
        ));
    }
    let result_size = read_varint(delta, &mut position)? as usize;
    let mut result = Vec::with_capacity(result_size);
    while position < delta.len() {
        let opcode = delta[position];
        position += 1;
        if opcode & 0x80 != 0 {
            let mut offset = 0usize;
            for bit in 0..4 {
                if opcode & (1 << bit) != 0 {
                    let byte = *delta
                        .get(position)
                        .ok_or_else(|| "复制指令的偏移被截断".to_string())?;
                    position += 1;
                    offset |= (byte as usize) << (bit * 8);
                }
            }
            let mut size = 0usize;
            for bit in 0..3 {
                if opcode & (1 << (4 + bit)) != 0 {
                    let byte = *delta
                        .get(position)
                        .ok_or_else(|| "复制指令的长度被截断".to_string())?;
                    position += 1;
                    size |= (byte as usize) << (bit * 8);
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let end = offset
                .checked_add(size)
                .ok_or_else(|| "复制指令范围溢出".to_string())?;
            if end > base.len() {
                return Err("复制指令超出基对象范围".to_string());
            }
            result.extend_from_slice(&base[offset..end]);
        } else if opcode != 0 {
            let length = opcode as usize;
            let end = position + length;
            if end > delta.len() {
                return Err("插入指令的字节被截断".to_string());
            }
            result.extend_from_slice(&delta[position..end]);
            position = end;
        } else {
            return Err("差量包含保留指令 0".to_string());
        }
    }
    if result.len() != result_size {
        return Err(format!(
            "差量结果长度 {} 与声明 {result_size} 不符",
            result.len()
        ));
    }
    Ok(result)
}

fn read_varint(data: &[u8], position: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *data
            .get(*position)
            .ok_or_else(|| "差量的长度字段被截断".to_string())?;
        *position += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift >= 64 {
            return Err("差量的长度字段过长".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_then_insert_reproduces_result() {
        let base = b"Hello, world!\n";
        // 基对象长度 14，结果长度 14；复制偏移 0 长度 7，再插入 7 字节。
        let delta = b"\x0e\x0e\x90\x07\x07gitdb!\n";
        assert_eq!(
            apply_delta(base, delta).unwrap(),
            b"Hello, gitdb!\n".to_vec()
        );
    }

    #[test]
    fn zero_size_copy_means_65536_bytes() {
        let base = vec![7u8; 65536];
        // base_size=65536, result_size=65536, copy offset=0；未设置大小字节时表示 65536
        let delta = vec![0x80, 0x80, 0x04, 0x80, 0x80, 0x04, 0x80];
        assert_eq!(apply_delta(&base, &delta).unwrap().len(), 65536);
    }

    #[test]
    fn truncated_delta_is_rejected() {
        assert!(apply_delta(b"abc", b"\x03").is_err());
    }
}
