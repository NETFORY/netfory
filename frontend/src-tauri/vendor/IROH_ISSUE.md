# Issue draft for https://github.com/n0-computer/iroh/issues/new

**Title:** Unbounded (geometric) growth of `pending_open_paths` in `RemoteStateActor` -> multi‑GB allocations / OOM on idle connections

## Summary

`iroh::socket::remote_map::remote_state::State::open_path_on_conn` re‑queues an address into
`pending_open_paths` every time `open_path_ensure` fails with `PathError::RemoteCidsExhausted`
or `PathError::MaxPathIdReached`. The scheduled retry (`scheduled_open_path`, 333 ms) drains the
whole queue and calls `open_path_on_all_conns` for **every** queued address, i.e. once per
connection to that remote. With `K` live connections to the same remote, every failing address is
re‑queued `K` times per tick, so the queue grows as `K^n`. Within minutes the backing `VecDeque`
reallocates in doublings (320 MiB -> 640 MiB -> 1.25 GiB -> ... -> 160 GiB) until the allocator fails.

Observed in production on Windows 10/11 (Tauri 2 desktop app, iroh `1.0.0`; the same code is
present in `1.0.3`). RSS reached 100+ GB (with pagefile), then `ALLOCATION FAILURE: 171798691840
bytes requested`.

## Environment

- iroh `1.0.0` / `1.0.3` (`src/socket/remote_map/remote_state.rs`)
- Windows 10/11 x64, Rust stable, tokio multi‑thread runtime
- Several long‑lived QUIC connections to the same remote (gossip + blobs + app protocol), mostly
  idle, relay + direct paths

## Backtrace (captured from a custom `GlobalAlloc` hook on a 336 MB allocation)

```
alloc::collections::vec_deque::VecDeque<T,A>::grow
alloc::collections::vec_deque::VecDeque<T,A>::push_back
iroh::socket::remote_map::remote_state::State::open_path_on_conn
iroh::socket::remote_map::remote_state::RemoteStateActor::open_path_on_all_conns
iroh::socket::remote_map::remote_state::RemoteStateActor::run
tokio::runtime::task::core::Core<T,S>::poll
(thread: tokio-rt-worker)
```

Sizes of consecutive reallocations: 335544320, 671088640, 1342177280, 2684354560, 5368709120,
10737418240, 21474836480, 42949672960, 85899345920, 171798691840 (= 5*2^n bytes, i.e. a
`VecDeque<FourTuple>` with 40‑byte elements doubling from 2^23 to 2^32 entries).

## Reproduction (sketch)

1. Open ≥ 2 connections (different ALPNs) from one endpoint to the same remote and keep them idle
   for a long time so that path ids / remote CIDs get exhausted on at least one of them
   (`open_path_ensure` starts returning `MaxPathIdReached` / `RemoteCidsExhausted`).
2. Trigger address changes (relay/direct flapping, `handle_address_lookup_item`) so that
   `open_path_on_conn` is called for a new `FourTuple`.
3. Watch `pending_open_paths.len()` - it grows geometrically every 333 ms.

## Suggested fix

Deduplicate and bound the queue (this is what we ship as a local `[patch.crates-io]`):

```rust
Some(Err(PathError::RemoteCidsExhausted))
| Some(Err(PathError::MaxPathIdReached)) => {
    self.scheduled_open_path = Some(Instant::now() + Duration::from_millis(333));
    const MAX_PENDING_OPEN_PATHS: usize = 64;
    if !self.pending_open_paths.contains(open_addr) {
        if self.pending_open_paths.len() >= MAX_PENDING_OPEN_PATHS {
            self.pending_open_paths.pop_front();
        }
        self.pending_open_paths.push_back(open_addr.clone());
    }
    trace!(?open_addr, ?ret, "scheduling open_path");
}
```

A more thorough fix would make the retry loop re‑queue at most once per address per tick
(collect failures into a `HashSet<FourTuple>` in `open_path_on_all_conns`), and/or stop retrying a
connection that reports `MaxPathIdReached` (the error is permanent for that connection).
