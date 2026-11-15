# Vocoders for P25 and DMR speech

P25 phase 1 speech is coded with IMBE, and DMR speech with AMBE+2. The node decodes those
protocols itself: calls, talkgroups, radio identities and encryption flags. Turning the voice
frames into sound needs a vocoder, and this project does not ship one. You build it yourself,
on your own machine, with one script.

## Before you build it

- AMBE and IMBE are patented. This project ships no implementation of them and builds none.
- [`tools/install-vocoders.sh`](../tools/install-vocoders.sh) downloads
  [mbelib-neo](https://github.com/arancormonk/mbelib-neo) from its authors, under their licence
  (GPL-2.0-or-later), and builds it on your machine at your request. The program it produces,
  `zeta-mbe`, is yours and is not to be redistributed with this project.
- Whether you may build or use it where you live is your responsibility.
- Encrypted calls are never decoded.
- Without the vocoder everything except the speech still works.

The script prints the same points and goes on only when you type `yes`.

## Building it

```bash
bash tools/install-vocoders.sh
```

It needs `git`, `cmake` and a C compiler (on Debian or Ubuntu: `sudo apt install git cmake
build-essential`), and names the package of any that is missing. Then it:

1. prints the notice above and waits for `yes`;
2. clones mbelib-neo at the one commit the script pins, and refuses any other;
3. builds the library as a static library with `cmake` (its build also fetches the FFT code it
   compiles in, PFFFT, from the same authors, checked against a pinned checksum), then compiles
   [`tools/vocoder/zeta-mbe.c`](../tools/vocoder/zeta-mbe.c) against it;
4. installs `zeta-mbe` under the prefix, `~/.local/bin/zeta-mbe` by default;
5. tests it: 50 frames of silence in for each codec, 160 samples out for each frame;
6. prints what it built, from which commit, and where it put it.

The build happens in a temporary directory, which the script removes when it ends.

| Option | Meaning |
|---|---|
| `--yes` | Accept the notice without the prompt. |
| `--src DIR` | Build from an mbelib-neo source tree you already have, and download nothing. The tree needs its bundled PFFFT archive (`third_party/pffft-*.tar.gz`), as mbelib-neo's release source archives have it. |
| `--prefix DIR` | Install `DIR/bin/zeta-mbe`. The default is `$HOME/.local`. |
| `--help` | Print the options and exit. |

## How the node finds it

The node looks for `zeta-mbe` when each call starts, so one installed while a job runs is used
from the next call. It takes the program from:

- `ZETA_HANDLER_ZETA_MBE`, when that names it: `ZETA_HANDLER_ZETA_MBE=/path/to/zeta-mbe`;
- otherwise the node's `PATH`.

`~/.local/bin` is on many shells' `PATH` but not on a service's. A node running as a systemd
service needs the variable in its unit (`Environment=ZETA_HANDLER_ZETA_MBE=...`) or the program
somewhere on the unit's `PATH`.

## What the program does

`zeta-mbe <codec>` reads frames on its standard input and writes speech on its standard output,
as `c2dec` does for M17:

| Codec | Protocol | Bytes in per frame | Out per frame |
|---|---|---|---|
| `imbe7200x4400` | P25 phase 1 | 184 (8 rows of 23) | 160 samples, s16le, 8 kHz |
| `ambe3600x2450` | DMR | 96 (4 rows of 24) | 160 samples, s16le, 8 kHz |

Each input byte is one bit of the channel-coded frame: 0 is a certain zero, 255 a certain one,
and the values between are the demodulator's confidence, which the library's soft-decision
decoding uses. Each frame's speech is written as soon as it is decoded. `zeta-mbe --version`
prints its own version and the library's; the end of the input ends it cleanly.
