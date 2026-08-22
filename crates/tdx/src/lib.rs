//! astock-tdx — 通达信行情 TCP 协议客户端（纯 Rust / tokio）。
//!
//! 协议编解码内化自 tdxrs (<https://github.com/jiangtaovan/tdxrs>)，
//! 其许可证为 MIT：
//!
//! ```text
//! MIT License
//! Copyright (c) 2026 Chiang Tao
//! Copyright (c) 2026 tdxrs Contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//! ```
//!
//! 服务器候选列表另合并了 injoyai/tdx (MIT) 的实测 IP。
//!
//! # 能力
//!
//! - 证券列表 (0x0450) / 证券数量 (0x044E)
//! - K 线 (0x052D，12 种周期，单次 ≤800 条，自动分页)
//! - 五档快照 (0x053E，单次 ≤60 只)
//! - 分时 (绕开 0x051D 已知价格编码 bug，走 0x0FB4 历史分时接口)
//!
//! 服务器只返回**未复权**原始数据（fq 字段为协议保留位），本 crate 不做复权。

pub mod client;
pub mod conn;
pub mod error;
pub mod pool;
pub mod protocol;
pub mod servers;

pub use client::TdxClient;
pub use error::{Result, TdxError};
pub use pool::{PoolConfig, ProbeResult, ServerPool};
pub use protocol::types::{KlineCategory, MinuteBar, Quote, SecurityBar, SecurityInfo};
pub use servers::Server;
