# Design Card — switch_a1 (draft, for review before layout)

*One page at the quiz. Every line here should be something you can defend cold, from evidence, without this page reminding you what it means.*

---

## 1. One-line summary

Link-state routing over the switch API: HELLO discovers neighbors and doubles
as a heartbeat; flooded state advertisements (ADV) give every switch the full
topology + prefix ownership; each switch runs its own BFS to pick a next-hop
port; the data plane is two dumb tables the control plane keeps filled in.

## 2. Protocol overview

```
   HELLO (proto 89, ttl=1)         ADV (proto 90, ttl=1)
   sent every 50ms, every port     flooded on change + every ~1s
   payload: switch_id (4B)         payload: origin(4B) seq(4B)
                                    neighbor_count(2B) neighbors(4B each)
   -> tells me who is on the       has_prefix(1B) prefix_network(4B)
      other end of each cable
   -> silence >150ms (3 ticks)     -> "here's my current live neighbor
      = link is dead                  list + my customer prefix if known"
                                    -> receiver keeps newest seq per
                                       origin, re-floods it onward
                                       (all ports except where it came
                                       from), then recomputes routes
```

**Prefix learning**: a switch has no idea what's behind it. The first data
packet with no route (`PuntReason::NoRoute`) that arrives on a port that has
*never* produced a HELLO is assumed to come from our own customer app — we
read its source `/24` off the packet, remember it as ours, and fold it into
our next ADV.

**Routing**: BFS from self over the graph built from every ADV we've
collected (including our own). For each destination switch we get "which of
*my* neighbors starts the shortest path there"; that neighbor's port is our
egress port for that switch's advertised prefix.

## 3. Data plane (two tables, T_PROTO then T_ROUTE, in order)

| Table | Kind | Key | Purpose |
|---|---|---|---|
| T_PROTO (id 1) | Exact | `ip_proto` | catches 89 (HELLO) and 90 (ADV) before routing ever sees them; both punt with `Custom(proto)` |
| T_ROUTE (id 2) | LPM | `ip_dst`, /24 | the real forwarding table; entry id = the prefix's network address itself (no separate id bookkeeping needed); miss -> punt `NoRoute` |

Route entries are always **delete-then-install** on change, never a bare
install over an existing id — `install()` just appends, so leaving the old
entry in place risks it winning the match ahead of the new one.

## 4. Key parameters and why

| Parameter | Value | Why |
|---|---|---|
| HELLO tick | 50ms | frequent enough for fast failure detection, negligible vs. 1Gbps link capacity |
| Dead-neighbor threshold | 3 missed ticks (~150ms) | balances "don't declare dead on one lost packet" against the 1000ms recovery budget |
| ADV periodic refresh | every 20 ticks (~1s) | backstop in case a flooded ADV is itself lost on a bad link; self-heals stale state |
| Measured recovery time (practice, all 15 Part 2 runs) | ~150-200ms worst case | detection (~150ms) + flood/recompute (sub-ms) dominates; matches the ~200ms course reference |

## 5. Known limitation: transient microloop during convergence

Observed in `part2-ring-010-f001`: after link 2-14 failed, switch 2 and
switch 10 each independently recomputed their route to switch 14's prefix
and installed their new tables **123 microseconds apart**. For that window,
switch 2's new route pointed *through* switch 10, while switch 10's
not-yet-updated route still pointed *through* switch 2 — a two-node
ping-pong. One in-flight packet bounced once (`s10->s2->s10`) before
escaping on switch 10's very next (now-correct) lookup.

- **Why it happens**: every switch installs its own new forwarding decision
  the instant *its own* flood-driven recompute fires, with zero coordination
  on timing with any other switch. Once every switch has heard the same
  news the shortest-path result is globally consistent and loop-free by
  construction — the loop only exists in the seam while the flood is still
  propagating.
- **Why it's not a full loop**: it self-corrects within microseconds because
  convergence is fast; TTL decrement is the real backstop if it ever didn't.
- **How I'd patch it** (not done — not graded until A2): make-before-break —
  hold the old route a moment past when the new one is computed, or delay
  installing a next-hop flip until neighbors have had time to converge too.

## 6. Reading evidence at the quiz — cheat sheet

- **"Which path is traffic on?"** Look up the destination `/24` in T_ROUTE ->
  `SetEgress{port}`. Cross-reference that port against the neighbor table
  (learned via HELLO) to name the next switch. Repeat at that switch.
- **"Why did switch X do Y at time T?"** Check what ADV/HELLO it had most
  recently received before T — its decision is a pure function of
  `node_state` at that instant, which is itself just "newest seq per origin
  I've heard."
- **"What if link A-B had failed instead of C-D?"** Re-derive the new
  shortest-path tree by hand: remove that edge from the topology, BFS from
  the affected switch again. Any switch whose shortest path used that edge
  reroutes; nothing else changes.
- **"How would you break this network?"** Two ideas to have ready: (1) flood
  a forged ADV with a very high sequence number for someone else's switch id
  — there's no authentication, so it would be believed and could blackhole
  that switch's prefix; (2) fail two links whose repair windows overlap in a
  way that temporarily disconnects a switch from everyone — the assignment
  promises this won't happen in grading, but it's the honest answer to "what
  would break your design."

---

*Fill in before printing: team/group name, switch_id -> owner-port cheat
notes if useful, and anything you personally keep forgetting under quiz
pressure.*
