# The KiwiSDR waterfall stream, from the server's source

What a KiwiSDR's `W/F` (waterfall) websocket does, as the server's source has it. Written for the
Kiwi source's self-sweep (plan 36.2); read 2026-11-11, no KiwiSDR was connected.

Server: `jks-prv/Beagle_SDR_GPS` at `efb38e2b` (v1.711; paths below are relative to it, `rx/` is its
receiver code). Reference client: `jks-prv/kiwiclient` at `4eb733e6`. The IQ codec in this repo was
checked against a later server (v1.837); the facts below are unverified on that version.

## Socket and slot rules

- The path is `/<timestamp>/W/F` (`rx/rx_server.cpp:303-310`); a path naming `W/F` marks a waterfall
  connection (`:342`).
- A new `SND` or `W/F` whose timestamp equals a live connection's of the other type, from the same
  address (or with bit 62 of the timestamp set), is linked to it and shares its receiver channel:
  one user (`:466-496`, `rx/rx_util.h:37`). Two of one type with one timestamp: the second is dropped
  silently (`:466-469`). The reference waterfall recorder gives its `W/F` its own timestamp
  (`kiwiclient/kiwiwfrecorder.py:242-247`).
- **The W/F socket always stands alone for us.** Unpaired, it is given a channel of its own, a user
  slot, searched from channel 0 (`:561-566`; `rx/rx_util.cpp:198-227`).
- An `SND` from a client that is not the Kiwi's page takes channels without a waterfall first
  (`rx/rx_util.cpp:223-227`). A `W/F` paired to an `SND` on such a channel is closed with no message
  (`:672-680`).
- **A pair ends together.** A `W/F` that closes, is kicked or misses its keepalive (60 s,
  `kiwi.h:130`) stops its paired `SND` (`rx/rx_waterfall.cpp:706-723`; a closed socket sets `kick`,
  `rx/rx_server.cpp:268-276`). An `SND` stops its paired `W/F` (`rx/rx_sound.cpp:380-400`). So closing a
  paired `W/F` ends the IQ session.
- With the admin's `no_dup_ip` set, a second unpaired `SND`/`W/F` from one address gets `MSG badp=5`
  and is kicked; a paired one is exempt because it shares the channel (`rx/rx_cmd.cpp:755-792`).

## Refusals

- No channel free, or the free one has no waterfall (`rx_n >= wf_chans`): `MSG too_busy=<rx_chans>`,
  then the socket is dropped (`rx/rx_server.cpp:632`, `:645-646`).
- All connection slots taken: a closed `W/F` with no message at all (`:524-526`, the message is skipped
  for the waterfall type). A refusal can therefore also be a bare close.
- A locked Kiwi: `MSG exclusive_use` (`:570-582`). The status page names no waterfall channels
  (`rx/rx_server_ajax.cpp:734-757`), so they cannot be counted beforehand.

## Handshake and commands

- After `SET auth t=kiwi p=...` the server sends `MSG center_freq= bandwidth= adc_clk_nom=`
  (`rx/rx_waterfall.cpp:229`), then `MSG wf_fft_size=1024 wf_fps=23 wf_fps_max=23 zoom_max=14
  rx_chans= wf_chans= wf_chans_real= wf_cal= wf_setup` (`:235-236`). `wf_chans` is the configured count
  on a `W/F` path (0 on any other, `:233-236`). `bandwidth` is 30 or 32 MHz (`rx/rx_init.cpp:152`).
- Frames start once `zoom`/`start`, `maxdb`/`mindb` and `wf_speed` have all arrived (`:218`, `:750`):
  `SET zoom=<z> cf=<kHz>` (`:375-381`), `SET maxdb=<n> mindb=<n>` (`:542-548`), `SET wf_speed=4` (23 fps,
  `:628-637`), then `SET wf_comp=0` (`rx/rx_cmd.cpp:1964`) and `SET interp=10` (`:576-585`: strongest
  FFT bin per pixel with CIC compensation). Commands are drained between frames (`:664-674`).
- A start is counted in `bandwidth / (1024 << 14)` Hz and clamped to the band (`:70`, `:259`, `:484-490`).
  Zoom is clamped 0..14 (`:367`; `MAX_ZOOM` is 14, `kiwi.config:275`).

## The frame

- 1040 bytes: `"W/F "`, `x_bin_server` u32, `flags_x_zoom_server` u32, `seq` u32 (a 16-byte
  header, little endian), then 1024 bytes (`rx/rx_waterfall.h:72-86`, `rx/rx_waterfall.cpp:281`;
  the client reads the same fields, `kiwiclient/kiwi/client.py:733`).
- Flag bit `0x00010000` marks ADPCM, with a 10-byte pad (`rx/rx_waterfall.h:77`, `.cpp:1662-1667`).
- `dB = byte - 255`, clamped -200..0 (`.cpp:1577-1583`); the client adds `wf_cal`
  (`kiwiclient/kiwirecorder.py:666`). The first 2 pixels are zeroed, 4 at zoom 0 (`.cpp:1341`).

## Settling after a zoom or centre change

- `x_bin_server` and the low 16 bits of `flags_x_zoom_server` are the start and zoom the frame was
  computed with (`.cpp:1650-1653`); the branch that would report the previous view is never armed
  (`flush_wf_pipe` is only set in commented code, `:520-522`, `:1126`).
- Up to zoom 10 every frame refills the sample buffer from empty (`:987-1010`, `:1027-1030`): at 23 fps
  the wait is 63 ms against a 86 ms limit, and zoom 11 exceeds it and samples overlapped. So the first
  frame whose header names the new view was sampled after the change; drop frames until it does.
  The down-converter's flush is a commented-out "uncertain" note (`:1124`), hence one extra frame.

## What was built (plan 36.2)

- `zeta-source-kiwi` opens a `W/F` socket of its own for a scan (never paired with the IQ session, so
  the IQ session is untouched and the sweep costs one more user slot while it runs), with its own
  timestamp, and steps zoom and centre over the span (`wf_plan`; at most zoom 10, `WF_ZOOM_MAX`, so a
  frame refills within its limit). The zoom and the step count follow `bin_hz`; a span that fits one view
  is one step at a finer zoom.
- It keeps only the frames whose header names the asked view and drops `WF_SETTLE_FRAMES` after a
  change, then paints each view's rows as scan rows (frame `channel` `scan/<step>`) with the first
  pixels masked and `wf_cal` added, so the levels are calibrated dBm (`calibrated` on the row).
- A refusal (`too_busy`, `exclusive_use`, a bare close, `badp`, a handshake over `WF_HANDSHAKE_MAX_MS`)
  is read as the sweep's reason; the node's scan falls back to stepped or, for a restarted `source`
  scan, waits and asks again. The IQ session ending does not end the sweep.
- Still unverified on a live Kiwi: the pairing, `no_dup_ip` and the settle (`UAT.md` "Plan 36.2").

## Not verified

- That a later server (v1.837 or a KiwiSDR 2) behaves as above.
- The frame rate over a real link; the number of extra frames to drop after a change.
- That `MSG wf_chans=0` on a `W/F` path always means a Kiwi configured without waterfalls.
- The 63 ms figure assumes a 66.66 MHz ADC clock (the server's own variable, `.cpp:451`).
