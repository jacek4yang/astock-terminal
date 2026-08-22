//! 响应记录解析器。
//!
//! 内化自 tdxrs `src/protocol/parsers.rs`（MIT，见 crate 根文档注释）。
//! 价格协议原始单位为「厘」，解析时 ÷1000 转为元；
//! 成交量/成交额经 [`get_volume`] 解码（服务端语义，未做周期级单位换算）。

use encoding_rs::GBK;

use super::types::{price_coefficient, MinuteBar, Quote, SecurityBar, SecurityInfo};
use super::varint::{get_price, get_volume};
use crate::error::{Result, TdxError};

#[inline]
fn read_u16(data: &[u8], pos: usize) -> u16 {
    if pos + 2 > data.len() {
        return 0;
    }
    u16::from_le_bytes([data[pos], data[pos + 1]])
}

#[inline]
fn read_u32(data: &[u8], pos: usize) -> u32 {
    if pos + 4 > data.len() {
        return 0;
    }
    u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
}

fn err(msg: &str) -> TdxError {
    TdxError::Protocol(msg.to_string())
}

/// 解析证券数量（0x044E 响应体）。
pub fn parse_security_count(body: &[u8]) -> Result<u16> {
    if body.len() < 2 {
        return Err(err("count body too short"));
    }
    Ok(read_u16(body, 0))
}

/// 解析证券列表（0x0450 响应体，记录定长 29 字节 `<6sH8s4sBI4s>`）。
pub fn parse_security_list(body: &[u8]) -> Result<Vec<SecurityInfo>> {
    if body.len() < 2 {
        return Err(err("security list body too short"));
    }
    let count = read_u16(body, 0) as usize;
    let mut pos = 2;
    let mut result = Vec::with_capacity(count);

    for _ in 0..count {
        if pos + 29 > body.len() {
            break;
        }
        let code = String::from_utf8_lossy(&body[pos..pos + 6])
            .trim_end_matches('\0')
            .to_string();
        pos += 6;
        let volunit = read_u16(body, pos);
        pos += 2;
        let (name, _, _) = GBK.decode(&body[pos..pos + 8]);
        let name = name.trim_end_matches('\0').to_string();
        pos += 8;
        pos += 4; // reserved
        let decimal_point = body[pos];
        pos += 1;
        let pre_close = get_volume(read_u32(body, pos) as i64);
        pos += 4;
        pos += 4; // reserved

        result.push(SecurityInfo {
            code,
            name,
            volunit,
            decimal_point,
            pre_close,
        });
    }
    Ok(result)
}

/// K 线时间解码（双轨：分钟级位打包相对时间 / 日及以上 YYYYMMDD 整数）。
fn get_datetime(minute_level: bool, buf: &[u8], pos: usize) -> (u32, u32, u32, u32, u32, usize) {
    if minute_level {
        let zip_day = read_u16(buf, pos) as u32;
        let minutes = read_u16(buf, pos + 2) as u32;
        (
            (zip_day >> 11) + 2004,
            (zip_day % 2048) / 100,
            (zip_day % 2048) % 100,
            minutes / 60,
            minutes % 60,
            pos + 4,
        )
    } else {
        let zip_day = read_u32(buf, pos);
        (
            zip_day / 10000,
            (zip_day % 10000) / 100,
            zip_day % 100,
            0,
            0,
            pos + 4,
        )
    }
}

/// 解析个股 K 线（0x052D 响应体）。返回未复权数据，按时间升序。
///
/// `minute_level` 决定时间解码分支，必须与实际请求的 category 匹配。
pub fn parse_security_bars(body: &[u8], minute_level: bool) -> Result<Vec<SecurityBar>> {
    if body.len() < 2 {
        return Err(err("bars body too short"));
    }
    let count = read_u16(body, 0) as usize;
    let mut pos = 2;
    let mut result = Vec::with_capacity(count);
    let mut pre_diff_base: i64 = 0;

    for _ in 0..count {
        // 每条至少 datetime(4) + 4×price(≥1) + vol(4) + amount(4)
        if pos + 16 > body.len() {
            break;
        }
        let (year, month, day, hour, minute, new_pos) = get_datetime(minute_level, body, pos);
        // 垃圾数据防御：日期非法则截断，保留已解析结果
        if !(1980..=2100).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day)
        {
            break;
        }
        pos = new_pos;

        let datetime = if minute_level {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
        } else {
            format!("{year:04}-{month:02}-{day:02}")
        };

        // 价格差分链：open 基于上一根 close，close/high/low 依次基于 open 累加（单位：厘）
        let (open_diff, p) = get_price(body, pos);
        pos = p;
        let open_raw = pre_diff_base + open_diff;
        let (close_diff, p) = get_price(body, pos);
        pos = p;
        let (high_diff, p) = get_price(body, pos);
        pos = p;
        let (low_diff, p) = get_price(body, pos);
        pos = p;

        let close_raw = open_raw + close_diff;
        pre_diff_base = close_raw;

        let vol = get_volume(read_u32(body, pos) as i64);
        pos += 4;
        let amount = get_volume(read_u32(body, pos) as i64);
        pos += 4;

        result.push(SecurityBar {
            datetime,
            open: open_raw as f64 / 1000.0,
            close: close_raw as f64 / 1000.0,
            high: (open_raw + high_diff) as f64 / 1000.0,
            low: (open_raw + low_diff) as f64 / 1000.0,
            vol,
            amount,
        });
    }
    Ok(result)
}

/// 解析五档快照（0x053E 响应体）。
pub fn parse_security_quotes(body: &[u8]) -> Result<Vec<Quote>> {
    if body.len() < 4 {
        return Err(err("quotes body too short"));
    }
    let mut pos = 2; // skip b1 cb
    let count = read_u16(body, pos) as usize;
    pos += 2;

    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        // 边界保护：每条记录至少 ~30 字节
        if pos + 30 > body.len() {
            break;
        }
        let market = body[pos];
        pos += 1;
        let code = String::from_utf8_lossy(&body[pos..pos + 6])
            .trim_end_matches('\0')
            .to_string();
        pos += 6;
        pos += 2; // active1
        let coefficient = price_coefficient(market, &code);

        let (price_raw, p) = get_price(body, pos);
        pos = p;
        let (last_close_diff, p) = get_price(body, pos);
        pos = p;
        let (open_diff, p) = get_price(body, pos);
        pos = p;
        let (high_diff, p) = get_price(body, pos);
        pos = p;
        let (low_diff, p) = get_price(body, pos);
        pos = p;
        let (servertime_raw, p) = get_price(body, pos);
        pos = p;
        let (_, p) = get_price(body, pos); // reversed1
        pos = p;
        let (vol, p) = get_price(body, pos);
        pos = p;
        let (cur_vol, p) = get_price(body, pos);
        pos = p;
        let amount = get_volume(read_u32(body, pos) as i64);
        pos += 4;
        let (s_vol, p) = get_price(body, pos);
        pos = p;
        let (b_vol, p) = get_price(body, pos);
        pos = p;
        let (_, p) = get_price(body, pos); // reversed2
        pos = p;
        let (_, p) = get_price(body, pos); // reversed3
        pos = p;

        let mut bid = [(0.0f64, 0.0f64); 5];
        let mut ask = [(0.0f64, 0.0f64); 5];
        for i in 0..5 {
            let (bid_diff, p) = get_price(body, pos);
            pos = p;
            let (ask_diff, p) = get_price(body, pos);
            pos = p;
            let (bid_vol, p) = get_price(body, pos);
            pos = p;
            let (ask_vol, p) = get_price(body, pos);
            pos = p;
            bid[i] = ((price_raw + bid_diff) as f64 * coefficient, bid_vol as f64);
            ask[i] = ((price_raw + ask_diff) as f64 * coefficient, ask_vol as f64);
        }

        pos += 2; // reversed4 (u16)
        for _ in 0..4 {
            let (_, p) = get_price(body, pos); // reversed5..8
            pos = p;
        }
        pos += 2; // reversed9 (i16)
        pos += 2; // active2 (u16)

        // servertime 形如 HHMMSSmmm 十进制整数
        let ts_str = servertime_raw.to_string();
        let servertime = if ts_str.len() >= 8 {
            let (hh, rest) = ts_str.split_at(ts_str.len() - 6);
            format!("{}:{}:{}", hh, &rest[..2], &rest[2..4])
        } else {
            ts_str
        };

        result.push(Quote {
            market,
            code,
            price: price_raw as f64 * coefficient,
            last_close: (price_raw + last_close_diff) as f64 * coefficient,
            open: (price_raw + open_diff) as f64 * coefficient,
            high: (price_raw + high_diff) as f64 * coefficient,
            low: (price_raw + low_diff) as f64 * coefficient,
            servertime,
            vol: vol as f64,
            cur_vol: cur_vol as f64,
            amount,
            s_vol: s_vol as f64,
            b_vol: b_vol as f64,
            bid,
            ask,
        });
    }
    Ok(result)
}

/// 分时索引 → 时间字符串（每天 240 点：09:31–11:30 / 13:01–15:00）。
pub fn minute_time_from_index(index: usize) -> String {
    let total = if index < 120 {
        9 * 60 + 31 + index
    } else {
        13 * 60 + 1 + (index - 120)
    };
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// 解析历史分时（0x0FB4 响应体；当日分时也走此接口）。
/// 返回按时间升序。
pub fn parse_history_minute_time(body: &[u8], market: u8, code: &str) -> Result<Vec<MinuteBar>> {
    let coefficient = price_coefficient(market, code);
    if body.len() < 6 {
        return Err(err("minute time body too short"));
    }
    let mut pos = 6; // skip 6 bytes header
    let mut result = Vec::new();
    let mut pre_diff_base: i64 = 0;
    let mut cum_amount = 0.0;
    let mut cum_vol = 0.0;

    while pos < body.len() {
        let old_pos = pos;
        let (price_diff, p) = get_price(body, pos);
        pos = p;
        pre_diff_base += price_diff;
        let price = pre_diff_base as f64 * coefficient;

        let (_, p) = get_price(body, pos); // reversed
        pos = p;
        let (vol_diff, p) = get_price(body, pos);
        pos = p;
        if pos == old_pos {
            break; // get_price 越界时 pos 不变，防死循环
        }
        let vol = vol_diff as f64;

        cum_amount += price * vol;
        cum_vol += vol;
        let avg_price = if cum_vol > 0.0 {
            cum_amount / cum_vol
        } else {
            price
        };

        result.push(MinuteBar {
            time: minute_time_from_index(result.len()),
            price,
            avg_price,
            vol,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minute_time_mapping() {
        assert_eq!(minute_time_from_index(0), "09:31");
        assert_eq!(minute_time_from_index(119), "11:30");
        assert_eq!(minute_time_from_index(120), "13:01");
        assert_eq!(minute_time_from_index(239), "15:00");
    }

    #[test]
    fn count_parse() {
        assert_eq!(parse_security_count(&[0x39, 0x15]).unwrap(), 0x1539);
        assert!(parse_security_count(&[0x01]).is_err());
    }

    #[test]
    fn empty_bodies() {
        assert!(parse_security_list(&[]).is_err());
        assert!(parse_security_bars(&[], false).is_err());
        assert!(parse_security_quotes(&[]).is_err());
        assert!(parse_history_minute_time(&[], 1, "600519").is_err());
        // count=0 的空列表合法
        assert!(parse_security_bars(&[0, 0], false).unwrap().is_empty());
        assert!(parse_security_quotes(&[0, 0, 0, 0]).unwrap().is_empty());
    }
}
