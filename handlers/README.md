# Handler files

A handler file adds an external program to each decoder mode it lists, beside the mode
row's own decoder. The node embeds the `*.json` files here and reads the operator's from
`--handlers-dir` (default `$XDG_CONFIG_HOME/zetasdr/handlers`, else
`~/.config/zetasdr/handlers`); an operator file replaces the embedded one of its `id`.

- `id`: the handler's id, `^[a-z0-9][a-z0-9_-]*$`; never `builtin` or `none` (reserved),
  nor a mode's own program.
- `name`: its name for people; `programs`, `install`: as a program row's
  [`decoder`](../modes/adsb_decode.json) has them.
- `modes`: by mode id, a program block as a program row's (`args`, `input`, `output`,
  optional `packets` and `demod`), whose `output.fields` maps the row's subject key, and
  `extra`: fields the handler adds that the row's event schema lacks, and
  `const`: row event fields given a constant value (a string, number, boolean or null the
  schema's `type` accepts) on every event, after the field map (`{"crc_ok": true}`); not a
  key the event envelope sets (`protocol`, `handler`, `summary`, `t_ns`, `position`,
  `access`), the subject key, nor a field the program also gives.

An entry's `args` take the placeholders of a program row's (`$port`, `$packet_port`,
`$rate_hz`, `$center_hz`, `$center_khz`, `$fifo`) and `$bandwidth_hz`: the job's band width in
Hz, a whole number (a retune to a new width relaunches the program with the new one).

A row may have no decoder of its own (`"decoder": null`): only a handler file's entry runs
it. Such a row is listed `ready` while one of its entries is ready and `unavailable`
otherwise, with the reason `no decoder of its own, and no handler for it is ready`, then
`; <id>: <reason>` for each entry (the missing program, say); its `decoder_kind` is `null`
and its `handlers` are its entries only. A job of it always records its handler, the
policy's pick when the start names none. Its entry's `output.fields`, when it maps the
program's line, names every field of the row's event schema but the envelope's
(`protocol`, `summary`, `t_ns`, `position`) that no `const` or row `derive` fills, since
nothing else does (`output.fields: <name> is not mapped, and the row has no decoder of its
own to fill it`). The row takes no `identify.quick`, and its `test` is a fixture, which
`zeta modes check <id>` runs through the handler the policy picks (`<id>@<handler>: ok`;
with none ready, `<id>: skipped: no decoder of its own and no handler ready`).

A handler job outputs events, and packets when its entry has `packets`; never audio or
pictures. Where the row says on each event whether speech is produced, the entry gives
that as a `const` (OnAir's DMR and P25: `{"audio": false}`); a transcriber attached to a
handler job is refused (`job <id> has no audio stream`).

The embedded `onair.json` runs OnAir's receivers through `onair-rx` 0.4.0, a program the
operator installs (an older one is probed `ready`, and a mode it lacks then fails at launch with
`onair-rx: no mode <m>`: install it before the node). An entry has no notes field, so what OnAir gives for each mode, how it
differs from the row's own decoder and what it was measured to cost are in
[RECIPES.md](../RECIPES.md#a-second-opinion-on-a-decoder); the eleven rows only OnAir decodes, with
no row decoder behind them, are in
[RECIPES.md](../RECIPES.md#modes-only-onair-decodes).

The operator's node-wide handler ranking does not prefer a handler whose `outputs` lack a
kind the mode's own decoder gives (events but no audio, on a voice mode): that handler is
ordered as if the ranking did not name it, so one entry cannot take speech away from every
voice job. Rank it for the mode, or name it when starting a job, to run it; nothing is
excluded, and it runs when it is the best one ready.

A broken file, or an entry naming no mode or failing validation, is reported at startup
and never stops the node.
