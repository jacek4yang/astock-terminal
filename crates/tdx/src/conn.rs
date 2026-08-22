//! 单条异步 TCP 连接：握手、请求/响应、zlib 解压。

use std::io::Read;
use std::time::Duration;

use flate2::read::ZlibDecoder;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{Result, TdxError};
use crate::protocol::constants::{RESPONSE_HEADER_SIZE, SETUP_CMD1, SETUP_CMD2, SETUP_CMD3};
use crate::protocol::frame::ResponseHeader;
use crate::servers::Server;

/// 一条到某台 tdx 服务器的已握手连接。
pub struct Conn {
    stream: TcpStream,
    pub server: Server,
    timeout: Duration,
}

impl Conn {
    /// TCP 连接 + 三步握手。
    pub async fn connect(server: Server, timeout: Duration) -> Result<Self> {
        let addr = format!("{}:{}", server.ip, server.port);
        let stream = tokio::time::timeout(timeout, TcpStream::connect(&addr)).await??;
        stream.set_nodelay(true).ok();
        let mut conn = Self {
            stream,
            server,
            timeout,
        };
        conn.handshake().await?;
        Ok(conn)
    }

    /// 三步固定字节握手；每步读响应（可含 zlib 压缩体），内容忽略。
    async fn handshake(&mut self) -> Result<()> {
        for cmd in [SETUP_CMD1, SETUP_CMD2, SETUP_CMD3] {
            self.stream.write_all(cmd).await?;
            let _ = self.request_raw().await?;
        }
        Ok(())
    }

    /// 发送请求包，返回解压后的响应体。
    pub async fn request(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        self.stream.write_all(packet).await?;
        self.request_raw().await
    }

    /// 读 16 字节响应头 + 数据域；zip_size ≠ unzip_size 时 zlib 解压并校验长度。
    async fn request_raw(&mut self) -> Result<Vec<u8>> {
        let timeout = self.timeout;
        let mut head = [0u8; RESPONSE_HEADER_SIZE];
        tokio::time::timeout(timeout, self.stream.read_exact(&mut head)).await??;
        let header = ResponseHeader::parse(&head)?;
        let zip_size = header.zip_size as usize;
        let unzip_size = header.unzip_size as usize;

        let mut body = vec![0u8; zip_size];
        tokio::time::timeout(timeout, self.stream.read_exact(&mut body)).await??;

        if zip_size == unzip_size {
            return Ok(body);
        }
        let mut decoder = ZlibDecoder::new(&body[..]);
        let mut out = Vec::with_capacity(unzip_size);
        decoder
            .read_to_end(&mut out)
            .map_err(|e| TdxError::Protocol(format!("zlib decompress: {e}")))?;
        if out.len() != unzip_size {
            return Err(TdxError::Protocol(format!(
                "zlib length mismatch: expected {unzip_size}, got {}",
                out.len()
            )));
        }
        Ok(out)
    }
}
