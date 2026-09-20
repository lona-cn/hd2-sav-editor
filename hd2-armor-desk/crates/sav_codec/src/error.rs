//! Distinguishable codec errors. Messages are developer-facing; the UI maps
//! these to player-facing Chinese text without inventing success.

use thiserror::Error;

/// Decoding failures, ordered the way `SaveImage::decode` checks them.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DecodeError {
    #[error("文件长度 {size} 超出接受范围（40 字节至 16 MiB）")]
    TruncatedOrOversized { size: usize },

    #[error("外层魔数不匹配，这不是本工具支持的容器")]
    UnrecognizedMagic,

    #[error("容器标志位不是已核验组合")]
    UnsupportedFlags,

    #[error(
        "外层 CRC32 不匹配（存储 0x{stored:08X}，计算 0x{calculated:08X}）：文件可能未写完或已损坏"
    )]
    BadOuterCrc { stored: u32, calculated: u32 },

    #[error("压缩区长度字段不匹配（存储 {stored}，实际 {actual}）")]
    StoredSizeMismatch { stored: usize, actual: usize },

    #[error("逻辑正文长度 {size} 超出接受范围")]
    InvalidLogicalSize { size: usize },

    #[error("逻辑长度副本不匹配（副本 {copy}，长度 {size}）")]
    LogicalCopyMismatch { copy: u64, size: usize },

    #[error("偏移 {at:#x} 处缺少压缩块长度")]
    MissingBlockLength { at: usize },

    #[error("偏移 {at:#x} 的压缩块长度 {length} 越界")]
    BlockOutOfBounds { at: usize, length: usize },

    #[error("偏移 {at:#x} 的 raw LZ4 块无效")]
    InvalidLz4Block { at: usize },

    #[error("偏移 {at:#x} 的块解压后长度为 {length}，不是 65536")]
    UnexpectedBlockLength { at: usize, length: usize },

    #[error("块解析在 {consumed} 字节处停止，文件总长 {total}：存在未解析的尾部数据")]
    TrailingBytes { consumed: usize, total: usize },

    #[error("终端填充区存在非零字节")]
    NonZeroPadding,

    #[error("正文内层长度 {stored} 与逻辑长度 {logical} 不一致")]
    InnerLengthMismatch { stored: usize, logical: usize },

    #[error("内层 MurmurHash64A-low32 校验失败（存储 0x{stored:08X}，计算 0x{calculated:08X}）：禁止写入")]
    BadInnerHash { stored: u32, calculated: u32 },

    #[error("读取越界")]
    OutOfBounds,
}

impl DecodeError {
    /// Whether a watcher should treat this as "still being written" rather than
    /// a hard failure. Both are retried, but only this one is expected to clear.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            DecodeError::BadOuterCrc { .. }
                | DecodeError::StoredSizeMismatch { .. }
                | DecodeError::MissingBlockLength { .. }
                | DecodeError::BlockOutOfBounds { .. }
                | DecodeError::TrailingBytes { .. }
                | DecodeError::InvalidLz4Block { .. }
                | DecodeError::UnexpectedBlockLength { .. }
        )
    }
}

/// Encoding failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EncodeError {
    #[error("此存档布局未经核验，只允许查看，不能按已知字段写入")]
    LayoutNotWritable,

    #[error("偏移 {offset:#x} 的修改超出正文范围")]
    PatchOutOfBounds { offset: usize },

    #[error("偏移 {offset:#x} 的原始字节不匹配：期望 {expected:02X?}，实际 {found:02X?}")]
    PatchBeforeMismatch {
        offset: usize,
        expected: [u8; 4],
        found: [u8; 4],
    },

    #[error("偏移 {offset:#x} 位于受保护的正文头部（魔数、长度或内层校验），不能直接修改")]
    ProtectedRegion { offset: usize },

    #[error("偏移 {offset:#x} 的修改与另一处修改重叠")]
    OverlappingPatches { offset: usize },

    #[error("压缩块长度超出 u32")]
    BlockTooLarge,

    #[error("重新封装后独立解码失败：{0}")]
    VerificationFailed(String),

    #[error("重新封装后正文与目标不一致")]
    VerificationMismatch,
}
