//! 快照文件头编解码（T4-01）。
//!
//! 布局（共 12 字节，全部小端）：
//!
//! ```text
//! +0..4   magic: "IDX1"
//! +4..8   format_version: u32
//! +8..12  crc32: u32   ← 覆盖文件头之后的全部正文
//! ```

use std::io::{Read, Write};

use crate::error::{Error, Result};

/// 魔数。变更即视为不兼容格式。
pub const MAGIC: &[u8; 4] = b"IDX1";

/// 当前格式版本。
///
/// 注意：`positions` feature 会改变 `Posting` 的序列化布局，
/// 该 feature 的开关必须在版本号上体现（这里约定：positions 开启时版本 +1 写入）。
pub const FORMAT_VERSION: u32 = 1;

pub const HEADER_LEN: usize = 12;

pub fn write_header<W: Write>(w: &mut W, crc: u32) -> std::io::Result<()> {
    w.write_all(MAGIC)?;
    w.write_all(&FORMAT_VERSION.to_le_bytes())?;
    w.write_all(&crc.to_le_bytes())?;
    Ok(())
}

/// 读并校验文件头，返回正文的 CRC 期望值。
///
/// - magic 错 / 版本不匹配 → `SnapshotVersionMismatch`（绝不静默读错）
pub fn read_header<R: Read>(r: &mut R) -> Result<u32> {
    let mut buf = [0u8; HEADER_LEN];
    r.read_exact(&mut buf).map_err(Error::Io)?;

    if &buf[0..4] != MAGIC {
        // magic 读成 u32 当作 found，与版本号共用同一错误类型（见 Error 定义）
        return Err(Error::SnapshotVersionMismatch {
            found: u32::from_le_bytes(buf[0..4].try_into().unwrap_or_default()),
            expected: u32::from_le_bytes(
                MAGIC
                    .iter()
                    .take(4)
                    .copied()
                    .collect::<Vec<u8>>()
                    .try_into()
                    .unwrap_or_default(),
            ),
        });
    }
    let found_version = u32::from_le_bytes(buf[4..8].try_into().expect("4 字节"));
    if found_version != FORMAT_VERSION {
        return Err(Error::SnapshotVersionMismatch {
            found: found_version,
            expected: FORMAT_VERSION,
        });
    }
    Ok(u32::from_le_bytes(buf[8..12].try_into().expect("4 字节")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let mut buf = Vec::new();
        write_header(&mut buf, 0xDEAD_BEEF).unwrap();
        assert_eq!(buf.len(), HEADER_LEN);
        let mut cur = std::io::Cursor::new(&buf);
        let crc = read_header(&mut cur).unwrap();
        assert_eq!(crc, 0xDEAD_BEEF);
    }

    #[test]
    fn 错误magic拒绝() {
        let mut buf = Vec::new();
        write_header(&mut buf, 1).unwrap();
        buf[0] = b'X'; // 破坏 magic
        let mut cur = std::io::Cursor::new(&buf);
        assert!(matches!(
            read_header(&mut cur),
            Err(Error::SnapshotVersionMismatch { .. })
        ));
    }

    #[test]
    fn 版本不匹配拒绝() {
        let mut buf = Vec::new();
        write_header(&mut buf, 1).unwrap();
        buf[4] = 0xFF; // 破坏版本
        let mut cur = std::io::Cursor::new(&buf);
        assert!(matches!(
            read_header(&mut cur),
            Err(Error::SnapshotVersionMismatch { .. })
        ));
    }
}
