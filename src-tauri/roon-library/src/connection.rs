// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Protocol details from arthursoares/roon-api-reverse-engineering (MIT), see NOTICE.

//! The TCP connection to the Core and the handshake that turns it into a
//! remoting session:
//!
//! ```text
//! 1. TCP connect to <core>:9332
//! 2. -> "ROON" 01 04  <server broker id, 16 bytes>  <our random client id, 16 bytes>
//! 3. <- "ROON" 01 80  (ok)   or  "ROON" 01 81 (wrong Core id) and the Core hangs up
//! 4. -> "ROON" 01 02
//! 5. <- "ROON" 01 82  <session id, 16 bytes>
//! 6. -> ConnectRequest (the frame the official client sends, with our client id in it)
//! 7. <- ConnectResponse; from here on everything is remoting frames
//! ```
//!
//! The server broker id is the Core id every Roon client already knows (SOOD
//! discovery's `unique_id`, the extension API's `core_id`) in .NET Guid byte
//! order. There is no authentication on the local network.

use std::fmt;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const DEFAULT_PORT: u16 = 9332;

const MAGIC: &[u8; 4] = b"ROON";

/// The ConnectRequest the Roon desktop client (protocol version 28, production
/// branch) sends after the handshake, captured by the upstream project. Bytes
/// 72..88 hold the client broker id and are replaced with ours.
const CONNECT_REQUEST_TEMPLATE: &str = "470181670000000100012c536f6f6c6f6f732e4d73672e446973747269627574656442726f6b65722e436f6e6e6563745265717565737424840e436c69656e7442726f6b6572496400000000000000000000000000000000228110436c69656e7442726f6b65724e616d650000000848512d30303435321b810f50726f746f636f6c56657273696f6e0000000232383e810c50726f746f636f6c48617368000000286161656464323265326536653435323233316537346464333039666662396432376139373531656420810c436c69656e744272616e63680000000a70726f64756374696f6e05030503";
const CLIENT_ID_OFFSET: usize = 72;

#[derive(Debug)]
pub enum ConnectError {
    /// The Core turned us down: wrong Core id, or a Roon version whose protocol we don't speak.
    Unsupported(String),
    Io(std::io::Error),
    Timeout,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Unsupported(m) => write!(f, "the Core rejected the connection: {m}"),
            ConnectError::Io(e) => write!(f, "network error: {e}"),
            ConnectError::Timeout => f.write_str("the Core did not finish the handshake in time"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<std::io::Error> for ConnectError {
    fn from(e: std::io::Error) -> Self {
        ConnectError::Io(e)
    }
}

/// The 16 bytes the handshake wants for a Core id such as
/// "fafb763c-9ad0-4f07-887d-44e19b8374e0": .NET's Guid.ToByteArray() order,
/// i.e. the first three groups byte-reversed and the last two as written.
pub fn broker_id_from_core_id(core_id: &str) -> Option<[u8; 16]> {
    let hex: String = core_id.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut b = [0u8; 16];
    for (i, slot) in b.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    b[0..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    Some(b)
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("template is valid hex"))
        .collect()
}

/// The result of a successful handshake: the open socket, and the first
/// remoting bytes that arrived with the ConnectResponse (feed them to the
/// frame parser before anything else read from the socket).
pub struct Handshake {
    pub stream: TcpStream,
    pub initial: Vec<u8>,
    pub client_broker_id: [u8; 16],
}

/// TCP connect + handshake + ConnectRequest, all within `timeout`.
pub async fn connect(host: &str, port: u16, server_broker_id: [u8; 16], timeout: Duration) -> Result<Handshake, ConnectError> {
    tokio::time::timeout(timeout, connect_inner(host, port, server_broker_id))
        .await
        .map_err(|_| ConnectError::Timeout)?
}

async fn connect_inner(host: &str, port: u16, server_broker_id: [u8; 16]) -> Result<Handshake, ConnectError> {
    let mut stream = TcpStream::connect((host, port)).await?;
    stream.set_nodelay(true)?;
    let client_broker_id = random_client_id();

    // 2. hello
    let mut hello = Vec::with_capacity(38);
    hello.extend_from_slice(MAGIC);
    hello.extend_from_slice(&[0x01, 0x04]);
    hello.extend_from_slice(&server_broker_id);
    hello.extend_from_slice(&client_broker_id);
    stream.write_all(&hello).await?;

    // 3. ack
    let mut ack = [0u8; 6];
    read_exact_or_reject(&mut stream, &mut ack, "our hello (wrong Core id?)").await?;
    if &ack[0..4] != MAGIC {
        return Err(ConnectError::Unsupported("unexpected reply to our hello".into()));
    }
    match ack[5] {
        0x80 => {}
        0x81 => return Err(ConnectError::Unsupported("the Core id does not match".into())),
        other => return Err(ConnectError::Unsupported(format!("unexpected hello reply {:02x}{:02x}", ack[4], other))),
    }

    // 4./5. session
    stream.write_all(&[b'R', b'O', b'O', b'N', 0x01, 0x02]).await?;
    let mut session = [0u8; 22];
    read_exact_or_reject(&mut stream, &mut session, "the session request").await?;
    if &session[0..4] != MAGIC || session[5] != 0x82 {
        return Err(ConnectError::Unsupported(format!("unexpected session reply {:02x}{:02x}", session[4], session[5])));
    }

    // 6. ConnectRequest with our client id patched in
    let mut request = hex_to_bytes(CONNECT_REQUEST_TEMPLATE);
    request[CLIENT_ID_OFFSET..CLIENT_ID_OFFSET + 16].copy_from_slice(&client_broker_id);
    stream.write_all(&request).await?;

    // 7. the first remoting bytes (ConnectResponse) mean we're in
    let mut initial = vec![0u8; 4096];
    let n = stream.read(&mut initial).await?;
    if n == 0 {
        return Err(ConnectError::Unsupported(
            "the Core closed the connection after our connection request (unsupported Roon version?)".into(),
        ));
    }
    initial.truncate(n);
    if initial.starts_with(MAGIC) {
        return Err(ConnectError::Unsupported("unexpected handshake bytes after the connection request".into()));
    }
    Ok(Handshake { stream, initial, client_broker_id })
}

/// 16 random-enough bytes for our client broker id (it only has to be unique
/// among the Core's current sessions), from the standard library's randomly
/// seeded hasher plus the clock.
fn random_client_id() -> [u8; 16] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(nanos ^ (i as u64) ^ ((std::process::id() as u64) << 32));
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

async fn read_exact_or_reject(stream: &mut TcpStream, buf: &mut [u8], what: &str) -> Result<(), ConnectError> {
    match stream.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(ConnectError::Unsupported(format!(
            "the Core hung up on {what}"
        ))),
        Err(e) => Err(ConnectError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_id_is_the_core_id_in_dotnet_order() {
        let id = broker_id_from_core_id("fafb763c-9ad0-4f07-887d-44e19b8374e0").unwrap();
        let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "3c76fbfad09a074f887d44e19b8374e0");
        assert_eq!(broker_id_from_core_id("FAFB763C9AD04F07887D44E19B8374E0"), Some(id));
        assert_eq!(broker_id_from_core_id("not-a-guid"), None);
    }

    #[test]
    fn template_has_the_client_id_slot_where_we_think() {
        let bytes = hex_to_bytes(CONNECT_REQUEST_TEMPLATE);
        assert_eq!(bytes.len(), 235);
        // "ClientBrokerId" ends right before the slot
        assert_eq!(&bytes[CLIENT_ID_OFFSET - 14..CLIENT_ID_OFFSET], b"ClientBrokerId");
        assert!(bytes[CLIENT_ID_OFFSET..CLIENT_ID_OFFSET + 16].iter().all(|b| *b == 0));
        // and "ClientBrokerName" follows the slot's length prefix
        assert_eq!(&bytes[CLIENT_ID_OFFSET + 16 + 3..CLIENT_ID_OFFSET + 16 + 3 + 16], b"ClientBrokerName");
    }
}
