use std::str::Utf8Error;

use thiserror::Error;
use wstd::io::AsyncPollable;

use crate::{
    config::ImapConfig,
    wasi::{
        io::streams::StreamError,
        sockets::{
            instance_network::instance_network,
            ip_name_lookup::resolve_addresses,
            network::{IpAddress, IpAddressFamily, Ipv4SocketAddress, Ipv6SocketAddress},
            tcp::{ErrorCode, InputStream, Network, OutputStream, TcpSocket},
            tcp_create_socket::create_tcp_socket,
            udp::IpSocketAddress,
        },
        tls::types::ClientHandshake,
    },
};

// field order matters: https://github.com/bytecodealliance/wasmtime/issues/11804
pub struct ImapConnection {
    _pollable: AsyncPollable,
    _recv_stream: InputStream,
    _send_stream: OutputStream,
    _tls_connection: Option<crate::wasi::tls::types::ClientConnection>,
    _stream: Option<crate::wasi::tls::types::FutureClientStreams>,
    _sock: Option<TcpSocket>,
}

impl ImapConnection {
    pub async fn new(config: &ImapConfig) -> Result<Self> {
        println!("Connecting to imap server on {config}");

        // We need to use underlying primitives to split the TcpStream because
        // TcpStream.split() returns borrows to the halves and tls needs owned halves

        let addr = Address::new(config).await?;
        let sock = ConnectedSocket::new(&addr).await?;

        if config.tls {
            let TlsConnection {
                pollable,
                recv,
                send,
                connection,
                stream,
            } = TlsConnection::new(&config.host, sock).await?;

            Ok(Self {
                _pollable: pollable,
                _recv_stream: recv,
                _send_stream: send,
                _sock: None,
                _tls_connection: Some(connection),
                _stream: Some(stream),
            })
        } else {
            let ConnectedSocket {
                pollable,
                recv,
                send,
                sock,
            } = sock;

            Ok(Self {
                _pollable: pollable,
                _recv_stream: recv,
                _send_stream: send,
                _sock: Some(sock),
                _tls_connection: None,
                _stream: None,
            })
        }
    }
}

struct TlsConnection {
    // PROBABLY IMPORTANT (untested, but see sock below): the order of these fields matters for drop order!!
    // https://github.com/bytecodealliance/wasmtime/issues/11804
    pub pollable: AsyncPollable,
    pub recv: crate::wasi::tls::types::InputStream,
    pub send: crate::wasi::tls::types::OutputStream,
    pub connection: crate::wasi::tls::types::ClientConnection,
    pub stream: crate::wasi::tls::types::FutureClientStreams,
}

impl TlsConnection {
    pub async fn new(host: &str, sock: ConnectedSocket) -> Result<Self> {
        // bit of an odd syntax, but whatever
        let handshake = ClientHandshake::new(host, sock.recv, sock.send);
        let stream = ClientHandshake::finish(handshake);

        let pollable = AsyncPollable::new(stream.subscribe());

        // https://github.com/bytecodealliance/wasmtime/blob/7d413555c075b635d1a4f237cbcc22827366cb1d/crates/test-programs/src/bin/tls_sample_application.rs#L5
        let (connection, recv, send) = loop {
            // https://github.com/bytecodealliance/wasmtime/blob/7d413555c075b635d1a4f237cbcc22827366cb1d/crates/wasi-tls/src/host.rs#L116
            match stream.get() {
                None => {
                    pollable.wait_for().await;
                }
                Some(Ok(Ok(res))) => break res,
                Some(Ok(Err(e))) => {
                    return Err(ImapConnectionError::TlsHandshakeError(e.to_debug_string()));
                }
                Some(Err(_)) => {
                    return Err(ImapConnectionError::TlsHandshakeError(
                        "stream consumed".to_string(),
                    ));
                }
            }
        };

        Ok(Self {
            pollable,
            recv,
            send,
            connection,
            stream,
        })
    }
}

struct ConnectedSocket {
    // IMPORTANT: the order of these fields matters for drop order!!
    // https://github.com/bytecodealliance/wasmtime/issues/11804
    pub pollable: AsyncPollable,
    pub recv: crate::wasi::sockets::tcp::InputStream,
    pub send: crate::wasi::sockets::tcp::OutputStream,
    pub sock: TcpSocket,
}

impl ConnectedSocket {
    pub async fn new(addr: &Address) -> Result<Self> {
        let sock = create_tcp_socket(addr.family)?;

        // not sure...
        //sock.set_keep_alive_enabled(true)?;
        sock.start_connect(&addr.network, addr.sock_addr)?;

        let pollable = AsyncPollable::new(sock.subscribe());
        let (recv, send) = loop {
            match sock.finish_connect() {
                Ok(res) => break res,
                Err(ErrorCode::WouldBlock) => {
                    pollable.wait_for().await;
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        };

        Ok(Self {
            pollable,
            recv,
            send,
            sock,
        })
    }
}

struct Address {
    network: Network,
    family: IpAddressFamily,
    sock_addr: IpSocketAddress,
}

impl Address {
    pub async fn new(config: &ImapConfig) -> Result<Self> {
        let network = instance_network();

        let stream = resolve_addresses(&network, &config.host)?;

        let pollable = AsyncPollable::new(stream.subscribe());

        let ip_addr = loop {
            match stream.resolve_next_address() {
                Ok(Some(addr)) => break addr,
                Ok(None) => {
                    return Err(ImapConnectionError::NoAddress {
                        host: config.host.to_string(),
                    });
                }
                Err(ErrorCode::WouldBlock) => {
                    pollable.wait_for().await;
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        };

        let (family, sock_addr) = match ip_addr {
            IpAddress::Ipv4(addr) => (
                IpAddressFamily::Ipv4,
                IpSocketAddress::Ipv4(Ipv4SocketAddress {
                    address: addr,
                    port: config.port,
                }),
            ),
            IpAddress::Ipv6(addr) => (
                IpAddressFamily::Ipv6,
                IpSocketAddress::Ipv6(Ipv6SocketAddress {
                    address: addr,
                    port: config.port,
                    // TODO: handle these properly?
                    flow_info: 0,
                    scope_id: 0,
                }),
            ),
        };

        Ok(Self {
            network,
            family,
            sock_addr,
        })
    }
}

type Result<T> = std::result::Result<T, ImapConnectionError>;

#[derive(Error, Debug)]
pub enum ImapConnectionError {
    #[error("{0:?}")]
    Stream(#[from] StreamError),

    #[error("{0:?}")]
    Tcp(#[from] ErrorCode),

    #[error("Tls handshake error: {0}")]
    TlsHandshakeError(String),

    #[error("No address found for: {host}")]
    NoAddress { host: String },

    #[error("UTF8 parse: {0:?}")]
    Utf8(#[from] Utf8Error),
}

impl std::io::Read for ImapConnection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let data = self
            ._recv_stream
            .blocking_read(buf.len() as u64)
            .map_err(|e| std::io::Error::other(format!("Stream error: {:?}", e)))?;

        if data.is_empty() {
            return Ok(0);
        }

        buf[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }
}

impl std::io::Write for ImapConnection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let size = self
                ._send_stream
                .check_write()
                .map_err(|e| std::io::Error::other(format!("Stream error: {:?}", e)))?;
            let size = size.min(buf.len() as u64).try_into().unwrap();

            if size == 0 {
                self._send_stream.subscribe().block();
                continue;
            }

            self._send_stream
                .write(&buf[..size])
                .map_err(|e| std::io::Error::other(format!("Stream error: {:?}", e)))?;

            return Ok(size);
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self._send_stream
            .flush()
            .map_err(|e| std::io::Error::other(format!("Stream error: {:?}", e)))
    }
}
