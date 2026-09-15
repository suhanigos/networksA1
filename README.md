# NetworkArena — Assignment 1 starter kit

You write one program. It runs on every switch you own. We drop it into
networks it has never seen and check whether the traffic gets through.

The assignment text and the grading rules are in the handout your instructor
gave you. They are not in this kit.

## Layout

```
src/  switch_program_types/  switch_program_sdk/   the simulator and the SDK
examples/hello_switch/       start here: it runs, forwards nothing, scores 0%
examples/learning_switch/    55 lines: installs one route on the first punt
worlds/practice/             5 topologies, each as a Part 1 and a Part 2 world
worlds/practice/failures/    15 failure schedules, 3 per Part 2 world
viewer/                      browser replay for any run you log
docs/                        simulator reference
```

## Setup

```sh
rustup target add wasm32-unknown-unknown
cargo build --release
```

## Write your program

Copy `examples/hello_switch/`, then edit its `src/lib.rs`. You implement three
functions:

```rust
impl SwitchProgram for MyProgram {
    fn init(switch_id: u32, local_ports: Vec<u16>) -> (Self, ProgramSetup);
    fn on_punt(&mut self, ev: PuntEvent) -> Vec<Action>;
    fn on_timer(&mut self, ev: TimerEvent) -> Vec<Action>;
}
switch_program!(MyProgram);
```

`init` returns your data-plane program and its table declarations. `on_punt`
fires when a packet finds no matching route. `on_timer` fires when you asked
it to. Nothing else wakes you up.

Build it:

```sh
cd examples/hello_switch
cargo build --release --target wasm32-unknown-unknown
# -> target/wasm32-unknown-unknown/release/hello_switch.wasm
```

## Run it

Part 1 — no failures:

```sh
./target/release/competitive_net_sim run-world \
  worlds/practice/practice-ring-002.toml \
  --program examples/hello_switch/target/wasm32-unknown-unknown/release/hello_switch.wasm \
  --score
```

Part 2 — the same network, now with links dying:

```sh
./target/release/competitive_net_sim run-world \
  worlds/practice/part2-ring-002.toml \
  --failures worlds/practice/failures/part2-ring-002-f001.toml \
  --program YOUR.wasm \
  --score
```

Both print the report card we grade with, and exit non-zero when it fails.

Sweep every Part 1 world:

```sh
for w in worlds/practice/practice-*.toml; do
  ./target/release/competitive_net_sim run-world "$w" --program YOUR.wasm --score
done
```

**The two parts share their five networks.** `practice-ring-002` and
`part2-ring-002` are the same topology, the same switches, the same customer
prefixes; the Part 2 twin just runs longer and has links dying under it. So
when recovery misbehaves, run the Part 1 twin first: if that one is clean,
your routing is fine and the bug is in how you notice and route around
silence. It is the cheapest bisect you have.

## Watch a run

Add `--log run.simlog` to any command above, then serve the viewer over HTTP
(opening the file directly will not work — it loads wasm as a module):

```sh
python3 -m http.server -d viewer/web 8080
```

Open <http://localhost:8080> and drag `run.simlog` onto the page.

## Reference

| File | What it covers |
|------|----------------|
| `docs/switch-programs.md` | Your API: the SDK, the TinyVM text format, the raw wasm ABI. |
| `docs/tinyvm.md` | The data-plane instruction set. |
| `docs/sim_log.md` | The `.simlog` format and the viewer. |
| `docs/programming-a-switch.md`, `docs/building-a-network.md` | How the simulator is built. Not your API. |
