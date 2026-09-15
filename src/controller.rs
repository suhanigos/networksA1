use crate::packet::{Packet, PuntReason};
use crate::tinyvm::TableEntry;
use crate::types::{EntryId, PortId, SimTime, SwitchId, TableId};

#[derive(Debug, Clone)]
pub struct PuntEvent {
    pub now: SimTime,
    pub switch: SwitchId,
    pub ingress_port: PortId,
    pub reason: PuntReason,
    pub packet: Packet,
}

#[derive(Debug, Clone)]
pub enum LinkEvent {
    Up { port: PortId },
    Down { port: PortId },
}

#[derive(Debug, Clone, Default)]
pub struct QueueConfig {
    /// Drop-tail queue capacity in bytes. 0 means leave unchanged.
    pub capacity_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum ControllerAction {
    InstallTableEntry {
        table_id: TableId,
        entry: TableEntry,
    },
    DeleteTableEntry {
        table_id: TableId,
        entry_id: EntryId,
    },
    InjectPacket {
        packet: Packet,
        port: PortId,
    },
    SetQueueConfig {
        port: PortId,
        config: QueueConfig,
    },
    /// Ask the simulator to fire `on_timer(now)` again after `delay`.
    /// Travels through the config pipe like other actions.
    ScheduleTimer {
        delay: std::time::Duration,
    },
}

impl ControllerAction {
    /// Charged payload size for the config pipe.
    /// This is the *payload* component; the switch adds its `min_control_msg_size_bytes`.
    pub fn payload_size(&self) -> u64 {
        match self {
            ControllerAction::InstallTableEntry { .. } => 32,
            ControllerAction::DeleteTableEntry { .. } => 16,
            ControllerAction::InjectPacket { packet, .. } => packet.size_bytes,
            ControllerAction::SetQueueConfig { .. } => 16,
            ControllerAction::ScheduleTimer { .. } => 8,
        }
    }
}

pub trait SwitchController {
    fn on_punt(&mut self, event: PuntEvent) -> Vec<ControllerAction>;
    fn on_timer(&mut self, now: SimTime) -> Vec<ControllerAction>;
    fn on_link_event(&mut self, event: LinkEvent) -> Vec<ControllerAction>;

    /// Why this controller stopped running, if it did, as one printable
    /// block. A controller that fails is never called again and its
    /// switch forwards on frozen state -- which is indistinguishable from
    /// a routing bug unless somebody says otherwise. Controllers that
    /// cannot fail return `None`.
    fn failure(&self) -> Option<String> {
        None
    }
}

/// A controller that always returns no actions. Useful for tests where
/// we want to exercise the data plane in isolation.
pub struct NoopController;

impl SwitchController for NoopController {
    fn on_punt(&mut self, _event: PuntEvent) -> Vec<ControllerAction> {
        Vec::new()
    }
    fn on_timer(&mut self, _now: SimTime) -> Vec<ControllerAction> {
        Vec::new()
    }
    fn on_link_event(&mut self, _event: LinkEvent) -> Vec<ControllerAction> {
        Vec::new()
    }
}
