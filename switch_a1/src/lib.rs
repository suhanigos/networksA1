//! switch_a1 — link-state routing over the NetworkArena switch-program API.
//!
//! Design (see design-card.pdf for the version you'll defend at the quiz):
//!
//! - Neighbor discovery + liveness: periodic HELLO (proto 89) on every local
//!   port, ttl=1. A port that ever produced a HELLO is a switch-to-switch
//!   port; one that produces plain data traffic instead is the customer/app
//!   port. Liveness: if a port that was alive stops producing HELLOs for
//!   HELLO_TIMEOUT_NS, we declare it dead.
//! - Topology + prefix distribution: each switch floods its own "node state"
//!   (its live neighbor switch ids + its own customer prefix, if known) as
//!   an ADV packet (proto 90), tagged with a per-origin sequence number.
//!   Receivers keep only the newest state per origin and re-flood it out
//!   every port except the one it arrived on (classic link-state flooding).
//! - Routing: every switch runs BFS over the graph built from all node
//!   states it has collected, computing the first-hop neighbor toward every
//!   other switch. A destination's owning switch is looked up by prefix;
//!   the local egress port is "whatever port reaches that first hop."
//! - Data plane: two tables. T_PROTO (exact match on ip_proto) punts our own
//!   control traffic before it ever reaches routing. T_ROUTE (LPM on
//!   ip_dst) is the real forwarding table; a miss punts with NoRoute, which
//!   is how we learn our own prefix and how we backfill a route the first
//!   time we see traffic toward a prefix we already know the owner of.

use std::collections::HashMap;
use switch_program_sdk::*;

const T_PROTO: u32 = 1;
const T_ROUTE: u32 = 2;

const PROTO_HELLO: u8 = 89;
const PROTO_ADV: u8 = 90;

/// Address nothing routes; only used so the receiving switch's data plane
/// has something to put in ip_dst. T_PROTO catches these before T_ROUTE
/// ever looks at the destination.
const CTRL_IP: u32 = 0xe000_0005;

const TICK_NS: u64 = 50_000_000; // 50 ms
/// Declare a neighbor dead after this many ms of silence. Three missed
/// ticks gives ~150ms worst-case detection latency -- comfortably inside
/// the 1000ms recovery budget.
const HELLO_TIMEOUT_NS: u64 = 3 * TICK_NS;
/// Force a full re-advertisement of our own state this often, as a
/// backstop in case a flooded ADV was lost along the way.
const REFRESH_EVERY_TICKS: u32 = 20; // ~1s

#[derive(Clone, Copy)]
struct NeighborInfo {
    switch_id: u32,
    last_hello_ns: u64,
    alive: bool,
}

#[derive(Clone)]
struct NodeState {
    seq: u32,
    neighbors: Vec<u32>,
    prefix: Option<u32>,
}

pub struct SwitchA1 {
    switch_id: u32,
    local_ports: Vec<u16>,

    /// port -> who's on the other end of that cable, learned via HELLO.
    neighbors: HashMap<u16, NeighborInfo>,

    /// Our own customer prefix (network address, e.g. 0x0a005a00 for
    /// 10.0.90.0/24) and the port it lives behind. Learned the first time
    /// a data packet with no route arrives on a non-switch port.
    my_prefix: Option<u32>,
    my_prefix_port: Option<u16>,
    my_seq: u32,

    /// Latest known state per origin switch id (includes our own, so
    /// routing can treat every switch uniformly).
    node_state: HashMap<u32, NodeState>,

    /// What's actually installed in T_ROUTE right now: prefix -> port.
    routes_installed: HashMap<u32, u16>,

    tick_count: u32,
}

impl SwitchA1 {
    fn neighbor_port_for_switch(&self, switch_id: u32) -> Option<u16> {
        self.neighbors
            .iter()
            .find(|(_, info)| info.alive && info.switch_id == switch_id)
            .map(|(port, _)| *port)
    }

    /// BFS over the graph built from all collected node states. Returns,
    /// for every switch we can reach, the neighbor of ours that starts the
    /// shortest path to it.
    fn first_hops(&self) -> HashMap<u32, u32> {
        use std::collections::VecDeque;

        let mut first_hop: HashMap<u32, u32> = HashMap::new();
        let mut visited: HashMap<u32, bool> = HashMap::new();
        visited.insert(self.switch_id, true);

        let mut queue: VecDeque<u32> = VecDeque::new();
        queue.push_back(self.switch_id);

        while let Some(u) = queue.pop_front() {
            let neighbors_of_u = match self.node_state.get(&u) {
                Some(s) => &s.neighbors,
                None => continue,
            };
            for &v in neighbors_of_u {
                if visited.contains_key(&v) {
                    continue;
                }
                visited.insert(v, true);
                let fh = if u == self.switch_id { v } else { first_hop[&u] };
                first_hop.insert(v, fh);
                queue.push_back(v);
            }
        }

        first_hop
    }

    /// Recompute desired routes from current knowledge, diff against what's
    /// installed, and emit the minimal set of delete/install actions.
    fn recompute_routes(&mut self) -> Vec<Action> {
        let first_hop = self.first_hops();

        let mut desired: HashMap<u32, u16> = HashMap::new();
        if let (Some(prefix), Some(port)) = (self.my_prefix, self.my_prefix_port) {
            desired.insert(prefix, port);
        }
        for (&origin, state) in self.node_state.iter() {
            if origin == self.switch_id {
                continue;
            }
            let Some(prefix) = state.prefix else { continue };
            let Some(&fh) = first_hop.get(&origin) else { continue };
            let Some(port) = self.neighbor_port_for_switch(fh) else { continue };
            desired.insert(prefix, port);
        }

        let mut actions = Vec::new();

        for (&prefix, &port) in desired.iter() {
            if self.routes_installed.get(&prefix) != Some(&port) {
                actions.push(actions::delete_entry(T_ROUTE, prefix as u64));
                actions.push(actions::install_route(T_ROUTE, prefix as u64, prefix as u64, 24, port));
                self.routes_installed.insert(prefix, port);
            }
        }

        let stale: Vec<u32> = self
            .routes_installed
            .keys()
            .filter(|p| !desired.contains_key(p))
            .copied()
            .collect();
        for prefix in stale {
            actions.push(actions::delete_entry(T_ROUTE, prefix as u64));
            self.routes_installed.remove(&prefix);
        }

        actions
    }

    fn encode_adv(&self, origin: u32, seq: u32, neighbors: &[u32], prefix: Option<u32>) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + 4 + 2 + neighbors.len() * 4 + 1 + 4);
        buf.extend_from_slice(&origin.to_le_bytes());
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(&(neighbors.len() as u16).to_le_bytes());
        for n in neighbors {
            buf.extend_from_slice(&n.to_le_bytes());
        }
        match prefix {
            Some(p) => {
                buf.push(1);
                buf.extend_from_slice(&p.to_le_bytes());
            }
            None => {
                buf.push(0);
                buf.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        buf
    }

    fn decode_adv(payload: &[u8]) -> Option<(u32, u32, Vec<u32>, Option<u32>)> {
        if payload.len() < 10 {
            return None;
        }
        let origin = u32::from_le_bytes(payload[0..4].try_into().ok()?);
        let seq = u32::from_le_bytes(payload[4..8].try_into().ok()?);
        let count = u16::from_le_bytes(payload[8..10].try_into().ok()?) as usize;
        let mut off = 10;
        let mut neighbors = Vec::with_capacity(count);
        for _ in 0..count {
            if payload.len() < off + 4 {
                return None;
            }
            neighbors.push(u32::from_le_bytes(payload[off..off + 4].try_into().ok()?));
            off += 4;
        }
        if payload.len() < off + 1 + 4 {
            return None;
        }
        let has_prefix = payload[off];
        off += 1;
        let prefix_val = u32::from_le_bytes(payload[off..off + 4].try_into().ok()?);
        let prefix = if has_prefix == 1 { Some(prefix_val) } else { None };
        Some((origin, seq, neighbors, prefix))
    }

    /// Bump our own sequence number, flood our current state to every
    /// local port, fold it into our own node_state entry, and recompute.
    fn reflood_self_and_recompute(&mut self) -> Vec<Action> {
        self.my_seq += 1;
        let live_neighbors: Vec<u32> = self
            .neighbors
            .values()
            .filter(|n| n.alive)
            .map(|n| n.switch_id)
            .collect();

        self.node_state.insert(
            self.switch_id,
            NodeState {
                seq: self.my_seq,
                neighbors: live_neighbors.clone(),
                prefix: self.my_prefix,
            },
        );

        let payload = self.encode_adv(self.switch_id, self.my_seq, &live_neighbors, self.my_prefix);

        let mut actions = Vec::new();
        for &port in &self.local_ports {
            actions.push(actions::inject_packet(port, CTRL_IP, CTRL_IP, PROTO_ADV, 1, payload.clone()));
        }
        actions.extend(self.recompute_routes());
        actions
    }
}

impl SwitchProgram for SwitchA1 {
    fn init(switch_id: u32, local_ports: Vec<u16>) -> (Self, ProgramSetup) {
        let mut setup = ProgramSetup::new();

        setup.declare_table(T_PROTO, MatchKind::Exact, 8);
        setup.declare_table(T_ROUTE, MatchKind::Lpm, 256);

        setup.set_program(
            text::parse_tiny_program(
                "
                stage alu=4 mem=1
                    load    r0, ip_proto
                    table   t1, r0 -> m0
                stage alu=4 mem=1
                    load    r1, ip_dst
                    table   t2, r1 -> m1
                ",
            )
            .unwrap(),
        );

        setup.add_entry(
            T_PROTO,
            TableEntry {
                id: wire::EntryIdW(1),
                key: PROTO_HELLO as u64,
                prefix_len: 8,
                priority: 0,
                action: TableAction::Punt { reason: PuntReason::Custom(PROTO_HELLO as u32) },
            },
        );
        setup.add_entry(
            T_PROTO,
            TableEntry {
                id: wire::EntryIdW(2),
                key: PROTO_ADV as u64,
                prefix_len: 8,
                priority: 0,
                action: TableAction::Punt { reason: PuntReason::Custom(PROTO_ADV as u32) },
            },
        );

        let me = Self {
            switch_id,
            local_ports,
            neighbors: HashMap::new(),
            my_prefix: None,
            my_prefix_port: None,
            my_seq: 0,
            node_state: HashMap::new(),
            routes_installed: HashMap::new(),
            tick_count: 0,
        };
        (me, setup)
    }

    fn on_punt(&mut self, ev: PuntEvent) -> Vec<Action> {
        match ev.reason {
            PuntReason::Custom(p) if p == PROTO_HELLO as u32 => {
                if ev.payload.len() < 4 {
                    return Vec::new();
                }
                let neighbor_switch_id = u32::from_le_bytes(ev.payload[0..4].try_into().unwrap());
                let was_alive = self.neighbors.get(&ev.ingress_port).map(|n| n.alive).unwrap_or(false);
                self.neighbors.insert(
                    ev.ingress_port,
                    NeighborInfo { switch_id: neighbor_switch_id, last_hello_ns: ev.now_ns, alive: true },
                );
                if !was_alive {
                    return self.reflood_self_and_recompute();
                }
                Vec::new()
            }

            PuntReason::Custom(p) if p == PROTO_ADV as u32 => {
                let Some((origin, seq, neighbors, prefix)) = Self::decode_adv(&ev.payload) else {
                    return Vec::new();
                };
                if origin == self.switch_id {
                    return Vec::new();
                }
                let is_new = match self.node_state.get(&origin) {
                    Some(existing) => seq > existing.seq,
                    None => true,
                };
                if !is_new {
                    return Vec::new();
                }
                self.node_state.insert(origin, NodeState { seq, neighbors, prefix });

                let mut actions = Vec::new();
                for &port in &self.local_ports {
                    if port == ev.ingress_port {
                        continue;
                    }
                    actions.push(actions::inject_packet(
                        port,
                        CTRL_IP,
                        CTRL_IP,
                        PROTO_ADV,
                        1,
                        ev.payload.clone(),
                    ));
                }
                actions.extend(self.recompute_routes());
                actions
            }

            PuntReason::NoRoute => {
                let mut actions = Vec::new();
                let src_prefix = ev.ip_src & 0xffff_ff00;
                let is_switch_port = self.neighbors.contains_key(&ev.ingress_port);

                if self.my_prefix.is_none() && !is_switch_port {
                    self.my_prefix = Some(src_prefix);
                    self.my_prefix_port = Some(ev.ingress_port);
                    actions.extend(self.reflood_self_and_recompute());
                }

                let dst_prefix = ev.ip_dst & 0xffff_ff00;
                if !self.routes_installed.contains_key(&dst_prefix) {
                    let owner_port = if self.my_prefix == Some(dst_prefix) {
                        self.my_prefix_port
                    } else {
                        self.node_state.iter().find_map(|(&origin, state)| {
                            if state.prefix == Some(dst_prefix) {
                                self.first_hops()
                                    .get(&origin)
                                    .and_then(|&fh| self.neighbor_port_for_switch(fh))
                            } else {
                                None
                            }
                        })
                    };
                    if let Some(port) = owner_port {
                        actions.push(actions::delete_entry(T_ROUTE, dst_prefix as u64));
                        actions.push(actions::install_route(T_ROUTE, dst_prefix as u64, dst_prefix as u64, 24, port));
                        self.routes_installed.insert(dst_prefix, port);
                    }
                }

                actions
            }

            _ => Vec::new(),
        }
    }

    fn on_timer(&mut self, ev: TimerEvent) -> Vec<Action> {
        self.tick_count += 1;
        let mut actions = Vec::new();

        let payload = self.switch_id.to_le_bytes().to_vec();
        for &port in &self.local_ports {
            actions.push(actions::inject_packet(port, CTRL_IP, CTRL_IP, PROTO_HELLO, 1, payload.clone()));
        }

        let mut dirty = false;
        for info in self.neighbors.values_mut() {
            if info.alive && ev.now_ns.saturating_sub(info.last_hello_ns) > HELLO_TIMEOUT_NS {
                info.alive = false;
                dirty = true;
            }
        }
        if self.tick_count % REFRESH_EVERY_TICKS == 0 {
            dirty = true;
        }
        if dirty {
            actions.extend(self.reflood_self_and_recompute());
        }

        actions.push(actions::schedule_timer(TICK_NS));
        actions
    }
}

switch_program!(SwitchA1);
