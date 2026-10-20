# perseids (Rust, experimental)

**Experimental.** `bin/perseids` (bash) is the supported CLI. This port has `run`, `exec`, `key`, `ls`, `reap` and
`image build`, but not `bisect` or `--split` yet, and its Linux builds are untested.

The Rust port of `bin/perseids`, built as a single binary with no Ruby, jq or curl needed on your machine. It plans the workflow
itself (`src/plan.rs`, kept identical to `lib/runner.rb plan` by `tests/parity.rs`). It embeds `lib/runner.rb`,
which still runs each job on the box under Ruby. It shells out to the system `ssh`, `scp` and `git`.

```bash
cargo test             # unit tests + parity with `ruby lib/runner.rb plan` (needs ruby for the parity test only)
cargo build --release  # target/release/perseids, about 2.3 MB on macOS arm64
```

The macOS binary links only system libraries (libSystem, Foundation).

## Linux builds (x86_64 and arm64)

Use musl targets to get a fully static binary. TLS is rustls, so there's no OpenSSL to link. `ring` needs a C cross-compiler,
and zig provides one:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo install cargo-zigbuild   # also needs zig on PATH (ziglang.org or `pip install ziglang`)
cargo zigbuild --release --target x86_64-unknown-linux-musl
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

On a Linux host, `cross build --release --target <triple>` (Docker) works too. These Linux builds have not been run yet.

## Differences from the bash CLI

- `perseids plan <workflow> [job...]` is hidden. It prints the same TSV as `ruby runner.rb plan`.
- YAML is read as YAML 1.2 (serde_yaml), the way GitHub reads it. Ruby's YAML 1.1 reads unquoted `yes`, `no`, `on` and `off`
  as booleans. serde_yaml also accepts a few flow-style plain scalars with `:` that Psych rejects (`{ container: node:22 }`).
- When the SSH link drops, the summary keeps "error the SSH link dropped (rerun)". The bash worker's EXIT trap overwrites
  that message.
- `reap` treats a process it isn't allowed to signal as alive, so it never deletes another user's live boxes on a shared host.
