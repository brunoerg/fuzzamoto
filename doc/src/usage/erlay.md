# Fuzzing Erlay

The `erlay` feature configures the IR scenario for Bitcoin Core PR 35591. It adds an
`outbound-full-recon` connection so Bitcoin Core is exercised as the reconciliation initiator,
while the existing inbound Erlay connections exercise it as the responder.

The feature also enables protocol-aware generation for `sendtxrcncl`, `reqtxrcncl`, `sketch`,
`reqsketchext`, and `reconcildiff`. Generated payloads include valid encodings, malformed
CompactSize values, protocol boundary values, and short request/extension/finalization sequences.

## Build

Build the PR with ASan instrumentation using the existing LibAFL image:

```sh
docker build --build-arg PR_NUMBER=35591 \
  -f Dockerfile.libafl -t fuzzamoto-erlay .
docker run --privileged -it -v "$PWD:/fuzzamoto" fuzzamoto-erlay bash
```

Inside the container, build Fuzzamoto with Erlay enabled:

```sh
cd /fuzzamoto
BITCOIND_PATH=/bitcoin/build_fuzz/bin/bitcoind \
  cargo build --workspace --release --features fuzz,erlay
```

Build the crash handler and initialize the Nyx share directory as described in the regular
LibAFL documentation:

```sh
clang-19 -fPIC -DENABLE_NYX -D_GNU_SOURCE -DNO_PT_NYX \
  ./fuzzamoto-nyx-sys/src/nyx-crash-handler.c -ldl -I. -shared \
  -o libnyx_crash_handler.so

./target/release/fuzzamoto-cli init \
  --sharedir /tmp/fuzzamoto-erlay \
  --crash-handler /fuzzamoto/libnyx_crash_handler.so \
  --bitcoind /bitcoin/build_fuzz/bin/bitcoind \
  --scenario ./target/release/scenario-ir \
  --nyx-dir ./target/release/
```

Initialization writes the matching `ir.context`. Generate an initial corpus containing Erlay
messages, transactions, and time advancement so reconciliation sets and scheduled outbound rounds
are reachable from the start of the campaign:

```sh
mkdir -p /tmp/erlay-in /tmp/erlay-out
./target/release/fuzzamoto-cli ir generate \
  --output /tmp/erlay-in \
  --iterations 12 \
  --programs 512 \
  --context ./ir.context \
  --generators ErlayMessageGenerator,SingleTxGenerator,OneParentOneChildGenerator,AdvanceTimeGenerator
```

Run the normal crash and state-machine campaign:

```sh
./target/release/fuzzamoto-libafl \
  --input /tmp/erlay-in \
  --output /tmp/erlay-out \
  --share /tmp/fuzzamoto-erlay \
  --cores 0-15
```

Run a separate build with `v2transport` added to the scenario features to cover the same protocol
over BIP324. Use a separate share and output directory because the VM snapshot differs.

## Resource-exhaustion campaign

Maximum-capacity sketch decoding is intentionally excluded from the normal mutation schedule. It
can take multiple seconds per message and would otherwise dominate execution time and corpus
selection.

Generate a separate corpus for capacity boundaries:

```sh
mkdir -p /tmp/erlay-dos-in /tmp/erlay-dos-out
./target/release/fuzzamoto-cli ir generate \
  --output /tmp/erlay-dos-in \
  --iterations 10 \
  --programs 1024 \
  --context ./ir.context \
  --generators ErlayExpensiveSketchGenerator,SingleTxGenerator,AdvanceTimeGenerator
```

Run it on a small number of cores with hang reporting enabled:

```sh
./target/release/fuzzamoto-libafl \
  --input /tmp/erlay-dos-in \
  --output /tmp/erlay-dos-out \
  --share /tmp/fuzzamoto-erlay \
  --cores 0-1 \
  --timeout 1000 \
  --hang-multiple 5 \
  --mutators InputMutator,OperationMutator,ErlayExpensiveSketchGenerator,SingleTxGenerator,AdvanceTimeGenerator
```

Treat expected maximum-sketch timeouts separately from deadlocks. Re-run minimized hangs with a
larger timeout and inspect whether the node eventually answers pings from other peers.

## Troubleshooting process creation

`Failed to create Nyx PROCESS` happens before any fuzz input is compiled or executed. It normally
means the VM boot script exited before initializing the fuzzing agent; it is not caused by an
individual Erlay testcase.

Check the following in order:

1. The workspace was built with both features, exactly as above: `--features fuzz,erlay`. The
   `fuzz` feature enables the Nyx agent; the `erlay` feature only selects the PR connection
   topology and protocol generator.
2. The Docker container was started with `--privileged`, so `/dev/kvm` is available to Nyx.
3. The share directory used by `--share` was created by the current `fuzzamoto-cli init` and the
   output directory starts with previous campaign output instead of deleting state.
4. The Bitcoind binary in the share is the PR 35591 image build. Without the PR, the Erlay
   snapshot's `outbound-full-recon` connection fails during initialization. Use a fresh container
   image after changing the source, Docker arguments, or Fuzzamoto feature flags.
5. Do not stop at `invalid path to external symbolizer`. That warning only means the guest cannot
   resolve the stack; the immediately preceding ASan/QEMU error is the actual fault.

A host-side reproduction must use the absolute paths of that host, not the Docker image's
`/fuzzamoto` paths. The Nyx guest utilities and QEMU binary also have to match the host glibc
version; otherwise QEMU can boot but the checked-in `hcat`/`habort` helpers abort before the
scenario initializes.

For the shortest diagnostic run, use one worker and retain fuzzer console output:

```sh
RUST_LOG=debug RUST_BACKTRACE=full ./target/release/fuzzamoto-libafl \
  --input /tmp/erlay-in \
  --output /tmp/erlay-debug \
  --share /tmp/fuzzamoto-erlay \
  --cores 0
```

Inspect the oldest error in `/tmp/erlay-debug/workdir/dump` and, if `nyx_log` was enabled at
initialization, `primary.log`. A missing `llvm-symbolizer` only removes source-level names from
that log; it does not prevent `bitcoind` or the scenario from starting.

## Coverage goals

A useful campaign should reach all of the following:

- Negotiation success, ignored negotiation, duplicate negotiation, and disconnect cleanup.
- Both initiator and responder roles over v1 and v2 transport.
- Initial reconciliation success, failure, and empty-set shortcuts.
- Extension request, extension response, successful extended decode, and fallback after failure.
- Initial and extension sketch capacities exactly at and around every accepted limit.
- Canonical and non-canonical Boolean values, CompactSize encodings, truncation, and trailing data.
- Duplicate, unknown, zero, maximum, and over-capacity `ask_shortids` vectors.
- Transaction arrival, INV/TX reception, mempool removal, mining, and peer disconnect during every
  reconciliation phase.
- Several peers reconciling concurrently while the global request queue is advanced.

Fuzzamoto's default crash oracle detects crashes, failed assumptions, sanitizer reports, and hangs.
It does not by itself prove semantic properties such as correct transaction delivery, snapshot
privacy, parent-before-child ordering, or queue fairness. Those require explicit behavioral oracles
or comparison against a reference implementation in addition to this campaign.
