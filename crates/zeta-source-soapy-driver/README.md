# zeta-source-soapy-driver

The real SoapySDR adapter for `zeta-source-soapy`: `impl SoapyDevice for
Driver` over `soapysdr::Device` (receive channel 0), `open(id, args)` and
`find(filter)` (the `SoapySDRUtil --find` equivalent).

It links libSoapySDR, so it is **excluded from the workspace** (root
`Cargo.toml`, `[workspace] exclude`); `cargo --workspace --all-features` never
builds it. The port logic and its tests live in `zeta-source-soapy`.

Device gains are left at the driver's defaults; a `source.set_gain` operation
is future work.

## Build

```sh
sudo apt install libsoapysdr-dev soapysdr-module-hackrf   # headers, SoapySDR.pc, HackRF module
cd crates/zeta-source-soapy-driver
cargo clippy --all-targets -- -D warnings
cargo test                         # compiles and links; no tests run without --ignored
cargo test -- --ignored            # HackRF: 1 s at 100 MHz, 2 MS/s
```

With no HackRF attached the ignored test prints `skipped: no hackrf attached`.

Without the package, `DOCS_RS=1 cargo check --all-targets` type-checks the
crate (soapysdr-sys ships pre-generated bindings and skips its library probe
under `DOCS_RS`), but nothing links.

## In the `zeta` CLI

`crates/zeta` depends on this crate under `[target.'cfg(soapy)'.dependencies]`, so it is built
only when the `soapy` cfg is set (never by a cargo feature, so `--all-features` stays free of
libSoapySDR):

```sh
RUSTFLAGS="--cfg soapy" cargo build -p zeta --release   # from the workspace root
zeta soapy find                                         # attached devices as JSON
zeta --source-soapy hackrf0=driver=hackrf source list
```
