# Where hyper-multilog came from

- **Not moved.** No file of this crate is another project's file moved. It was designed from slates'
  MLRaft layer, the hyper-raft core and the literature (`docs/multilog.md`, sources in
  `docs/research/multilog.md`), and written here, in the steps `docs/multilog.md` §11 names.
- **Why.** mantle note 32 (`docs/research/32-shared-transport-and-raft.md`) R25: slates' MLRaft
  becomes a shared layer over the shared core, for any owner whose group's one log is its limit.
- **slates' layer read.** `crates/cluster/src/multilog.rs`, `tests/multilog.rs` and
  `tests/multilog_timed.rs` of `github.com/hyper-light/slates` at
  `5cce86aa4cae58b421d171756b24e4305797a3e8` (the revision `hyper-raft-compare` pins), identical at
  `c7a2b75`.

## What came from where

| Piece | From | Here |
|---|---|---|
| A group's log divided into `n` logs over one set of voters; keyed commands by key, global ones in log 0, barriers in the other logs | MLRaft (Wang et al., EITCE '22), as slates built it | `docs/multilog.md` §2–§3, `src/route.rs`, `src/entry.rs` |
| The order applied: keyed in log order and epoch, a global once every other log stands at a barrier naming it | slates' merge | restated as the order `≺` and proved deterministic and linearizable (§4, Lamport's command histories, Herlihy and Wing); a barrier passes once log 0's merge has consumed the index it names, which equals slates' rule for a barrier naming a global and cannot hold a log forever where none is (§4.1); `src/merge.rs` |
| Routing: SplitMix64's finalizer of the key, modulo `n` | slates (`mix`) | the same function (`src/route.rs`); slates' routing test held each log above a margin, this crate holds the exact counts the function gives |
| Entries out of place | slates passes them over and applies a keyed entry in whatever log holds it | refused alike on every member and reported, never applied: a misrouted key's commands would not commute (§2.2) |
| Who appends barriers | each log's leader (slates) | every member proposes the barriers it owes and a follower's are forwarded, so a barrier passes a leader cut off from log 0's leader; a leader drops a forwarded barrier it covers (§3.1) |
| Leaders spread over the voters | slates' ranked voters and priority elections | the core's priority and transfers, ranked by the owner (`MultiLog::spread`, §7) |
| A member cut from another log's leader | not handled (slates) | yields what it leads when cut from a lower log's leader and is not handed it back, found by the hostile-network measurement (`docs/multilog.md` §7.1) |
| A batch of commands | one proposal a log (slates) | `MultiLog::propose_in`, one proposal of the core a batch (§9) |
| Changes of configuration | applied as merged (slates) | applied as given to apply, so a log never waits on the merge to elect (§3.4) |
| Images and restart | slates' retained state, no compaction across logs (owed in its notes) | cuts every member can reach and install, images at canonical cuts, `Point` (§5–§6, `src/point.rs`) |
| What a log holds past its merge | unbounded (slates) | `Limits::unmerged`, refused at the leader with a typed error (§8) |
| `n = 1` | slates: "today's single log" | the same code path, shown equal to the bare core (`tests/layer.rs`) |

## Tests

- `tests/multilog.rs`: slates' unit tests of its layer, each retargeted at this crate's API with the
  facts it asserts kept; where slates' test leaned on a rule this crate changed (a key applied in the
  log that does not own it), the test asserts this crate's rule and says so. slates' explorer
  (three voters × three logs and five × two, adversarial and calm stretches, crashes, a bag of
  messages) on hyper-sim's world and network, with `twice` and named coverage counters.
- `tests/multilog_timed.rs`: slates' timed simulation of five Azure regions on hyper-sim's ordered
  discipline, its margins replaced by exact facts.
- `tests/merge.rs`, `tests/layer.rs`, `tests/allocs.rs`: this crate's own.
