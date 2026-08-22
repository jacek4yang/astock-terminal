//! 请求帧构建与响应头解析。
//!
//! 内化自 tdxrs `src/net/packet.rs` / `src/net/utils.rs`（MIT，见 crate 根文档注释）。

use super::constants::*;
use crate::error::{Result, TdxError};

/// 16 字节响应头，全小端 `<IIIHH`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseHeader {
    pub seq: u32,
    pub method: u32,
    pub zip_size: u32,
    pub unzip_size: u32,
}

impl ResponseHeader {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < RESPONSE_HEADER_SIZE {
            return Err(TdxError::Protocol(format!(
                "response header: expected {RESPONSE_HEADER_SIZE} bytes, got {}",
                buf.len()
            )));
        }
        let seq = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let method = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let zip_size = u16::from_le_bytes([buf[12], buf[13]]) as u32;
        let unzip_size = u16::from_le_bytes([buf[14], buf[15]]) as u32;
        Ok(Self {
            seq,
            method,
            zip_size,
            unzip_size,
        })
    }
}

/// 股票代码 → 6 字节定长数组（ASCII，右补 0）。
pub fn code_bytes(code: &str) -> [u8; 6] {
    let mut buf = [0u8; 6];
    let bytes = code.as_bytes();
    let len = bytes.len().min(6);
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

/// 构建 K 线请求包（0x052D，38 字节）。`fq` 为协议保留位，恒传 0（未复权）。
pub fn build_security_bars_packet(
    category: u8,
    market: u8,
    code: &str,
    start: u16,
    count: u16,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(38);
    pkt.extend_from_slice(&MAGIC.to_le_bytes());
    pkt.extend_from_slice(&0x01016408u32.to_le_bytes());
    pkt.extend_from_slice(&0x001Cu16.to_le_bytes());
    pkt.extend_from_slice(&0x001Cu16.to_le_bytes());
    pkt.extend_from_slice(&CMD_SECURITY_BARS.to_le_bytes());
    pkt.extend_from_slice(&(market as u16).to_le_bytes());
    pkt.extend_from_slice(&code_bytes(code));
    pkt.extend_from_slice(&(category as u16).to_le_bytes());
    pkt.extend_from_slice(&0u16.to_le_bytes()); // fq 保留位 = 0（未复权）
    pkt.extend_from_slice(&start.to_le_bytes());
    pkt.extend_from_slice(&count.to_le_bytes());
    pkt.extend_from_slice(&0u32.to_le_bytes());
    pkt.extend_from_slice(&0u32.to_le_bytes());
    pkt.extend_from_slice(&0u16.to_le_bytes());
    pkt
}

/// 构建证券数量请求包（0x044E）。心跳也复用此包。
pub fn build_security_count_packet(market: u8) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(18);
    pkt.extend_from_slice(&[
        0x0c, 0x0c, 0x18, 0x6c, 0x00, 0x01, 0x08, 0x00, 0x08, 0x00, 0x4e, 0x04,
    ]);
    pkt.extend_from_slice(&(market as u16).to_le_bytes());
    pkt.extend_from_slice(&[0x75, 0xc7, 0x33, 0x01]);
    pkt
}

/// 构建证券列表请求包（0x0450）。
pub fn build_security_list_packet(market: u8, start: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(16);
    pkt.extend_from_slice(&[
        0x0c, 0x01, 0x18, 0x64, 0x01, 0x01, 0x06, 0x00, 0x06, 0x00, 0x50, 0x04,
    ]);
    pkt.extend_from_slice(&(market as u16).to_le_bytes());
    pkt.extend_from_slice(&start.to_le_bytes());
    pkt
}

/// 构建五档快照请求包（0x053E；头为特殊布局，命令码为 u32）。
/// 单次上限 [`MAX_QUOTES_COUNT`] 只，超出截断。
pub fn build_security_quotes_packet(stocks: &[(u8, &str)]) -> Vec<u8> {
    let stocks = if stocks.len() > MAX_QUOTES_COUNT {
        &stocks[..MAX_QUOTES_COUNT]
    } else {
        stocks
    };
    let stock_len = stocks.len() as u16;
    let pkgdatalen = stock_len * 7 + 12;

    let mut pkt = Vec::with_capacity(26 + stocks.len() * 7);
    pkt.extend_from_slice(&MAGIC.to_le_bytes());
    pkt.extend_from_slice(&0x02006320u32.to_le_bytes());
    pkt.extend_from_slice(&pkgdatalen.to_le_bytes());
    pkt.extend_from_slice(&pkgdatalen.to_le_bytes());
    pkt.extend_from_slice(&CMD_SECURITY_QUOTES.to_le_bytes());
    pkt.extend_from_slice(&0u32.to_le_bytes());
    pkt.extend_from_slice(&0u16.to_le_bytes());
    pkt.extend_from_slice(&stock_len.to_le_bytes());
    for &(market, code) in stocks {
        pkt.push(market);
        pkt.extend_from_slice(&code_bytes(code));
    }
    pkt
}

/// 构建历史分时请求包（0x0FB4）。`date` 为 YYYYMMDD。
pub fn build_history_minute_packet(market: u8, code: &str, date: u32) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(23);
    pkt.extend_from_slice(&[
        0x0c, 0x01, 0x30, 0x00, 0x01, 0x01, 0x0d, 0x00, 0x0d, 0x00, 0xb4, 0x0f,
    ]);
    pkt.extend_from_slice(&date.to_le_bytes());
    pkt.push(market);
    pkt.extend_from_slice(&code_bytes(code));
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_parse_basic() {
        let buf: [u8; 16] = [
            1, 0, 0, 0, // seq
            2, 0, 0, 0, // method
            0, 0, 0, 0, // reserved
            100, 0, // zip_size
            200, 0, // unzip_size
        ];
        let h = ResponseHeader::parse(&buf).unwrap();
        assert_eq!(h.seq, 1);
        assert_eq!(h.method, 2);
        assert_eq!(h.zip_size, 100);
        assert_eq!(h.unzip_size, 200);
    }

    #[test]
    fn header_parse_too_short() {
        assert!(ResponseHeader::parse(&[0u8; 10]).is_err());
        assert!(ResponseHeader::parse(&[]).is_err());
    }

    #[test]
    fn bars_packet_layout() {
        let pkt = build_security_bars_packet(9, 1, "600519", 0, 800);
        assert_eq!(pkt.len(), 38);
        assert_eq!(u16::from_le_bytes([pkt[0], pkt[1]]), MAGIC);
        assert_eq!(u16::from_le_bytes([pkt[10], pkt[11]]), CMD_SECURITY_BARS);
        assert_eq!(u16::from_le_bytes([pkt[12], pkt[13]]), 1); // market
        assert_eq!(&pkt[14..20], b"600519");
        assert_eq!(u16::from_le_bytes([pkt[20], pkt[21]]), 9); // category
        assert_eq!(u16::from_le_bytes([pkt[22], pkt[23]]), 0); // fq 保留位
        assert_eq!(u16::from_le_bytes([pkt[26], pkt[27]]), 800); // count
    }

    #[test]
    fn quotes_packet_layout_and_truncation() {
        let stocks: Vec<(u8, &str)> = vec![(1, "600519"), (0, "000001")];
        let pkt = build_security_quotes_packet(&stocks);
        // 固定部分: 2+4+2+2+4(cmd u32)+4+2+2 = 22 字节
        assert_eq!(pkt.len(), 22 + 14);
        assert_eq!(
            u32::from_le_bytes([pkt[10], pkt[11], pkt[12], pkt[13]]),
            CMD_SECURITY_QUOTES
        );
        assert_eq!(u16::from_le_bytes([pkt[20], pkt[21]]), 2); // stock_len
        assert_eq!(pkt[22], 1); // first market
        assert_eq!(&pkt[23..29], b"600519");

        // 超过 60 只自动截断
        let many: Vec<(u8, &str)> = (0..100).map(|_| (1u8, "600519")).collect();
        let pkt = build_security_quotes_packet(&many);
        assert_eq!(
            u16::from_le_bytes([pkt[20], pkt[21]]),
            MAX_QUOTES_COUNT as u16
        );
        assert_eq!(pkt.len(), 22 + MAX_QUOTES_COUNT * 7);
    }

    #[test]
    fn history_minute_packet_layout() {
        let pkt = build_history_minute_packet(1, "600519", 20260821);
        assert_eq!(pkt.len(), 23);
        assert_eq!(
            u16::from_le_bytes([pkt[10], pkt[11]]),
            CMD_HISTORY_MINUTE_TIME
        );
        assert_eq!(
            u32::from_le_bytes([pkt[12], pkt[13], pkt[14], pkt[15]]),
            20260821
        );
        assert_eq!(pkt[16], 1); // market
        assert_eq!(&pkt[17..23], b"600519");
    }

    #[test]
    fn count_and_list_packets() {
        let c = build_security_count_packet(1);
        assert_eq!(c.len(), 18);
        assert_eq!(u16::from_le_bytes([c[10], c[11]]), CMD_SECURITY_COUNT);
        let l = build_security_list_packet(0, 1000);
        assert_eq!(l.len(), 16);
        assert_eq!(u16::from_le_bytes([l[10], l[11]]), CMD_SECURITY_LIST);
        assert_eq!(u16::from_le_bytes([l[14], l[15]]), 1000); // start
    }
}
