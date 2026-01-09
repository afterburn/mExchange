use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver as ChannelReceiver, Sender as ChannelSender};
use parking_lot::Mutex;
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

/// Configuration for the UDP receiver
#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    /// Expected stream ID (0 = accept any)
    pub stream_id: u32,
    /// Channel capacity for incoming messages
    pub channel_capacity: usize,
    /// Socket receive timeout
    pub recv_timeout: Duration,
    /// Stream timeout (no packets)
    pub stream_timeout: Duration,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            stream_id: 0,
            channel_capacity: 10_000,
            recv_timeout: Duration::from_millis(10),
            stream_timeout: Duration::from_millis(STREAM_TIMEOUT_MS),
        }
    }
}

/// Stream state tracked by receiver
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// Initial state, waiting for first packet
    Initializing,
    /// Receiving normally
    Active,
    /// Gap detected, waiting for recovery
    Degraded,
    /// No packets received within timeout
    Down,
}

/// Gap information
#[derive(Debug, Clone)]
pub struct GapInfo {
    pub expected_seq: u64,
    pub received_seq: u64,
    pub gap_size: u64,
    pub timestamp: Instant,
}

/// Statistics for the receiver
#[derive(Debug, Default)]
pub struct ReceiverStats {
    pub packets_received: AtomicU64,
    pub messages_received: AtomicU64,
    pub bytes_received: AtomicU64,
    pub heartbeats_received: AtomicU64,
    pub duplicates_dropped: AtomicU64,
    pub gaps_detected: AtomicU64,
    pub total_gap_messages: AtomicU64,
    pub recv_errors: AtomicU64,
}

impl ReceiverStats {
    pub fn snapshot(&self) -> ReceiverStatsSnapshot {
        ReceiverStatsSnapshot {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            messages_received: self.messages_received.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            heartbeats_received: self.heartbeats_received.load(Ordering::Relaxed),
            duplicates_dropped: self.duplicates_dropped.load(Ordering::Relaxed),
            gaps_detected: self.gaps_detected.load(Ordering::Relaxed),
            total_gap_messages: self.total_gap_messages.load(Ordering::Relaxed),
            recv_errors: self.recv_errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReceiverStatsSnapshot {
    pub packets_received: u64,
    pub messages_received: u64,
    pub bytes_received: u64,
    pub heartbeats_received: u64,
    pub duplicates_dropped: u64,
    pub gaps_detected: u64,
    pub total_gap_messages: u64,
    pub recv_errors: u64,
}

/// Received message with metadata
#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
    pub seq: u64,
    pub stream_id: u32,
}

/// UDP receiver with gap detection and sequence tracking
pub struct UdpReceiver {
    #[allow(dead_code)]
    config: ReceiverConfig,
    rx: ChannelReceiver<ReceivedMessage>,
    stats: Arc<ReceiverStats>,
    state: Arc<Mutex<StreamState>>,
    gaps: Arc<Mutex<VecDeque<GapInfo>>>,
    running: Arc<AtomicBool>,
    worker_handle: Option<JoinHandle<()>>,
}

impl UdpReceiver {
    /// Create a new receiver bound to the specified address
    pub fn new(config: ReceiverConfig, bind_addr: SocketAddr) -> Result<Self> {
        let socket = create_udp_socket_with_reuse(bind_addr)?;
        socket.set_read_timeout(Some(config.recv_timeout))?;

        let (tx, rx) = bounded(config.channel_capacity);
        let stats = Arc::new(ReceiverStats::default());
        let state = Arc::new(Mutex::new(StreamState::Initializing));
        let gaps = Arc::new(Mutex::new(VecDeque::new()));
        let running = Arc::new(AtomicBool::new(true));

        let worker = ReceiverWorker {
            config: config.clone(),
            socket,
            tx,
            stats: stats.clone(),
            state: state.clone(),
            gaps: gaps.clone(),
            running: running.clone(),
            expected_packet_seq: 0,
            expected_msg_seq: 0,
            initialized: false,
            last_packet_time: Instant::now(),
        };

        let handle = thread::Builder::new()
            .name(format!("udp-receiver-{}", config.stream_id))
            .spawn(move || worker.run())
            .map_err(|e| ProtocolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        Ok(Self {
            config,
            rx,
            stats,
            state,
            gaps,
            running,
            worker_handle: Some(handle),
        })
    }

    /// Receive a message (blocking)
    pub fn recv(&self) -> Result<ReceivedMessage> {
        self.rx.recv().map_err(|_| ProtocolError::ChannelClosed)
    }

    /// Try to receive a message (non-blocking)
    pub fn try_recv(&self) -> Result<Option<ReceivedMessage>> {
        match self.rx.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(None),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(ProtocolError::ChannelClosed),
        }
    }

    /// Receive with timeout
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<ReceivedMessage>> {
        match self.rx.recv_timeout(timeout) {
            Ok(msg) => Ok(Some(msg)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(None),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(ProtocolError::ChannelClosed)
            }
        }
    }

    /// Get current stream state
    pub fn state(&self) -> StreamState {
        *self.state.lock()
    }

    /// Get current statistics
    pub fn stats(&self) -> ReceiverStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get detected gaps
    pub fn gaps(&self) -> Vec<GapInfo> {
        self.gaps.lock().iter().cloned().collect()
    }

    /// Clear detected gaps (after recovery)
    pub fn clear_gaps(&self) {
        self.gaps.lock().clear();
        let mut state = self.state.lock();
        if *state == StreamState::Degraded {
            *state = StreamState::Active;
        }
    }

    /// Check if the receiver is running
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Shutdown the receiver
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for UdpReceiver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ReceiverWorker {
    config: ReceiverConfig,
    socket: UdpSocket,
    tx: ChannelSender<ReceivedMessage>,
    stats: Arc<ReceiverStats>,
    state: Arc<Mutex<StreamState>>,
    gaps: Arc<Mutex<VecDeque<GapInfo>>>,
    running: Arc<AtomicBool>,
    expected_packet_seq: u64,
    expected_msg_seq: u64,
    initialized: bool,
    last_packet_time: Instant,
}

impl ReceiverWorker {
    fn run(mut self) {
        let mut buf = vec![0u8; MAX_MTU];

        while self.running.load(Ordering::Relaxed) {
            match self.socket.recv_from(&mut buf) {
                Ok((len, _addr)) => {
                    self.last_packet_time = Instant::now();
                    self.stats.bytes_received.fetch_add(len as u64, Ordering::Relaxed);

                    if let Err(_e) = self.process_packet(&buf[..len]) {
                        self.stats.recv_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Check for stream timeout
                    if self.initialized
                        && self.last_packet_time.elapsed() > self.config.stream_timeout
                    {
                        let mut state = self.state.lock();
                        if *state != StreamState::Down {
                            *state = StreamState::Down;
                        }
                    }
                }
                Err(_) => {
                    self.stats.recv_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn process_packet(&mut self, data: &[u8]) -> Result<()> {
        let packet = Packet::parse(data)?;

        // Filter by stream_id if configured
        if self.config.stream_id != 0 && packet.header.stream_id != self.config.stream_id {
            return Ok(());
        }

        self.stats.packets_received.fetch_add(1, Ordering::Relaxed);

        // Handle heartbeat
        if packet.header.is_heartbeat() {
            self.stats.heartbeats_received.fetch_add(1, Ordering::Relaxed);
            self.update_state_active();
            return Ok(());
        }

        // Initialize on first packet
        if !self.initialized {
            self.expected_packet_seq = packet.header.packet_seq;
            self.expected_msg_seq = packet.header.first_msg_seq;
            self.initialized = true;
            self.update_state_active();
        }

        // Check for duplicates
        if packet.header.packet_seq < self.expected_packet_seq {
            self.stats.duplicates_dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        // Check for gaps
        if packet.header.packet_seq > self.expected_packet_seq {
            let gap_size = packet.header.first_msg_seq - self.expected_msg_seq;
            self.record_gap(self.expected_msg_seq, packet.header.first_msg_seq, gap_size);
        }

        // Process messages
        for msg in packet.messages {
            if let Some(msg_type) = msg.message_type() {
                let received = ReceivedMessage {
                    msg_type,
                    payload: msg.payload,
                    seq: msg.seq,
                    stream_id: packet.header.stream_id,
                };

                self.stats.messages_received.fetch_add(1, Ordering::Relaxed);

                // Non-blocking send - drop if channel full
                let _ = self.tx.try_send(received);
            }
        }

        // Update expected sequences
        self.expected_packet_seq = packet.header.packet_seq + 1;
        self.expected_msg_seq = packet.header.first_msg_seq + packet.header.msg_count as u64;

        Ok(())
    }

    fn record_gap(&mut self, expected: u64, received: u64, gap_size: u64) {
        self.stats.gaps_detected.fetch_add(1, Ordering::Relaxed);
        self.stats.total_gap_messages.fetch_add(gap_size, Ordering::Relaxed);

        let gap = GapInfo {
            expected_seq: expected,
            received_seq: received,
            gap_size,
            timestamp: Instant::now(),
        };

        let mut gaps = self.gaps.lock();
        gaps.push_back(gap);

        // Keep only recent gaps
        while gaps.len() > 100 {
            gaps.pop_front();
        }

        let mut state = self.state.lock();
        *state = StreamState::Degraded;
    }

    fn update_state_active(&self) {
        let mut state = self.state.lock();
        if *state == StreamState::Initializing || *state == StreamState::Down {
            *state = StreamState::Active;
        }
    }
}
