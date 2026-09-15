//! Wire types and TinyVM text parser shared between the simulator host and
//! the `switch_program_sdk`.
//!
//! All on-the-wire structs are postcard-serializable and `no_std`-friendly so
//! a switch program compiled to `wasm32-unknown-unknown` can encode and decode
//! them with the same code the simulator uses.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

pub mod text;
pub mod bgp;

// ---------- Identity ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortIdW(pub u16);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueueIdW(pub u16);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableIdW(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RegisterArrayIdW(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CounterArrayIdW(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryIdW(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MetaKeyW(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RegW(pub u8);

// ---------- Packet fields ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PacketFieldW {
    IpSrc,
    IpDst,
    IpProto,
    IpTtl,
    IpDscp,
    SrcPort,
    DstPort,
    AppId,
    FlowId,
    Custom(u8),
    Size,
    Kind,
    LabelTop,
    LabelDepth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PuntReasonW {
    NoRoute,
    TtlExpired,
    Custom(u32),
}

// ---------- TinyVM ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatchKindW {
    Exact,
    Lpm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InstrW {
    LoadField { dst: RegW, field: PacketFieldW },
    StoreField { field: PacketFieldW, src: RegW },
    LoadMeta { dst: RegW, key: MetaKeyW },
    StoreMeta { key: MetaKeyW, src: RegW },
    Const { dst: RegW, value: u64 },
    Add { dst: RegW, a: RegW, b: RegW },
    Sub { dst: RegW, a: RegW, b: RegW },
    And { dst: RegW, a: RegW, b: RegW },
    Or  { dst: RegW, a: RegW, b: RegW },
    Xor { dst: RegW, a: RegW, b: RegW },
    Eq  { dst: RegW, a: RegW, b: RegW },
    Lt  { dst: RegW, a: RegW, b: RegW },
    TableLookup { table_id: TableIdW, key_reg: RegW, result_meta: MetaKeyW },
    RegisterRead { array: RegisterArrayIdW, index: RegW, dst: RegW },
    RegisterWrite { array: RegisterArrayIdW, index: RegW, src: RegW },
    CounterAdd { counter: CounterArrayIdW, index: RegW, value: RegW },
    BranchIf { cond: RegW, target: u32 },
    Drop,
    Punt { reason: PuntReasonW },
    SetEgress { port: PortIdW },
    SetQueue { queue: QueueIdW },
    Recirculate,
    Noop,
    PushLabel { label: u32 },
    PopLabel,
    SwapLabel { label: u32 },
    /// Mark the packet so that wherever it terminates, a TraceReply is
    /// magically delivered to *this* switch on the egress port the data
    /// plane chose. The reply consumes link bandwidth normally.
    MarkTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageProgramW {
    pub instrs: Vec<InstrW>,
    pub max_alu_ops: u32,
    pub max_memory_accesses: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TinyProgramW {
    pub stages: Vec<StageProgramW>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TableActionW {
    SetMeta { key: MetaKeyW, value: u64 },
    SetEgress { port: PortIdW },
    SetQueue { queue: QueueIdW },
    Drop,
    Punt { reason: PuntReasonW },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableEntryW {
    pub id: EntryIdW,
    pub key: u64,
    pub prefix_len: u8,
    pub priority: i32,
    pub action: TableActionW,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDeclW {
    pub id: TableIdW,
    pub kind: MatchKindW,
    pub max_entries: u32,
    pub initial_entries: Vec<TableEntryW>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeclW {
    pub id: RegisterArrayIdW,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterDeclW {
    pub id: CounterArrayIdW,
    pub size: u32,
}

/// What `init` returns: the data plane's program plus state declarations.
/// The simulator takes ownership and installs them on the switch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgramSetupW {
    pub program: TinyProgramW,
    pub tables: Vec<TableDeclW>,
    pub registers: Vec<RegisterDeclW>,
    pub counters: Vec<CounterDeclW>,
}

// ---------- Controller events / actions ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitInputW {
    pub switch_id: u32,
    /// Local port ids that have a link attached, filled in from the
    /// topology. Which port leads where is not said, and nothing here
    /// implies what a program should do with them.
    pub local_ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PuntEventW {
    pub now_ns: u64,
    pub switch_id: u32,
    pub ingress_port: u16,
    pub reason: PuntReasonW,
    pub packet_size: u64,
    pub ip_src: u32,
    pub ip_dst: u32,
    pub ip_proto: u8,
    pub ip_ttl: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEventW {
    /// 0 = up, 1 = down
    pub kind: u8,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerEventW {
    pub now_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionW {
    InstallTableEntry { table_id: TableIdW, entry: TableEntryW },
    DeleteTableEntry { table_id: TableIdW, entry_id: EntryIdW },
    SetQueueConfig { port: PortIdW, capacity_bytes: u64 },
    InjectPacket {
        port: PortIdW,
        ip_src: u32,
        ip_dst: u32,
        ip_proto: u8,
        ip_ttl: u8,
        /// PacketKind discriminant: 0=Data, 1=SimpleBgp, 2=TraceProbe, 3=TraceReply.
        kind: u8,
        size_bytes: u64,
        payload: Vec<u8>,
    },
    /// Schedule a controller-timer callback `delay_ns` after now.
    ScheduleTimer { delay_ns: u64 },
}

// ---------- Trace reply (delivered to marking switch's controller) ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceHopW {
    pub switch_id: u32,
    pub arrived_at_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceReplyW {
    /// The switch that asked for the trace by running `MarkTrace`.
    pub mark_switch: u32,
    /// The port on `mark_switch` where the reply is arriving (this is
    /// also the egress port the marked packet originally took).
    pub mark_port: u16,
    /// Switches the original packet visited, in order.
    pub hops: Vec<TraceHopW>,
    /// Whether the packet was eventually delivered (true) or dropped.
    pub delivered: bool,
    /// 0 = delivered, 1 = ttl, 2 = no_route, 3 = no_egress_link, 4 = link_drop, 5 = switch_failed, 6 = recirc.
    pub drop_reason: u8,
}

// ---------- Decode error ----------

#[derive(Debug)]
pub enum WireError {
    Decode(String),
}

impl WireError {
    pub fn decode(e: impl ToString) -> Self {
        WireError::Decode(e.to_string())
    }
}
