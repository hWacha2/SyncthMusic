//! Транспорт для обмена сообщениями между ПК и агентом.
//!
//! Протокол не привязан к TCP: UI и логика работают с [`Transport`], а TCP —
//! только одна реализация. Позже сюда же добавляется ADB-транспорт.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

/// Верхняя граница одного кадра. Защищает от случайного огромного сообщения.
pub const MAX_FRAME: usize = 256 * 1024 * 1024;

/// Двусторонний канал сообщений.
pub trait Transport: Send {
    /// Адрес или идентификатор второй стороны для журнала и UI.
    fn peer(&self) -> String;

    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()>;

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> Result<()>;

    /// Читает один кадр целиком.
    fn read_frame(&mut self) -> Result<Vec<u8>>;

    /// Отправляет один кадр целиком.
    fn write_frame(&mut self, body: &[u8]) -> Result<()>;

    fn shutdown(&mut self);
}

impl Transport for TcpStream {
    fn peer(&self) -> String {
        self.peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default()
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        TcpStream::set_read_timeout(self, timeout)?;
        Ok(())
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        TcpStream::set_write_timeout(self, timeout)?;
        Ok(())
    }

    fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut len = [0u8; 4];
        self.read_exact(&mut len)
            .context("соединение закрылось до начала сообщения")?;
        let n = u32::from_be_bytes(len) as usize;
        anyhow::ensure!(n <= MAX_FRAME, "сообщение слишком большое: {n} байт");
        let mut body = vec![0u8; n];
        self.read_exact(&mut body)?;
        Ok(body)
    }

    fn write_frame(&mut self, body: &[u8]) -> Result<()> {
        anyhow::ensure!(
            body.len() <= MAX_FRAME,
            "сообщение слишком большое: {} байт",
            body.len()
        );
        self.write_all(&(body.len() as u32).to_be_bytes())?;
        self.write_all(body)?;
        self.flush()?;
        Ok(())
    }

    fn shutdown(&mut self) {
        let _ = TcpStream::shutdown(self, Shutdown::Both);
    }
}

/// Отправляет JSON-сообщение в любом транспорте.
pub fn send_json<T: Serialize + ?Sized>(transport: &mut dyn Transport, message: &T) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    transport.write_frame(&body)
}

/// Читает JSON-сообщение из любого транспорта.
pub fn recv_json<T: DeserializeOwned>(transport: &mut dyn Transport) -> Result<T> {
    let body = transport.read_frame()?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::net::TcpListener;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Ping {
        value: u32,
        text: String,
    }

    #[test]
    fn tcp_transport_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let ping: Ping = recv_json(&mut stream).unwrap();
            assert_eq!(ping.value, 7);
            send_json(
                &mut stream,
                &Ping {
                    value: ping.value + 1,
                    text: "pong".into(),
                },
            )
            .unwrap();
        });

        let mut client = TcpStream::connect(addr).unwrap();
        send_json(
            &mut client,
            &Ping {
                value: 7,
                text: "ping".into(),
            },
        )
        .unwrap();
        let pong: Ping = recv_json(&mut client).unwrap();
        assert_eq!(
            pong,
            Ping {
                value: 8,
                text: "pong".into()
            }
        );
        server.join().unwrap();
    }
}
