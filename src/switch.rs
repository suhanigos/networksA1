use crate::controller::{ControllerAction, SwitchController};
use crate::packet::Packet;
use crate::tinyvm::{PipelineResult, StageDecision, TinyProgram, TinyVmState, run_stage};
use crate::types::{OwnerId, PortId, QueueId, SimTime, SwitchId};
use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SwitchConfig {
    pub switch_id: SwitchId,

    pub stages: usize,
    pub processing_delay_per_stage: Duration,
    pub recirculation_enabled: bool,
    pub max_recirculations: u8,

    pub min_control_msg_size_bytes: u64,

    pub cpu_fuel_per_tick: u64,
    pub cpu_seconds_per_fuel: f64,

    pub punt_pipe_latency: Duration,
    pub punt_pipe_bandwidth_bps: u64,
    pub punt_pipe_queue_bytes: u64,

    pub config_pipe_latency: Duration,
    pub config_pipe_bandwidth_bps: u64,
    pub config_pipe_queue_bytes: u64,

    pub tinyvm_alu_ops_per_stage: u32,
    pub tinyvm_memory_accesses_per_stage: u8,
}

impl SwitchConfig {
    pub fn defaults(switch_id: SwitchId) -> Self {
        Self {
            switch_id,
            stages: 4,
            processing_delay_per_stage: Duration::from_nanos(100),
            recirculation_enabled: true,
            max_recirculations: 2,
            min_control_msg_size_bytes: 64,
            cpu_fuel_per_tick: 100_000,
            cpu_seconds_per_fuel: 1e-9,
            punt_pipe_latency: Duration::from_micros(10),
            punt_pipe_bandwidth_bps: 1_000_000_000,
            punt_pipe_queue_bytes: 1 << 20,
            config_pipe_latency: Duration::from_micros(10),
            config_pipe_bandwidth_bps: 1_000_000_000,
            config_pipe_queue_bytes: 1 << 20,
            tinyvm_alu_ops_per_stage: 32,
            tinyvm_memory_accesses_per_stage: 1,
        }
    }
}

/// A simple drop-tail FIFO per egress port.
#[derive(Debug)]
pub struct EgressQueue {
    pub capacity_bytes: u64,
    pub queue: VecDeque<Packet>,
    pub queued_bytes: u64,
    pub busy_until: SimTime,
}

impl EgressQueue {
    pub fn new(capacity_bytes: u64) -> Self {
        Self {
            capacity_bytes,
            queue: VecDeque::new(),
            queued_bytes: 0,
            busy_until: Duration::ZERO,
        }
    }
}

/// A pipe with latency, bandwidth, and a drop-tail byte queue.
/// Used for punt and config pipes.
#[derive(Debug)]
pub struct Pipe {
    pub latency: Duration,
    pub bandwidth_bps: u64,
    pub queue_capacity_bytes: u64,
    queued_bytes: u64,
    busy_until: SimTime,
    pub packets_dropped: u64,
}

impl Pipe {
    pub fn new(latency: Duration, bandwidth_bps: u64, queue_capacity_bytes: u64) -> Self {
        Self {
            latency,
            bandwidth_bps,
            queue_capacity_bytes,
            queued_bytes: 0,
            busy_until: Duration::ZERO,
            packets_dropped: 0,
        }
    }

    fn serialization_delay(&self, size_bytes: u64) -> Duration {
        if self.bandwidth_bps == 0 {
            return Duration::ZERO;
        }
        Duration::from_nanos(
            ((size_bytes as u128 * 8 * 1_000_000_000) / self.bandwidth_bps as u128) as u64,
        )
    }

    /// Try to send a `size_bytes` payload. Returns the absolute arrival time
    /// at the far end, or None if dropped.
    pub fn schedule(&mut self, now: SimTime, size_bytes: u64) -> Option<SimTime> {
        if self.queued_bytes + size_bytes > self.queue_capacity_bytes {
            self.packets_dropped += 1;
            return None;
        }
        // We use a simplified model: each msg occupies the pipe for its
        // serialization time; arrivals are then delayed by `latency`. The
        // queued_bytes is a logical bound; we decrement it when serialization
        // completes. To keep things simple we drop based on outstanding bytes
        // currently being serialized.
        self.queued_bytes += size_bytes;
        let start = self.busy_until.max(now);
        let serialized_at = start + self.serialization_delay(size_bytes);
        let arrive = serialized_at + self.latency;
        self.busy_until = serialized_at;
        // Defer freeing queued_bytes for simplicity: we just decrement at
        // arrival time. Caller is responsible for calling `release`.
        Some(arrive)
    }

    pub fn release(&mut self, size_bytes: u64) {
        self.queued_bytes = self.queued_bytes.saturating_sub(size_bytes);
    }
}

/// State of the switch's CPU. When `failed` is true the controller stops
/// receiving events but the data plane keeps forwarding.
pub struct CpuState {
    pub failed: bool,
}

impl Default for CpuState {
    fn default() -> Self {
        Self { failed: false }
    }
}

pub struct Switch {
    pub config: SwitchConfig,
    pub program: TinyProgram,
    pub state: TinyVmState,
    pub controller: Box<dyn SwitchController>,
    pub egress_queues: Vec<EgressQueue>,
    pub punt_pipe: Pipe,
    pub config_pipe: Pipe,
    pub cpu: CpuState,

    /// The owner of the switch. Programs installed via
    /// `Simulator::install_program` are scoped to an owner; switches with no
    /// owner keep whatever program they were constructed with.
    pub owner: Option<OwnerId>,

    /// Total packets dropped due to no egress.
    pub packets_dropped_no_egress: u64,
    /// True after the entire switch has failed (data plane stops too).
    pub failed: bool,
}

impl Switch {
    pub fn new(
        config: SwitchConfig,
        program: TinyProgram,
        state: TinyVmState,
        controller: Box<dyn SwitchController>,
        num_ports: usize,
        per_port_queue_bytes: u64,
    ) -> Self {
        let punt_pipe = Pipe::new(
            config.punt_pipe_latency,
            config.punt_pipe_bandwidth_bps,
            config.punt_pipe_queue_bytes,
        );
        let config_pipe = Pipe::new(
            config.config_pipe_latency,
            config.config_pipe_bandwidth_bps,
            config.config_pipe_queue_bytes,
        );
        Self {
            config,
            program,
            state,
            controller,
            egress_queues: (0..num_ports)
                .map(|_| EgressQueue::new(per_port_queue_bytes))
                .collect(),
            punt_pipe,
            config_pipe,
            cpu: CpuState::default(),
            owner: None,
            packets_dropped_no_egress: 0,
            failed: false,
        }
    }

    /// Tag this switch with an owner. Useful with [`Simulator::install_program`].
    pub fn with_owner(mut self, owner: OwnerId) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Total processing delay through the pipeline.
    pub fn pipeline_delay(&self) -> Duration {
        self.config
            .processing_delay_per_stage
            .saturating_mul(self.config.stages as u32)
    }

    /// Run the configured pipeline against a packet exactly once (one pass).
    /// Returns the pipeline outcome.
    pub fn process_pipeline(&mut self, packet: &mut Packet) -> PipelineOutcome {
        let mut result = PipelineResult::default();
        let mut early: Option<PipelineOutcome> = None;
        for stage in &self.program.stages {
            let dec = run_stage(stage, packet, &mut self.state, &mut result);
            match dec {
                StageDecision::Continue => continue,
                StageDecision::Drop => { early = Some(PipelineOutcome::Drop); break; }
                StageDecision::Punt(reason) => {
                    early = Some(PipelineOutcome::Punt(reason));
                    break;
                }
                StageDecision::Recirculate => {
                    early = Some(PipelineOutcome::Recirculate);
                    break;
                }
            }
        }

        // Persist trace-mark info on the packet before returning. Early
        // exits still record a mark — the trace reply is for "wherever it
        // terminated", which is here.
        if result.trace_marked && packet.trail.trace_mark_switch.is_none() {
            packet.trail.trace_mark_switch = Some(self.config.switch_id);
            packet.trail.trace_mark_port = result.egress_port;
        }

        if let Some(out) = early {
            return out;
        }
        if let Some(port) = result.egress_port {
            PipelineOutcome::Forward {
                port,
                queue: result.queue.unwrap_or(QueueId::new(0)),
            }
        } else {
            PipelineOutcome::NoEgress
        }
    }

    /// Apply a controller action arriving via the config pipe.
    pub fn apply_action(&mut self, action: ControllerAction) {
        match action {
            ControllerAction::InstallTableEntry { table_id, entry } => {
                if let Some(t) = self.state.table_mut(table_id) {
                    let _ = t.install(entry);
                }
            }
            ControllerAction::DeleteTableEntry { table_id, entry_id } => {
                if let Some(t) = self.state.table_mut(table_id) {
                    t.delete(entry_id);
                }
            }
            ControllerAction::InjectPacket { .. } => {
                // Handled by simulator (it needs to schedule the packet).
            }
            ControllerAction::SetQueueConfig { port, config } => {
                if let Some(q) = self.egress_queues.get_mut(port.raw() as usize) {
                    if config.capacity_bytes > 0 {
                        q.capacity_bytes = config.capacity_bytes;
                    }
                }
            }
            ControllerAction::ScheduleTimer { .. } => {
                // Handled by the simulator.
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineOutcome {
    Forward { port: PortId, queue: QueueId },
    Drop,
    Punt(crate::packet::PuntReason),
    Recirculate,
    /// Pipeline ran cleanly but no egress was set.
    NoEgress,
}
