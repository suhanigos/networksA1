use crate::packet::Packet;
use crate::types::{LinkId, NodeId, PortId, SimTime};
use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct LinkConfig {
    pub latency: Duration,
    pub bandwidth_bps: u64,
    pub queue_capacity_bytes: u64,
}

impl LinkConfig {
    pub fn serialization_delay(&self, size_bytes: u64) -> Duration {
        if self.bandwidth_bps == 0 {
            return Duration::ZERO;
        }
        let nanos = (size_bytes as u128 * 8 * 1_000_000_000) / self.bandwidth_bps as u128;
        Duration::from_nanos(nanos as u64)
    }
}

/// A link from a source node/port to a destination node/port.
///
/// Per spec it has a drop-tail queue at the egress side and per-byte
/// serialization plus a propagation latency. Packets are emitted from the
/// queue in FIFO order; once serialized they spend `latency` propagating.
#[derive(Debug)]
pub struct Link {
    pub id: LinkId,
    pub config: LinkConfig,
    pub src: NodeId,
    pub src_port: PortId,
    pub dst: NodeId,
    pub dst_port: PortId,

    /// Drop-tail FIFO queue (head has been popped if `serializing` is true).
    queue: VecDeque<Packet>,
    queued_bytes: u64,

    /// Time at which the currently-serializing packet (if any) finishes.
    busy_until: SimTime,

    /// Total bytes dropped due to overflow.
    pub bytes_dropped: u64,
    pub packets_dropped: u64,
    /// When true, new enqueues are dropped. Already-queued packets still
    /// drain. Toggle through `Simulator::fail_link` / `restore_link`.
    pub failed: bool,
}

impl Link {
    pub fn new(
        id: LinkId,
        config: LinkConfig,
        src: NodeId,
        src_port: PortId,
        dst: NodeId,
        dst_port: PortId,
    ) -> Self {
        Self {
            id,
            config,
            src,
            src_port,
            dst,
            dst_port,
            queue: VecDeque::new(),
            queued_bytes: 0,
            busy_until: Duration::ZERO,
            bytes_dropped: 0,
            packets_dropped: 0,
            failed: false,
        }
    }

    /// Try to enqueue a packet. Returns `Some((serialization_done_at, arrive_at))`
    /// if the packet was enqueued and we know exactly when it leaves the wire,
    /// `None` if it was dropped due to queue overflow.
    pub fn enqueue(&mut self, now: SimTime, packet: Packet) -> Option<(SimTime, SimTime)> {
        if self.failed {
            self.bytes_dropped += packet.size_bytes;
            self.packets_dropped += 1;
            return None;
        }
        if self.queued_bytes + packet.size_bytes > self.config.queue_capacity_bytes {
            self.bytes_dropped += packet.size_bytes;
            self.packets_dropped += 1;
            return None;
        }
        self.queued_bytes += packet.size_bytes;

        // The packet starts being serialized when the link becomes idle, but
        // not before now.
        let start = self.busy_until.max(now);
        let serialization_done = start + self.config.serialization_delay(packet.size_bytes);
        let arrive = serialization_done + self.config.latency;
        self.busy_until = serialization_done;
        self.queue.push_back(packet);
        Some((serialization_done, arrive))
    }

    /// Remove the head packet (the one whose serialization just finished).
    pub fn dequeue_head(&mut self) -> Option<Packet> {
        let pkt = self.queue.pop_front()?;
        self.queued_bytes -= pkt.size_bytes;
        Some(pkt)
    }

    pub fn queued_bytes(&self) -> u64 {
        self.queued_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PacketId;

    fn pkt(id: u64, size: u64) -> Packet {
        Packet::new(PacketId::new(id), Duration::ZERO, size)
    }

    fn cfg() -> LinkConfig {
        LinkConfig {
            latency: Duration::from_millis(10),
            bandwidth_bps: 8_000, // 1 KB/s -> 1ms per byte
            queue_capacity_bytes: 1000,
        }
    }

    #[test]
    fn serialization_delay_is_correct() {
        let c = LinkConfig {
            latency: Duration::ZERO,
            bandwidth_bps: 8_000_000_000, // 1 GB/s
            queue_capacity_bytes: 1_000_000,
        };
        // 1000 bytes at 1 GB/s = 1 us
        assert_eq!(c.serialization_delay(1000), Duration::from_micros(1));
    }

    #[test]
    fn link_propagation_and_serialization() {
        let mut link = Link::new(
            LinkId::new(0),
            cfg(),
            NodeId::new(0),
            PortId::new(0),
            NodeId::new(1),
            PortId::new(0),
        );

        // 1 byte, bandwidth 8000 bps -> 1ms serialization, 10ms latency.
        let (ser_done, arrive) = link.enqueue(Duration::ZERO, pkt(1, 1)).unwrap();
        assert_eq!(ser_done, Duration::from_millis(1));
        assert_eq!(arrive, Duration::from_millis(11));
    }

    #[test]
    fn drop_tail_overflow() {
        let small_cfg = LinkConfig {
            latency: Duration::ZERO,
            bandwidth_bps: 8_000_000_000,
            queue_capacity_bytes: 100,
        };
        let mut link = Link::new(
            LinkId::new(0),
            small_cfg,
            NodeId::new(0),
            PortId::new(0),
            NodeId::new(1),
            PortId::new(0),
        );

        assert!(link.enqueue(Duration::ZERO, pkt(1, 60)).is_some());
        assert!(link.enqueue(Duration::ZERO, pkt(2, 30)).is_some());
        // 60 + 30 + 30 > 100 -> dropped
        assert!(link.enqueue(Duration::ZERO, pkt(3, 30)).is_none());
        assert_eq!(link.packets_dropped, 1);
    }

    #[test]
    fn deterministic_packet_ordering() {
        let mut link = Link::new(
            LinkId::new(0),
            cfg(),
            NodeId::new(0),
            PortId::new(0),
            NodeId::new(1),
            PortId::new(0),
        );

        // Two packets back-to-back: second waits for the first to finish serializing.
        let (s1, a1) = link.enqueue(Duration::ZERO, pkt(1, 1)).unwrap();
        let (s2, a2) = link.enqueue(Duration::ZERO, pkt(2, 1)).unwrap();
        assert!(s1 < s2);
        assert!(a1 < a2);
    }
}
