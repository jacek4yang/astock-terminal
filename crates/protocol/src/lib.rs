//! Versioned language-neutral contracts and length-prefixed JSON framing.

mod generated;

pub use generated::*;

use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("frame is empty")]
    Empty,
    #[error("frame length {actual} exceeds the {maximum} byte limit")]
    Oversized { actual: usize, maximum: usize },
    #[error("malformed JSON frame: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut header = [0_u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            actual: length,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let body = serde_json::to_vec(value)?;
    if body.is_empty() {
        return Err(FrameError::Empty);
    }
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            actual: body.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    writer.write_all(&(body.len() as u32).to_le_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

impl ResponseEnvelope {
    pub fn success(request: &RequestEnvelope, payload: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            kind: request.kind.clone(),
            ok: true,
            payload,
            error: None,
        }
    }

    pub fn failure(
        request: &RequestEnvelope,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            kind: request.kind.clone(),
            ok: false,
            payload: serde_json::Value::Null,
            error: Some(ErrorBody {
                code: code.into(),
                message: message.into(),
                retryable,
                details: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn frame_round_trip() {
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "req-1".into(),
            kind: "system.handshake".into(),
            payload: json!({"client": "test"}),
            deadline_ms: Some(1_000),
            cancellation_id: None,
        };
        let (mut client, mut server) = tokio::io::duplex(4_096);
        let write = tokio::spawn(async move { write_frame(&mut client, &request).await });
        let decoded: RequestEnvelope = read_frame(&mut server).await.unwrap().unwrap();
        write.await.unwrap().unwrap();
        assert_eq!(decoded.request_id, "req-1");
        assert_eq!(decoded.payload, json!({"client": "test"}));
    }

    #[tokio::test]
    async fn rejects_oversized_header_before_allocation() {
        let (mut client, mut server) = tokio::io::duplex(16);
        let write = tokio::spawn(async move {
            client
                .write_all(&((MAX_FRAME_BYTES as u32) + 1).to_le_bytes())
                .await
                .unwrap();
        });
        let error = read_frame::<_, serde_json::Value>(&mut server)
            .await
            .unwrap_err();
        write.await.unwrap();
        assert!(matches!(error, FrameError::Oversized { .. }));
    }

    #[test]
    fn all_declared_engine_kinds_are_namespaced() {
        assert!(ENGINE_REQUEST_KINDS.iter().all(|kind| kind.contains('.')));
        assert!(ENGINE_REQUEST_KINDS.contains(&"system.handshake"));
    }
}
