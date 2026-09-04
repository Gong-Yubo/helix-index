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

/// 当前格式版本（base）。
///
/// **P6 / I-08**：1 → 2（快照新增 `ConfigFingerprint` section，p6-design 8.3）。
/// 实际写入/读取的版本号用 [`effective_version`]（positions 时 base+1），
/// 使 `positions` 快照与非 positions 快照**互斥**（否则两者版本号相同但
/// `Posting` 布局不同，仅靠 CRC 兜底——这是 I-09 修掉的现存隐患）。
pub const FORMAT_VERSION: u32 = 2;

/// positions feature 在 base 版本上的偏移量。
///
/// `positions` 会给 `Posting` 增加 `Vec<u32>` 字段，改变序列化布局。
/// 因此开启 `positions` 时的有效版本 = `FORMAT_VERSION + POSITIONS_OFFSET`。
const POSITIONS_OFFSET: u32 = 1;

/// 当前编译配置下的**有效**快照版本号。
///
/// `positions` 快照与非 positions 快照版本号必须不同，否则会静默互读
/// （bincode 解码失败被 CRC 兜底前，可能读到错位数据）。此函数把
/// 「positions 时 +1」从**注释约定**落成**实现**（p6-design 8.3 / 评审 P1）。
pub const fn effective_version() -> u32 {
    if cfg!(feature = "positions") {
        FORMAT_VERSION + POSITIONS_OFFSET
    } else {
        FORMAT_VERSION
    }
}

pub const HEADER_LEN: usize = 12;

pub fn write_header<W: Write>(w: &mut W, crc: u32) -> std::io::Result<()> {
    w.write_all(MAGIC)?;
    w.write_all(&effective_version().to_le_bytes())?;
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
    let effective = effective_version();
    if found_version != effective {
        return Err(Error::SnapshotVersionMismatch {
            found: found_version,
            expected: effective,
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

    #[test]
    fn effective_version与feature一致() {
        // positions feature 下 effective = base + 1，否则 = base
        #[cfg(feature = "positions")]
        assert_eq!(effective_version(), FORMAT_VERSION + 1);
        #[cfg(not(feature = "positions"))]
        assert_eq!(effective_version(), FORMAT_VERSION);
    }

    #[test]
    fn 相邻版本号互拒() {
        // 模拟"另一 feature 的快照"：写入 base+1 版本，当前编译配置必须拒绝。
        // （positions 快照与非 positions 快照互斥——不得靠 CRC 兜底，评审 P1）
        let mut buf = Vec::new();
        let other_version = FORMAT_VERSION + 1;
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&other_version.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // crc 占位
        let mut cur = std::io::Cursor::new(&buf);
        let res = read_header(&mut cur);
        if effective_version() == other_version {
            // 恰好本配置就是 other_version（positions 下），应正常读
            assert!(res.is_ok());
        } else {
            assert!(matches!(
                res,
                Err(Error::SnapshotVersionMismatch { found, expected })
                    if found == other_version && expected == effective_version()
            ));
        }
    }
}
