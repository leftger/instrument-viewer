# Tests, coverage & releases

## Running the suite

```bash
cargo test --all-targets                      # unit + integration tests
cargo llvm-cov --all-targets --summary-only   # coverage summary
cargo llvm-cov --all-targets --lcov --output-path lcov.info  # what CI uploads
```

The suite needs no hardware and no display.

## The mock instrument

Drivers are mostly session-bound: they write commands and parse replies. Those
paths are driven for real against [`src/mock_scpi.rs`](../src/mock_scpi.rs), a
loopback TCP SCPI instrument that answers

- line replies (`*IDN?`, `:FUNC?`, `READ?`, …),
- `#N` definite-length blocks, including an ASCII trace, a `WORD` waveform and
  two's-complement bytes,
- the Hantek HRDO2000's non-standard 128-byte waveform header,

matches command needles with `*` wildcards (`:CHAN*:SCAL?`), records every
command it receives so `apply_*` writes can be asserted, and answers an
unmatched query with a marker naming the command — a missing rule fails fast
instead of hanging.

`src/e2e_tests.rs` uses it to run, for every backend, `read_config`,
`fetch_channels`, `acquisition_status`, `apply_channel`/`apply_horizontal`/
`apply_trigger`/`apply_acquisition`, `wait_sequence` and `autoset`; the GUI's
worker thread (`Worker::spawn` → `Connect`/`ReadConfig`/`RawQuery`/`Fetch`/
`PollStatus`/`Autoset`/`Disconnect`); and the real `cli::run` dispatch
(`get`, `scpi query|write`, `channel`, `horizontal`, `trigger`, `acquisition`,
`autoset`, `export` to CSV/JSON/wide, sequence capture).

Those tests have caught real regressions:

- a profile whose `*IDN?` pattern (`RIGOL`) was broad enough to shadow the
  DM3058 and DSA800 profiles, handing a multimeter to the scope driver;
- a multimeter that never translated its own `:FUNC?` reply (`VOLT`) into the
  internal function name, so it could not measure anything.

## What Codecov measures

CI runs the same tests under source-based instrumentation and uploads the lcov
report to [Codecov](https://codecov.io/gh/leftger/instrument-viewer) with the
project/patch settings in [`codecov.yml`](../codecov.yml): project tracks
`auto` within 1%, patch expects 60%. The upload authenticates with the
`CODECOV_TOKEN` repository secret (Settings → Secrets and variables → Actions);
without it Codecov still accepts tokenless public uploads, but they are rate
limited and reported less reliably. The job runs the test suite once — the
summary comes from `cargo llvm-cov report`, which reuses the profile data from
the `--lcov` run.

Excluded from the score, because no test can reach them:

| File | Why |
| :--- | :--- |
| `src/app.rs` | egui layout; only runs inside a live window, checked by the `--screenshot` smoke test |
| `src/main.rs` | argument dispatch entry point |
| `src/usbtmc.rs` | needs a real USBTMC device on the bus |

Everything else — the SCPI session, every driver, the profile registry, the
config model, discovery parsing and the exporters — is measured.

## Releases

Push a `v*` tag, or run the **Release** workflow manually with
`create_release` to draft a release from `main`:

```bash
git tag v0.1.1 && git push origin v0.1.1
```

`.github/workflows/release.yml` then builds a release binary on Linux, Windows
and macOS, bundles it with `assets/`, `profiles/` and both licenses, uploads the
archives as workflow artifacts, and attaches them to the GitHub release. A
manual dispatch creates a draft (tagged `v0.1.0-manual-<epoch>`) so the packages
can be checked before publishing.
