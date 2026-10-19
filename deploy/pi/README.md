# ZetaSDR on a Raspberry Pi 5

A Pi at the antenna runs `zeta serve`; the laptop attaches it with `--source-node`.

## Build (dev box)

Needs docker and rustup. `build-pi.sh` installs `cross` if absent, builds `zeta` and
`zeta-mcp` for `aarch64-unknown-linux-gnu` (release) and copies them to `deploy/pi/out/`
(gitignored), printing sizes and sha256.

    deploy/pi/build-pi.sh
    deploy/pi/smoke.sh        # offline: runs the binary under qemu-aarch64-static

The binary is linked against the cross image's glibc (2.31), so it runs on Raspberry Pi OS
bookworm and newer. `smoke.sh` checks `zeta schema` is JSON with an `op` key and that
`--help` lists `--source-node`; for a dynamic aarch64 binary under qemu it needs a sysroot
(`QEMU_LD_PREFIX`, `/usr/aarch64-linux-gnu`, or it copies one from the cross image into
`out/sysroot`).

Soapy is not built: nothing is attached to the Pi yet. To drive an SDR on the Pi later,
build on the Pi itself: `sudo apt install libsoapysdr-dev` plus the device's SoapySDR module,
then `RUSTFLAGS="--cfg soapy" cargo build --release -p zeta` (see the main README).

## Install (Pi)

    scp deploy/pi/out/zeta deploy/pi/out/zeta-mcp deploy/pi/install.sh deploy/pi/zetasdr-check-env \
        deploy/pi/zetasdr.service deploy/pi/zetasdr.env.example pi@pi.local:
    ssh pi@pi.local
    sudo ./install.sh
    sudo editor /etc/zetasdr/env        # set ZETA_SECRET (openssl rand -hex 16) and ZETA_SOURCES, e.g. --source-kiwi kiwi=HOST:8073
    sudo systemctl start zetasdr       # refuses to start while the secret is empty or the placeholder
    systemctl status zetasdr

`install.sh` puts the binaries in `/usr/local/bin`, creates the `zetasdr` user with state in
`/var/lib/zetasdr`, writes `/etc/zetasdr/env` (mode 0600) only if absent, and enables the unit.

`serve` binds LAN addresses only (loopback, private, link-local, CGNAT, unspecified). The unit
listens on `0.0.0.0:7979`, which is accepted because the unspecified address counts as LAN.
Do not forward the port to the internet; the secret is the only authentication.

## Use (laptop)

    ZETA_NODE_SECRET_PI=<the secret> zeta --source-node pi=kiwi@http://pi.local:7979 source list

`pi` is the local id, `kiwi` the source id on the Pi (here a `--source-kiwi kiwi=...` in
`ZETA_SOURCES`), and `ZETA_NODE_SECRET_<ID>` carries the secret.

## WiFi budget

A remote source ships at most 250 kHz of IQ by default. As ci16 that is 250 000 x 4 bytes
= 1 MB/s, about 8 Mbit/s, which a healthy WiFi link carries. Wider tunings need
`max_bandwidth_hz=N` on the flag and a wired link; a slow link drops blocks.
