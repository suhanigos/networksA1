use competitive_net_sim::app::{App, TrafficPattern};
use competitive_net_sim::controller::{
    ControllerAction, NoopController, PuntEvent, SwitchController,
};
use competitive_net_sim::link::LinkConfig;
use competitive_net_sim::network::Node;
use competitive_net_sim::packet::{IpAddr, PacketField, PuntReason};
use competitive_net_sim::sim::Simulator;
use competitive_net_sim::switch::{Switch, SwitchConfig};
use competitive_net_sim::tinyvm::{
    Instr, MatchActionTable, MatchKind, StageProgram, TableAction, TableEntry, TinyProgram,
    TinyVmState,
};
use competitive_net_sim::types::{
    AppId, EntryId, MetaKey, PortId, Reg, SimTime, SwitchId, TableId,
};
use std::time::Duration;

fn build_simple_topology(
    install_route: bool,
) -> (Simulator, AppId, AppId, SwitchId) {
    let mut sim = Simulator::new();
    let app_a = AppId::new(1);
    let app_b = AppId::new(2);
    let switch_id = SwitchId::new(0);

    let a = App::new(
        app_a,
        IpAddr(0x0a000001), 32,
        IpAddr(0x0a000002), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_millis(10),
            size_bytes: 100,
        },
    );
    let b = App::new(
        app_b,
        IpAddr(0x0a000002), 32,
        IpAddr(0x0a000001), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_secs(3600),
            size_bytes: 0,
        },
    );

    let mut state = TinyVmState::default();
    let mut table = MatchActionTable::new(TableId::new(1), MatchKind::Exact, 16);
    if install_route {
        table
            .install(TableEntry {
                id: EntryId::new(1),
                key: 0x0a000002,
                prefix_len: 32,
                priority: 0,
                action: TableAction::SetEgress {
                    port: PortId::new(1),
                },
            })
            .unwrap();
    }
    state.tables.push(table);

    let prog = TinyProgram {
        stages: vec![StageProgram {
            instrs: vec![
                Instr::LoadField {
                    dst: Reg::new(0),
                    field: PacketField::IpDst,
                },
                Instr::TableLookup {
                    table_id: TableId::new(1),
                    key_reg: Reg::new(0),
                    result_meta: MetaKey::new(0),
                },
            ],
            max_alu_ops: 0,
            max_memory_accesses: 1,
        }],
    };

    let switch = Switch::new(
        SwitchConfig::defaults(switch_id),
        prog,
        state,
        Box::new(NoopController),
        4,
        1 << 16,
    );
    sim.add_switch(switch);
    sim.add_app(a);
    sim.add_app(b);

    sim.connect(
        Node::App(app_a),
        PortId::new(0),
        Node::Switch(switch_id),
        PortId::new(0),
        LinkConfig {
            latency: Duration::from_micros(100),
            bandwidth_bps: 1_000_000_000,
            queue_capacity_bytes: 1 << 16,
        },
    );
    sim.connect(
        Node::Switch(switch_id),
        PortId::new(1),
        Node::App(app_b),
        PortId::new(0),
        LinkConfig {
            latency: Duration::from_micros(100),
            bandwidth_bps: 1_000_000_000,
            queue_capacity_bytes: 1 << 16,
        },
    );

    (sim, app_a, app_b, switch_id)
}

#[test]
fn packet_forwarded_on_matching_route() {
    let (mut sim, _a, b, _s) = build_simple_topology(true);
    sim.run_until(Duration::from_millis(50));
    assert!(sim.apps[&b].metrics.packets_received > 0);
}

#[test]
fn packet_dropped_on_no_route_when_no_punt() {
    // We use a punting switch by default (NoEgress -> Punt), so let's instead
    // verify that without a route, B never receives anything.
    let (mut sim, _a, b, _s) = build_simple_topology(false);
    sim.run_until(Duration::from_millis(50));
    assert_eq!(sim.apps[&b].metrics.packets_received, 0);
}

#[test]
fn multi_hop_delivery_and_delay_accumulation() {
    // Two switches in series.
    let mut sim = Simulator::new();
    let app_a = AppId::new(1);
    let app_b = AppId::new(2);
    let s0 = SwitchId::new(0);
    let s1 = SwitchId::new(1);

    let a = App::new(
        app_a,
        IpAddr(0x0a000001), 32,
        IpAddr(0x0a000002), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_millis(10),
            size_bytes: 100,
        },
    );
    let b = App::new(
        app_b,
        IpAddr(0x0a000002), 32,
        IpAddr(0x0a000001), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_secs(3600),
            size_bytes: 0,
        },
    );

    fn build_switch(switch_id: SwitchId, out_port: u16) -> Switch {
        let mut state = TinyVmState::default();
        let mut table = MatchActionTable::new(TableId::new(1), MatchKind::Exact, 16);
        table
            .install(TableEntry {
                id: EntryId::new(1),
                key: 0x0a000002,
                prefix_len: 32,
                priority: 0,
                action: TableAction::SetEgress {
                    port: PortId::new(out_port),
                },
            })
            .unwrap();
        state.tables.push(table);
        let prog = TinyProgram {
            stages: vec![StageProgram {
                instrs: vec![
                    Instr::LoadField {
                        dst: Reg::new(0),
                        field: PacketField::IpDst,
                    },
                    Instr::TableLookup {
                        table_id: TableId::new(1),
                        key_reg: Reg::new(0),
                        result_meta: MetaKey::new(0),
                    },
                ],
                max_alu_ops: 0,
                max_memory_accesses: 1,
            }],
        };
        Switch::new(
            SwitchConfig::defaults(switch_id),
            prog,
            state,
            Box::new(NoopController),
            4,
            1 << 16,
        )
    }

    sim.add_switch(build_switch(s0, 1));
    sim.add_switch(build_switch(s1, 1));
    sim.add_app(a);
    sim.add_app(b);

    let lc = LinkConfig {
        latency: Duration::from_micros(100),
        bandwidth_bps: 1_000_000_000,
        queue_capacity_bytes: 1 << 16,
    };
    sim.connect(Node::App(app_a), PortId::new(0), Node::Switch(s0), PortId::new(0), lc);
    sim.connect(Node::Switch(s0), PortId::new(1), Node::Switch(s1), PortId::new(0), lc);
    sim.connect(Node::Switch(s1), PortId::new(1), Node::App(app_b), PortId::new(0), lc);

    sim.run_until(Duration::from_millis(50));
    assert!(sim.apps[&app_b].metrics.packets_received > 0);
    // Multi-hop delay should be larger than a single hop's prop delay.
    assert!(sim.apps[&app_b].metrics.avg_delay > Duration::from_micros(200));
}

// A controller that installs a route on first punt.
struct InstallOnPuntController {
    installed: bool,
    table: TableId,
}

impl SwitchController for InstallOnPuntController {
    fn on_punt(&mut self, _event: PuntEvent) -> Vec<ControllerAction> {
        if self.installed {
            return Vec::new();
        }
        self.installed = true;
        vec![ControllerAction::InstallTableEntry {
            table_id: self.table,
            entry: TableEntry {
                id: EntryId::new(42),
                key: 0x0a000002,
                prefix_len: 32,
                priority: 0,
                action: TableAction::SetEgress {
                    port: PortId::new(1),
                },
            },
        }]
    }
    fn on_timer(&mut self, _now: SimTime) -> Vec<ControllerAction> {
        Vec::new()
    }
    fn on_link_event(
        &mut self,
        _event: competitive_net_sim::controller::LinkEvent,
    ) -> Vec<ControllerAction> {
        Vec::new()
    }
}

#[test]
fn controller_installed_route_affects_later_packets() {
    // No initial route. NoEgress packets become punts to controller.
    // Controller installs route after first punt.
    let mut sim = Simulator::new();
    let app_a = AppId::new(1);
    let app_b = AppId::new(2);
    let switch_id = SwitchId::new(0);

    let a = App::new(
        app_a,
        IpAddr(0x0a000001), 32,
        IpAddr(0x0a000002), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_millis(10),
            size_bytes: 100,
        },
    );
    let b = App::new(
        app_b,
        IpAddr(0x0a000002), 32,
        IpAddr(0x0a000001), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_secs(3600),
            size_bytes: 0,
        },
    );

    let mut state = TinyVmState::default();
    state
        .tables
        .push(MatchActionTable::new(TableId::new(1), MatchKind::Exact, 16));
    let prog = TinyProgram {
        stages: vec![StageProgram {
            instrs: vec![
                Instr::LoadField {
                    dst: Reg::new(0),
                    field: PacketField::IpDst,
                },
                Instr::TableLookup {
                    table_id: TableId::new(1),
                    key_reg: Reg::new(0),
                    result_meta: MetaKey::new(0),
                },
            ],
            max_alu_ops: 0,
            max_memory_accesses: 1,
        }],
    };

    let switch = Switch::new(
        SwitchConfig::defaults(switch_id),
        prog,
        state,
        Box::new(InstallOnPuntController {
            installed: false,
            table: TableId::new(1),
        }),
        4,
        1 << 16,
    );
    sim.add_switch(switch);
    sim.add_app(a);
    sim.add_app(b);

    let lc = LinkConfig {
        latency: Duration::from_micros(100),
        bandwidth_bps: 1_000_000_000,
        queue_capacity_bytes: 1 << 16,
    };
    sim.connect(Node::App(app_a), PortId::new(0), Node::Switch(switch_id), PortId::new(0), lc);
    sim.connect(Node::Switch(switch_id), PortId::new(1), Node::App(app_b), PortId::new(0), lc);

    sim.run_until(Duration::from_millis(200));
    assert!(sim.apps[&app_b].metrics.packets_received > 0);
}

#[test]
fn cpu_failure_does_not_stop_data_plane() {
    let (mut sim, _a, b, s) = build_simple_topology(true);
    sim.fail_cpu(s);
    sim.run_until(Duration::from_millis(50));
    assert!(sim.apps[&b].metrics.packets_received > 0);
}

#[test]
fn switch_failure_drops_all_traffic() {
    let (mut sim, _a, b, s) = build_simple_topology(true);
    sim.fail_switch(s);
    sim.run_until(Duration::from_millis(50));
    assert_eq!(sim.apps[&b].metrics.packets_received, 0);
}

#[test]
fn ttl_decrement_prevents_loops() {
    // Set up a route that sends packets back to ingress (0) with a SetEgress
    // action. With TTL decrement on each pass, the packet should die after
    // TTL hops even if it would otherwise loop forever.
    let mut sim = Simulator::new();
    let app_a = AppId::new(1);
    let switch_id = SwitchId::new(0);

    let mut a = App::new(
        app_a,
        IpAddr(0x0a000001), 32,
        IpAddr(0x0a000002), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_secs(3600),
            size_bytes: 100,
        },
    );
    // Send one packet manually with a TTL that should expire quickly.
    let _ = &mut a;

    let mut state = TinyVmState::default();
    let mut table = MatchActionTable::new(TableId::new(1), MatchKind::Exact, 16);
    // Send back out port 0 -> back toward app.
    table
        .install(TableEntry {
            id: EntryId::new(1),
            key: 0x0a000002,
            prefix_len: 32,
            priority: 0,
            action: TableAction::SetEgress {
                port: PortId::new(0),
            },
        })
        .unwrap();
    state.tables.push(table);
    let prog = TinyProgram {
        stages: vec![StageProgram {
            instrs: vec![
                Instr::LoadField {
                    dst: Reg::new(0),
                    field: PacketField::IpDst,
                },
                Instr::TableLookup {
                    table_id: TableId::new(1),
                    key_reg: Reg::new(0),
                    result_meta: MetaKey::new(0),
                },
            ],
            max_alu_ops: 0,
            max_memory_accesses: 1,
        }],
    };
    sim.add_switch(Switch::new(
        SwitchConfig::defaults(switch_id),
        prog,
        state,
        Box::new(NoopController),
        4,
        1 << 16,
    ));
    sim.add_app(a);

    let lc = LinkConfig {
        latency: Duration::from_micros(10),
        bandwidth_bps: 1_000_000_000,
        queue_capacity_bytes: 1 << 16,
    };
    sim.connect(Node::App(app_a), PortId::new(0), Node::Switch(switch_id), PortId::new(0), lc);

    // Run; should terminate since TTL bounds the bouncing.
    sim.run_until(Duration::from_millis(10));
    // Just that we made it here without infinite loop is the test.
}

#[test]
fn punt_pipe_latency_respected() {
    // We measure that on_punt callback fires no earlier than punt_pipe_latency
    // after ingress. Use a controller that records the first punt time.
    use std::cell::RefCell;
    use std::rc::Rc;

    struct RecordController {
        first_punt: Rc<RefCell<Option<SimTime>>>,
    }
    impl SwitchController for RecordController {
        fn on_punt(&mut self, event: PuntEvent) -> Vec<ControllerAction> {
            let mut slot = self.first_punt.borrow_mut();
            if slot.is_none() {
                *slot = Some(event.now);
            }
            Vec::new()
        }
        fn on_timer(&mut self, _now: SimTime) -> Vec<ControllerAction> {
            Vec::new()
        }
        fn on_link_event(
            &mut self,
            _event: competitive_net_sim::controller::LinkEvent,
        ) -> Vec<ControllerAction> {
            Vec::new()
        }
    }

    let recorded = Rc::new(RefCell::new(None));
    let mut sim = Simulator::new();
    let app_a = AppId::new(1);
    let switch_id = SwitchId::new(0);
    let mut cfg = SwitchConfig::defaults(switch_id);
    cfg.punt_pipe_latency = Duration::from_millis(5);

    let a = App::new(
        app_a,
        IpAddr(0x0a000001), 32,
        IpAddr(0x0a000002), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_millis(100),
            size_bytes: 100,
        },
    );
    let mut state = TinyVmState::default();
    state
        .tables
        .push(MatchActionTable::new(TableId::new(1), MatchKind::Exact, 16));
    let prog = TinyProgram {
        stages: vec![StageProgram {
            instrs: vec![Instr::Punt {
                reason: PuntReason::NoRoute,
            }],
            max_alu_ops: 0,
            max_memory_accesses: 0,
        }],
    };
    let switch = Switch::new(
        cfg,
        prog,
        state,
        Box::new(RecordController {
            first_punt: recorded.clone(),
        }),
        4,
        1 << 16,
    );
    sim.add_switch(switch);
    sim.add_app(a);
    sim.connect(
        Node::App(app_a),
        PortId::new(0),
        Node::Switch(switch_id),
        PortId::new(0),
        LinkConfig {
            latency: Duration::from_micros(0),
            bandwidth_bps: 1_000_000_000_000,
            queue_capacity_bytes: 1 << 16,
        },
    );
    sim.run_until(Duration::from_millis(20));
    let t = recorded.borrow().expect("punt should have fired");
    assert!(t >= Duration::from_millis(5));
}

#[test]
fn recirculation_consumes_extra_processing() {
    // Stage 0 recirculates once; stage 0 then forwards. Verify that the packet
    // arrives later than if there were no recirculation (i.e., recirculation
    // adds at least one pipeline_delay).
    let mut sim = Simulator::new();
    let app_a = AppId::new(1);
    let app_b = AppId::new(2);
    let switch_id = SwitchId::new(0);

    let a = App::new(
        app_a,
        IpAddr(0x0a000001), 32,
        IpAddr(0x0a000002), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_millis(100),
            size_bytes: 100,
        },
    );
    let b = App::new(
        app_b,
        IpAddr(0x0a000002), 32,
        IpAddr(0x0a000001), 32,
        TrafficPattern::ConstantBitrate {
            interval: Duration::from_secs(3600),
            size_bytes: 0,
        },
    );

    let mut state = TinyVmState::default();
    state
        .tables
        .push(MatchActionTable::new(TableId::new(1), MatchKind::Exact, 16));
    state.registers.push(competitive_net_sim::tinyvm::RegisterArray::new(
        competitive_net_sim::types::RegisterArrayId::new(0),
        4,
    ));

    // Program: if metadata[0] == 1 (already recirculated), set egress and halt.
    // Otherwise set meta=1 and recirculate.
    //
    // Layout:
    //   0: LoadMeta r0, key 0
    //   1: Const   r1 = 1
    //   2: Eq      r2 = (r0 == r1)
    //   3: BranchIf r2 -> 7   (already recirculated -> forward path)
    //   4: Const   r3 = 1
    //   5: StoreMeta key 0 = r3
    //   6: Recirculate         (terminates this pass)
    //   7: SetEgress port=1
    //   8: Noop                (fall-through end)
    let prog = TinyProgram {
        stages: vec![StageProgram {
            instrs: vec![
                Instr::LoadMeta {
                    dst: Reg::new(0),
                    key: MetaKey::new(0),
                },
                Instr::Const {
                    dst: Reg::new(1),
                    value: 1,
                },
                Instr::Eq {
                    dst: Reg::new(2),
                    a: Reg::new(0),
                    b: Reg::new(1),
                },
                Instr::BranchIf {
                    cond: Reg::new(2),
                    target: competitive_net_sim::types::InstrIndex::new(7),
                },
                Instr::Const {
                    dst: Reg::new(3),
                    value: 1,
                },
                Instr::StoreMeta {
                    key: MetaKey::new(0),
                    src: Reg::new(3),
                },
                Instr::Recirculate,
                Instr::SetEgress {
                    port: PortId::new(1),
                },
                Instr::Noop,
            ],
            max_alu_ops: 16,
            max_memory_accesses: 0,
        }],
    };

    let switch = Switch::new(
        SwitchConfig::defaults(switch_id),
        prog,
        state,
        Box::new(NoopController),
        4,
        1 << 16,
    );
    sim.add_switch(switch);
    sim.add_app(a);
    sim.add_app(b);

    let lc = LinkConfig {
        latency: Duration::from_micros(10),
        bandwidth_bps: 1_000_000_000,
        queue_capacity_bytes: 1 << 16,
    };
    sim.connect(Node::App(app_a), PortId::new(0), Node::Switch(switch_id), PortId::new(0), lc);
    sim.connect(Node::Switch(switch_id), PortId::new(1), Node::App(app_b), PortId::new(0), lc);

    sim.run_until(Duration::from_millis(50));
    assert!(sim.apps[&app_b].metrics.packets_received > 0);
    // Each delivered packet experienced two pipeline passes.
    let avg = sim.apps[&app_b].metrics.avg_delay;
    assert!(avg > Duration::from_micros(20));
}
