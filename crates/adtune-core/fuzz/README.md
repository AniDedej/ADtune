# Fuzzing the ADtune parsers

Coverage-guided fuzzing (libFuzzer via [`cargo-fuzz`]) of the two untrusted-input
parsers in `adtune-core`. These are the trust boundaries: a hostile file must
never panic, allocate unbounded, or produce a non-finite / out-of-range value
that reaches the DSP or a rendered config.

| Target | Parser | Boundary it guards |
| --- | --- | --- |
| `parametric_eq` | `parse_eq_bands` → `biquad_coeffs` | `%ProgramData%\ADtune\config.txt`, read by the APO inside `audiodg.exe` (a `Users`-writable file → protected process) |
| `profile_json` | `profile_from_json` → render | imported `.adtuneprofile` files (may come from anywhere) |

Each target asserts the security invariants directly (finite + in-range preamp,
gain, frequency, Q; band count ≤ `MAX_BANDS`; string fields bounded; biquad
coefficients always finite; rendering never panics), so any violation is a
libFuzzer crash with a saved reproducer.

## Prerequisites

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Run

From `crates/adtune-core`. Seed the (git-ignored) corpus from the committed
seeds once, then fuzz — libFuzzer writes new coverage-increasing inputs into the
corpus dir it's given, so seed *into* `corpus/` and keep `seeds/` pristine:

```sh
mkdir -p fuzz/corpus/parametric_eq fuzz/corpus/profile_json
cp -n fuzz/seeds/parametric_eq/* fuzz/corpus/parametric_eq/
cp -n fuzz/seeds/profile_json/*  fuzz/corpus/profile_json/

cargo +nightly fuzz run parametric_eq -- -max_total_time=300
cargo +nightly fuzz run profile_json  -- -max_total_time=300
```

A found defect is written to `fuzz/artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<crash-file>`.

The generated corpus, artifacts, and build output are git-ignored; only the
seed inputs under `seeds/` are tracked. Do NOT pass `fuzz/seeds/...` directly to
`fuzz run` — that makes it the writable corpus and pollutes the tracked seeds.

Last run (2026-07-16): ~1.7M execs (`parametric_eq`) and ~4.9M execs
(`profile_json`), zero crashes.

[`cargo-fuzz`]: https://github.com/rust-fuzz/cargo-fuzz
