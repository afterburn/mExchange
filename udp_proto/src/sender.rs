use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender as ChannelSender, TrySendError};
use socket2::{Domain, Protocol, Socket, Type};

use crate::error::{ProtocolError, Result};
use crate::protocol::*;

/// Create a UDP socket with SO_REUSEADDR set (allows quick restarts)
fn create_udp_socket_with_reuse(bind_addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let domain = if bind_addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;

    // On Unix-like systems, also set SO_REUSEPORT for better behavior
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    {
        socket.set_reuse_port(true)?;
    }

    socket.bind(&bind_addr.into())?;
    Ok(socket.into())
}

/// Configuration for the UDP sender
#[derive(Debug, Clone)]
pub struct SenderConfig {
    /// Stream ID for this sender
    pub stream_id: u32,
    /// Target address to send packets to
    pub target_addr: SocketAddr,
    /// Maximum time to wait before sending a partial batch
    pub max_batch_delay: Duration,
    /// Channel capacity for outgoing messages
    pub channel_capacity: usize,
    /// Enable heartbeats
    pub enable_heartbeats: bool,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            stream_id: 0,
            target_addr: "127.0.0.1:9000".parse().unwrap(),
            max_batch_delay: Duration::from_micros(100),
            channel_capacity: 10_000,
            enable_heartbeats: true,
        }
    }
}

/// Outgoing message to be sent
#[derive(Debug)]
pub struct OutgoingMessage {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
}

/// Statistics for the sender
#[derive(Debug, Default)]
pub struct SenderStats {
    pub packets_sent: AtomicU64,
    pub messages_sent: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub heartbeats_sent: AtomicU64,
    pub send_errors: AtomicU64,
}

impl SenderStats {
    pub fn snapshot(&self) -> SenderStatsSnapshot {
        SenderStatsSnapshot {
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            heartbeats_sent: self.heartbeats_sent.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SenderStatsSnapshot {
    pub packets_sent: u64,
    pub messages_sent: u64,
    pub bytes_sent: u64,
    pub heartbeats_sent: u64,
    pub send_errors: u64,
}

/// UDP sender with batching and automatic heartbeats
pub struct UdpSender {
    #[allow(dead_code)]
    config: SenderConfig,
    tx: ChannelSender<OutgoingMessage>,
    stats: Arc<SenderStats>,
    running: Arc<AtomicBool>,
    worker_handle: Option<JoinHandle<()>>,
}

impl UdpSender {
    /// Create a new sender bound to the specified local address
    pub fn new(config: SenderConfig, bind_addr: SocketAddr) -> Result<Self> {
        let socket = create_udp_socket_with_reuse(bind_addr)?;
        socket.set_nonblocking(false)?;

        let (tx, rx) = bounded(config.channel_capacity);
        let stats = Arc::new(SenderStats::default());
        let running = Arc::new(AtomicBool::new(true));

        let worker = SenderWorker {
            config: config.clone(),
            socket,
            rx,
            stats: stats.clone(),
            running: running.clone(),
            packet_seq: 0,
            msg_seq: 0,
        };

        let handle = thread::Builder::new()
            .name(format!("udp-sender-{}", config.stream_id))
            .spawn(move || worker.run())
            .map_err(|e| ProtocolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        Ok(Self {
            config,
            tx,
            stats,
            running,
            worker_handle: Some(handle),
        })
    }

    /// Send a message (non-blocking, may fail if channel is full)
    pub fn try_send(&self, msg_type: MessageType, payload: Vec<u8>) -> Result<()> {
        if payload.len() > MAX_PAYLOAD - MESSAGE_HEADER_LEN {
            return Err(ProtocolError::MessageTooLarge {
                size: payload.len(),
                max: MAX_PAYLOAD - MESSAGE_HEADER_LEN,
            });
        }

        self.tx
            .try_send(OutgoingMessage { msg_type, payload })
            .map_err(|e| match e {
                TrySendError::Full(_) => ProtocolError::ChannelClosed,
                TrySendError::Disconnected(_) => ProtocolError::ChannelClosed,
            })
    }

    /// Send a message (blocking)
    pub fn send(&self, msg_type: MessageType, payload: Vec<u8>) -> Result<()> {
        if payload.len() > MAX_PAYLOAD - MESSAGE_HEADER_LEN {
            return Err(ProtocolError::MessageTooLarge {
                size: payload.len(),
                max: MAX_PAYLOAD - MESSAGE_HEADER_LEN,
            });
        }

        self.tx
            .send(OutgoingMessage { msg_type, payload })
            .map_err(|_| ProtocolError::ChannelClosed)
    }

    /// Get current statistics
    pub fn stats(&self) -> SenderStatsSnapshot {
        self.stats.snapshot()
    }

    /// Check if the sender is running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Shutdown the sender
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for UdpSender {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SenderWorker {
    config: SenderConfig,
    socket: UdpSocket,
    rx: Receiver<OutgoingMessage>,
    stats: Arc<SenderStats>,
    running: Arc<AtomicBool>,
    packet_seq: u64,
    msg_seq: u64,
}

impl SenderWorker {
    fn run(mut self) {
        let mut last_send = Instant::now();
        let mut builder = PacketBuilder::new(self.config.stream_id, self.packet_seq, self.msg_seq);

        while self.running.load(Ordering::Relaxed) {
            // Try to receive with timeout
            match self.rx.recv_timeout(self.config.max_batch_delay) {
                Ok(msg) => {
                    // Try to add to current batch
                    if !builder.try_add_message(msg.msg_type, &msg.payload) {
                        // Batch full, send it
                        let msg_count = builder.msg_count();
                        self.send_packet(&builder.finish());
                        self.packet_seq += 1;
                        self.msg_seq += msg_count as u64;

                        // Start new batch
                        builder = PacketBuilder::new(
                            self.config.stream_id,
                            self.packet_seq,
                            self.msg_seq,
                        );
                        builder.try_add_message(msg.msg_type, &msg.payload);
                    }
                    last_send = Instant::now();
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // Timeout - send partial batch or heartbeat
                    if !builder.is_empty() {
                        let msg_count = builder.msg_count();
                        self.send_packet(&builder.finish());
                        self.packet_seq += 1;
                        self.msg_seq += msg_count as u64;
                        builder = PacketBuilder::new(
                            self.config.stream_id,
                            self.packet_seq,
                            self.msg_seq,
                        );
                        last_send = Instant::now();
                    } else if self.config.enable_heartbeats
                        && last_send.elapsed() >= Duration::from_millis(HEARTBEAT_INTERVAL_MS)
                    {
                        // Send heartbeat
                        let heartbeat =
                            PacketBuilder::heartbeat(self.config.stream_id, self.packet_seq, self.msg_seq);
                        self.send_heartbeat(&heartbeat);
                        self.packet_seq += 1;
                        last_send = Instant::now();
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        // Drain any remaining messages from the channel
        while let Ok(msg) = self.rx.try_recv() {
            if !builder.try_add_message(msg.msg_type, &msg.payload) {
                // Batch full, send it
                let msg_count = builder.msg_count();
                self.send_packet(&builder.finish());
                self.packet_seq += 1;
                self.msg_seq += msg_count as u64;
                builder = PacketBuilder::new(
                    self.config.stream_id,
                    self.packet_seq,
                    self.msg_seq,
                );
                builder.try_add_message(msg.msg_type, &msg.payload);
            }
        }

        // Flush remaining messages in the builder
        if !builder.is_empty() {
            self.send_packet(&builder.finish());
        }
    }

    fn send_packet(&self, data: &[u8]) {
        match self.socket.send_to(data, self.config.target_addr) {
            Ok(n) => {
                self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                self.stats.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
                // Parse to count messages
                if let Ok(packet) = Packet::parse(data) {
                    self.stats
                        .messages_sent
                        .fetch_add(packet.header.msg_count as u64, Ordering::Relaxed);
                }
            }
            Err(_) => {
                self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn send_heartbeat(&self, data: &[u8]) {
        match self.socket.send_to(data, self.config.target_addr) {
            Ok(n) => {
                self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                self.stats.heartbeats_sent.fetch_add(1, Ordering::Relaxed);
                self.stats.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(_) => {
                self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
