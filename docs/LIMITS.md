# Limits

Your program runs inside a sandbox with hard ceilings. They are the same
for everyone, and the grader uses these exact numbers.

## Per call into your program

| Limit | Value | What happens if you exceed it |
|-------|-------|-------------------------------|
| Instruction budget ("fuel") | 4,000,000 per call | The call traps. Your program is marked failed and stops being called for the rest of the run. |
| Returned actions, encoded | 4,096 bytes | The call is rejected with an output-too-large error. |

The budget is **per call**, not per run. `init`, each `on_punt` and each
`on_timer` gets its own 4,000,000. Spending it is free; overrunning it is
fatal, so keep per-call work bounded — a loop over every switch you know
about is fine, and on a 15-switch world a loop over every pair of them
still fits with room to spare.

The ceiling is set well above what a working solution needs, so you should
not have to think about it. If you are hitting it, the problem is almost
certainly an accidental unbounded loop, not honest work.

4,096 bytes is roughly 150 table-entry installs in one return. If you need
more, install them across several calls.

## When you break one of these

The simulator tells you. If your program is killed mid-run you get a line
naming the switch, the simulation time, and which handler was running:

```text
program failed: switch 7 stopped at t=3400 ms in on_timer
  ran out of fuel -- this one call used more than 4,000,000 instructions.
  That is the per-call ceiling, not a budget for the whole run, so the
  cause is almost always a loop that never ends.
```

Read that before you read the report card. A program that stopped early
keeps forwarding on the routes it had at the moment it died, and nothing
updates them afterwards. From the outside that looks exactly like a
routing bug — stale routes, no recovery after a link dies — so it is easy
to spend a long time fixing code that was never wrong.

Running out of fuel and panicking are reported differently, because the
fix is different. "Ran out of fuel" means a loop that does not end.
"Trapped" means the handler panicked: an `unwrap` on `None`, an index past
the end of a slice, a divide by zero.

## What `init` may declare

These are checked once, when your program is installed. Declaring more than
any of them means your program does not load at all, and the error names
what was over.

| Limit | Value |
|-------|-------|
| Tables | 16 |
| Entries per table | 1,024 |
| Register arrays | 16 |
| Slots per register array | 4,096 |
| Counter arrays | 16 |
| Slots per counter array | 4,096 |

For scale: A1 worlds have between 5 and 15 switches, and one customer app
on each. A correct solution needs tens of routes, not thousands.

## Per stage, in the data plane

Each TinyVM stage declares its own budget when you build it:

- `max_alu_ops` — arithmetic and logic operations the stage may run.
- `max_memory_accesses` — table lookups, register and counter touches.

These are yours to choose, but the simulator validates the stage against
them before the run and rejects a program that cannot fit.

## Not limited

Wall-clock time, and how many packets you send. Control traffic you invent
is never counted as workload delivery — but it does consume link bandwidth
like anything else, so flooding is not free.
