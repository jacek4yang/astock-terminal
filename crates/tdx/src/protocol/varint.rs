//! 通达信特有变长编码：`getprice`（带符号变长整数）与 `getvolume`（4 字节类浮点）。
//!
//! 内化自 tdxrs `src/helpers.rs`（MIT，见 crate 根文档注释），
//! 与 pytdx `helper.py` 的 get_price / get_volume 字节级一致。

/// 变长有符号整数解码（pytdx `getprice`）。
///
/// 编码：首字节 bit7=续位、bit6=符号、低 6 位有效；后续字节各贡献 7 位，
/// 最高位置 0 的字节为终止字节。
///
/// 返回 `(value, new_pos)`；数据不足时返回 `(0, data.len())`，不 panic。
#[inline]
pub fn get_price(data: &[u8], pos: usize) -> (i64, usize) {
    if pos >= data.len() {
        return (0, data.len());
    }
    let mut pos = pos;
    let first = data[pos];
    let sign = (first & 0x40) != 0;
    let mut result = (first & 0x3F) as i64;
    let mut shift = 6;

    if (first & 0x80) != 0 {
        loop {
            pos += 1;
            if pos >= data.len() {
                return (0, data.len());
            }
            let b = data[pos];
            result |= ((b & 0x7F) as i64) << shift;
            shift += 7;
            if (b & 0x80) == 0 {
                break;
            }
        }
    }

    pos += 1;
    let val = if sign { -result } else { result };
    (val, pos)
}

/// 4 字节类浮点解码（pytdx `getvolume`），用于成交量/成交额/昨收。
#[inline]
pub fn get_volume(vol: i64) -> f64 {
    if vol == 0 {
        return 0.0;
    }
    let logpoint = vol >> (8 * 3);

    let hleax = (vol >> (8 * 2)) & 0xFF;
    let lheax = (vol >> 8) & 0xFF;
    let lleax = vol & 0xFF;

    let dw_ecx = logpoint * 2 - 0x7F;
    let dw_edx = logpoint * 2 - 0x86;
    let dw_esi = logpoint * 2 - 0x8E;
    let dw_eax = logpoint * 2 - 0x96;

    let tmp_eax = if dw_ecx < 0 { -dw_ecx } else { dw_ecx };
    let mut dbl_xmm6 = 2.0_f64.powi(tmp_eax as i32);
    if dw_ecx < 0 {
        dbl_xmm6 = 1.0 / dbl_xmm6;
    }

    let dbl_xmm4 = if hleax > 0x80 {
        let dwtmpeax = dw_edx + 1;
        let tmpdbl_xmm3 = 2.0_f64.powi(dwtmpeax as i32);
        2.0_f64.powi(dw_edx as i32) * 128.0 + (hleax & 0x7F) as f64 * tmpdbl_xmm3
    } else if dw_edx >= 0 {
        2.0_f64.powi(dw_edx as i32) * hleax as f64
    } else {
        (1.0 / 2.0_f64.powi((-dw_edx) as i32)) * hleax as f64
    };

    let mut dbl_xmm3 = 2.0_f64.powi(dw_esi as i32) * lheax as f64;
    let mut dbl_xmm1 = 2.0_f64.powi(dw_eax as i32) * lleax as f64;

    if (hleax & 0x80) != 0 {
        dbl_xmm3 *= 2.0;
        dbl_xmm1 *= 2.0;
    }

    dbl_xmm6 + dbl_xmm4 + dbl_xmm3 + dbl_xmm1
}

/// 按 getprice 编码规则把 `val` 编码为字节流（测试专用，用于往返性质测试）。
#[cfg(test)]
fn encode_price(val: i64, out: &mut Vec<u8>) {
    let sign = val < 0;
    let mut mag = if sign { -val } else { val } as u64;
    let mut first = (mag & 0x3F) as u8;
    mag >>= 6;
    if sign {
        first |= 0x40;
    }
    if mag > 0 {
        first |= 0x80;
    }
    out.push(first);
    while mag > 0 {
        let mut b = (mag & 0x7F) as u8;
        mag >>= 7;
        if mag > 0 {
            b |= 0x80;
        }
        out.push(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- get_price golden（与 tdxrs/pytdx 单测同例） ---

    #[test]
    fn price_single_byte_positive() {
        assert_eq!(get_price(&[0x02], 0), (2, 1));
    }

    #[test]
    fn price_single_byte_negative() {
        assert_eq!(get_price(&[0x42], 0), (-2, 1));
    }

    #[test]
    fn price_multi_byte() {
        assert_eq!(get_price(&[0x81, 0x01], 0), (65, 2));
        assert_eq!(get_price(&[0x81, 0x81, 0x01], 0), (8257, 3));
    }

    #[test]
    fn price_zero_and_bounds() {
        assert_eq!(get_price(&[0x00], 0), (0, 1));
        assert_eq!(get_price(&[0x3F], 0), (63, 1));
        assert_eq!(get_price(&[0x7F], 0), (-63, 1));
        assert_eq!(get_price(&[], 0), (0, 0));
        assert_eq!(get_price(&[0xFF, 0x02, 0x03], 1), (2, 2));
    }

    // --- get_volume golden ---

    #[test]
    fn volume_zero() {
        assert_eq!(get_volume(0), 0.0);
    }

    #[test]
    fn volume_simple_values() {
        // 0x00_00_01_00: logpoint=0, lheax=1 → 非负且不 panic
        assert!(get_volume(0x00_00_01_00) >= 0.0);
        // 真实响应字节的 golden 校验见 tests/protocol_fixtures.rs
    }

    // --- 往返性质测试 ---

    proptest! {
        #[test]
        fn price_roundtrip(val in -50_000_000i64..50_000_000i64) {
            let mut buf = Vec::new();
            encode_price(val, &mut buf);
            let (decoded, pos) = get_price(&buf, 0);
            prop_assert_eq!(decoded, val);
            prop_assert_eq!(pos, buf.len());
        }
    }
}
