use std::io::{self, Read, Write};

use anyhow::{Context, Result, bail};

/// Header: 1 byte type + 4 bytes LE payload length.
pub const HEADER_SIZE: usize = 5;
/// Safety cap: no single frame larger than 64 MB.
pub const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    // Client → Daemon
    Hello = 1,
    CreateOrAttach = 2,
    Write = 3,
    Resize = 4,
    Kill = 5,
    Detach = 6,
    ListSessions = 7,
    Signal = 8,
    Shutdown = 9,
    Ping = 10,
    GetResourceSnapshot = 11,
    WorkspaceRegistered = 12,
    WorkspaceUnregistered = 13,
    WorkspaceFocused = 14,
    RefreshPr = 15,
    EnsureSession = 16,

    // Daemon → Client
    HelloAck = 101,
    SessionAttached = 102,
    Data = 103,
    Exit = 104,
    Error = 105,
    SessionList = 106,
    Pong = 107,
    ResourceSnapshot = 108,
    ResizeAck = 109,
    PrStatusUpdated = 110,
    PrStatusUnavailable = 111,
    SessionEnsured = 112,
}

impl MessageType {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Hello),
            2 => Some(Self::CreateOrAttach),
            3 => Some(Self::Write),
            4 => Some(Self::Resize),
            5 => Some(Self::Kill),
            6 => Some(Self::Detach),
            7 => Some(Self::ListSessions),
            8 => Some(Self::Signal),
            9 => Some(Self::Shutdown),
            10 => Some(Self::Ping),
            11 => Some(Self::GetResourceSnapshot),
            12 => Some(Self::WorkspaceRegistered),
            13 => Some(Self::WorkspaceUnregistered),
            14 => Some(Self::WorkspaceFocused),
            15 => Some(Self::RefreshPr),
            16 => Some(Self::EnsureSession),
            101 => Some(Self::HelloAck),
            102 => Some(Self::SessionAttached),
            103 => Some(Self::Data),
            104 => Some(Self::Exit),
            105 => Some(Self::Error),
            106 => Some(Self::SessionList),
            107 => Some(Self::Pong),
            108 => Some(Self::ResourceSnapshot),
            109 => Some(Self::ResizeAck),
            110 => Some(Self::PrStatusUpdated),
            111 => Some(Self::PrStatusUnavailable),
            112 => Some(Self::SessionEnsured),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub message_type: MessageType,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(message_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            message_type,
            payload,
        }
    }

    /// Encode a serde-serializable value as a MessagePack frame.
    pub fn from_msg<T: serde::Serialize>(message_type: MessageType, msg: &T) -> Result<Self> {
        let payload = rmp_serde::to_vec_named(msg).context("failed to serialize message")?;
        Ok(Self::new(message_type, payload))
    }

    /// Decode the payload as a MessagePack message.
    pub fn decode_msg<'a, T: serde::Deserialize<'a>>(&'a self) -> Result<T> {
        rmp_serde::from_slice(&self.payload).context("failed to deserialize message")
    }

    /// Encode the frame into wire format: [type:1][len:4 LE][payload].
    pub fn encode(&self, writer: &mut impl Write) -> io::Result<()> {
        if self.payload.len() > MAX_FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame payload too large: {} bytes (max {})",
                    self.payload.len(),
                    MAX_FRAME_SIZE
                ),
            ));
        }
        let header = encode_header(self.message_type, self.payload.len() as u32);
        writer.write_all(&header)?;
        writer.write_all(&self.payload)?;
        Ok(())
    }

    /// Read a single frame from a reader. Blocks until a full frame is available.
    pub fn decode(reader: &mut impl Read) -> Result<Self> {
        let mut header_buf = [0u8; HEADER_SIZE];
        reader
            .read_exact(&mut header_buf)
            .context("failed to read frame header")?;

        let (message_type, payload_len) = decode_header(&header_buf)?;
        let payload_len = payload_len as usize;

        if payload_len > MAX_FRAME_SIZE {
            bail!(
                "frame payload too large: {} bytes (max {})",
                payload_len,
                MAX_FRAME_SIZE
            );
        }

        let mut payload = vec![0u8; payload_len];
        reader
            .read_exact(&mut payload)
            .context("failed to read frame payload")?;

        Ok(Self {
            message_type,
            payload,
        })
    }
}

fn encode_header(msg_type: MessageType, payload_len: u32) -> [u8; HEADER_SIZE] {
    let mut buf = [0u8; HEADER_SIZE];
    buf[0] = msg_type as u8;
    buf[1..5].copy_from_slice(&payload_len.to_le_bytes());
    buf
}

fn decode_header(buf: &[u8; HEADER_SIZE]) -> Result<(MessageType, u32)> {
    let msg_type =
        MessageType::from_u8(buf[0]).context(format!("unknown message type: {}", buf[0]))?;
    let payload_len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
    Ok((msg_type, payload_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_header() {
        for original_type in [
            MessageType::CreateOrAttach,
            MessageType::EnsureSession,
            MessageType::SessionAttached,
            MessageType::SessionEnsured,
        ] {
            let original_len: u32 = 12345;
            let encoded = encode_header(original_type, original_len);
            let (decoded_type, decoded_len) = decode_header(&encoded).unwrap();
            assert_eq!(decoded_type, original_type);
            assert_eq!(decoded_len, original_len);
        }
    }

    #[test]
    fn round_trip_frame() {
        let payload = b"hello world".to_vec();
        let frame = Frame::new(MessageType::Data, payload.clone());

        let mut buf = Vec::new();
        frame.encode(&mut buf).unwrap();

        let decoded = Frame::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(decoded.message_type, MessageType::Data);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn round_trip_msgpack_frame() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct TestMsg {
            name: String,
            value: u32,
        }

        let msg = TestMsg {
            name: "test".into(),
            value: 42,
        };

        let frame = Frame::from_msg(MessageType::Hello, &msg).unwrap();

        let mut buf = Vec::new();
        frame.encode(&mut buf).unwrap();

        let decoded_frame = Frame::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(decoded_frame.message_type, MessageType::Hello);

        let decoded_msg: TestMsg = decoded_frame.decode_msg().unwrap();
        assert_eq!(decoded_msg, msg);
    }

    #[test]
    fn ensure_session_messages_round_trip() {
        use std::path::PathBuf;

        use crate::messages::{EnsureSessionMsg, SessionEnsuredMsg};

        let session_id = uuid::Uuid::new_v4();
        let workspace_id = uuid::Uuid::new_v4();
        let ensure = EnsureSessionMsg {
            session_id,
            workspace_id,
            cols: 100,
            rows: 40,
            cwd: Some(PathBuf::from("/tmp/seoul")),
            shell: Some("/bin/zsh".into()),
        };
        let frame = Frame::from_msg(MessageType::EnsureSession, &ensure).unwrap();
        let decoded: EnsureSessionMsg = frame.decode_msg().unwrap();
        assert_eq!(decoded.session_id, session_id);
        assert_eq!(decoded.workspace_id, workspace_id);
        assert_eq!(decoded.cols, 100);
        assert_eq!(decoded.rows, 40);
        assert_eq!(decoded.cwd, Some(PathBuf::from("/tmp/seoul")));
        assert_eq!(decoded.shell, Some("/bin/zsh".into()));

        let ensured = SessionEnsuredMsg {
            session_id,
            is_new: true,
            was_recovered: false,
            cols: 100,
            rows: 40,
            cwd: Some("/tmp/seoul".into()),
            foreground_process: Some("zsh".into()),
        };
        let frame = Frame::from_msg(MessageType::SessionEnsured, &ensured).unwrap();
        let decoded: SessionEnsuredMsg = frame.decode_msg().unwrap();
        assert_eq!(decoded.session_id, session_id);
        assert!(decoded.is_new);
        assert!(!decoded.was_recovered);
        assert_eq!(decoded.cols, 100);
        assert_eq!(decoded.rows, 40);
        assert_eq!(decoded.cwd.as_deref(), Some("/tmp/seoul"));
        assert_eq!(decoded.foreground_process.as_deref(), Some("zsh"));
    }

    #[test]
    fn create_or_attach_preserves_optional_scrollback_limit() {
        use crate::messages::CreateOrAttachMsg;

        let session_id = uuid::Uuid::new_v4();
        let workspace_id = uuid::Uuid::new_v4();
        let msg = CreateOrAttachMsg {
            session_id,
            workspace_id,
            cols: 120,
            rows: 50,
            cwd: None,
            shell: None,
            scrollback_limit_bytes: Some(128 * 1024),
        };

        let frame = Frame::from_msg(MessageType::CreateOrAttach, &msg).unwrap();
        let decoded: CreateOrAttachMsg = frame.decode_msg().unwrap();
        assert_eq!(decoded.session_id, session_id);
        assert_eq!(decoded.workspace_id, workspace_id);
        assert_eq!(decoded.scrollback_limit_bytes, Some(128 * 1024));
    }

    #[test]
    fn rejects_unknown_message_type() {
        let buf = [255u8, 0, 0, 0, 0];
        assert!(decode_header(&buf).is_err());
    }

    #[test]
    fn rejects_oversized_frame() {
        let huge_len = (MAX_FRAME_SIZE as u32) + 1;
        let mut header = [0u8; HEADER_SIZE];
        header[0] = MessageType::Data as u8;
        header[1..5].copy_from_slice(&huge_len.to_le_bytes());

        let mut reader = std::io::Cursor::new(header);
        // Will fail because it tries to read the too-large payload
        assert!(Frame::decode(&mut reader).is_err());
    }
}
