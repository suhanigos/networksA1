# Programming a switch

A `Switch` is the union of:

1. A **TinyVM data plane** (`TinyProgram`) running against every packet.
2. A **TinyVM state** (`TinyVmState`) holding tables, registers, counters.
3. A **CPU-style controller** (anything implementing `SwitchController`).
4. A **punt pipe** carrying punted packets up to the controller.
5. A **config pipe** carrying controller actions back down.
6. **Egress queues**, one per port (drop-tail FIFO).

Building a switch means filling in each of those.

## 1. SwitchConfig

`SwitchConfig` collects all the timing and budget knobs:

```rust
pub struct SwitchConfig {
    pub switch_id: SwitchId,

    pub stages: usize,                       // pipeline depth
    pub processing_delay_per_stage: Duration,
    pub recirculation_enabled: bool,
    pub max_recirculations: u8,

    pub min_control_msg_size_bytes: u64,     // overhead added to control msgs

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
```

`SwitchConfig::defaults(switch_id)` gives reasonable values for a
nanosecond-scale switch with 1 Gbps pipes; override any field afterwards.

The total pipeline delay one packet sees is
`stages * processing_delay_per_stage`. A recirculated packet pays it again.

Control-plane message size is charged as
`min_control_msg_size_bytes + payload_size`, so adjusting
`min_control_msg_size_bytes` lets you model the overhead of the control
protocol independently of payload.

## 2. TinyVmState

The state holds everything the data plane can read or mutate
persistently. You declare it before constructing the switch so the
validator can check programs against it:

```rust
let mut state = TinyVmState::default();
state.tables.push(MatchActionTable::new(TableId::new(1), MatchKind::Lpm, 1024));
state.registers.push(RegisterArray::new(RegisterArrayId::new(0), 256));
state.counters.push(CounterArray::new(CounterArrayId::new(0), 256));
```

You can pre-populate tables here for static forwarding, or leave them
empty and let the controller install entries at runtime.

## 3. The TinyProgram

See [tinyvm.md](tinyvm.md) for the language. A program is just a
`TinyProgram { stages }`. Validate it against the state before letting it
loose:

```rust
validate(&program, &state, &ValidatorLimits::default())?;
```

## 4. Controllers

A controller implements:

```rust
pub trait SwitchController {
    fn on_punt(&mut self, event: PuntEvent) -> Vec<ControllerAction>;
    fn on_timer(&mut self, now: SimTime) -> Vec<ControllerAction>;
    fn on_link_event(&mut self, event: LinkEvent) -> Vec<ControllerAction>;
}
```

Controllers receive events and return actions. They never touch tables or
queues directly — every effect goes through:

```rust
pub enum ControllerAction {
    InstallTableEntry { table_id, entry },
    DeleteTableEntry  { table_id, entry_id },
    InjectPacket      { packet, port },
    SetQueueConfig    { port, config },
}
```

Each returned action travels through the **config pipe** before it lands on
the data plane. So if the controller installs a route in response to a
punt, the install does not take effect until
`config_pipe_latency + serialization_delay(action_size)` after the
controller emits it. `InjectPacket` is the same — the simulator schedules
the packet onto the appropriate egress link at the config-pipe arrival
time.

A `NoopController` is provided for tests where the data plane is the only
thing under examination.

### A learning controller

```rust
struct InstallOnPuntController { installed: bool, table: TableId }

impl SwitchController for InstallOnPuntController {
    fn on_punt(&mut self, _event: PuntEvent) -> Vec<ControllerAction> {
        if self.installed { return vec![]; }
        self.installed = true;
        vec![ControllerAction::InstallTableEntry {
            table_id: self.table,
            entry: TableEntry {
                id: EntryId::new(42),
                key: 0x0a000002, prefix_len: 32, priority: 0,
                action: TableAction::SetEgress { port: PortId::new(1) },
            },
        }]
    }
    fn on_timer(&mut self, _now: SimTime) -> Vec<ControllerAction> { vec![] }
    fn on_link_event(&mut self, _: LinkEvent) -> Vec<ControllerAction> { vec![] }
}
```

The first packet for which the table misses gets punted (the `NoEgress`
fallback). The controller sees it, installs a route, and the next packet
hits the freshly-installed entry — provided enough time has passed for the
config pipe to drain.

### WASM controllers

If you build with `--features wasm` you also get `WasmController`, which
loads a `wasm` module exporting:

```text
init(config_ptr, config_len) -> i32
on_punt(event_ptr, event_len, out_ptr, out_cap) -> i32
on_timer(now_lo, now_hi, out_ptr, out_cap) -> i32
on_link_event(event_ptr, event_len, out_ptr, out_cap) -> i32
```

Inputs and outputs are postcard-encoded over a shared linear-memory
buffer. The host meters fuel (`WasmLimits.fuel_per_call`), catches traps,
bounds output size (`max_output_bytes`), and treats trap / fuel-exhaustion
/ decode errors as controller failure (the controller stops receiving
events; the data plane keeps forwarding).

### Failure model

Two distinct failure flags:

- `Switch::cpu.failed` — set by `Simulator::fail_cpu(switch_id)`. The
  controller stops receiving punts/timers/link events. The data plane
  keeps forwarding using the table state at the moment of failure.
- `Switch::failed` — set by `Simulator::fail_switch(switch_id)`. The whole
  switch is dead; ingress packets are silently dropped.

## 5. Putting it together

```rust
let switch = Switch::new(
    SwitchConfig::defaults(switch_id),
    program,
    state,
    Box::new(NoopController),     // or any custom controller
    /* num_ports        */ 4,
    /* per_port_queue_bytes */ 1 << 16,
);
sim.add_switch(switch);
```

`num_ports` sizes the egress-queue array; ports beyond it are unreachable.
`per_port_queue_bytes` is the initial drop-tail capacity per port —
controllers can resize it later via `SetQueueConfig`.
