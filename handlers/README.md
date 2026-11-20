# Handler files

A handler file adds an external program to each decoder mode it lists, beside the mode
row's own decoder. The node embeds the `*.json` files here and reads the operator's from
`--handlers-dir` (default `$XDG_CONFIG_HOME/zetasdr/handlers`, else
`~/.config/zetasdr/handlers`); an operator file replaces the embedded one of its `id`.

- `id`: the handler's id, `^[a-z0-9][a-z0-9_-]*$`; never `builtin`, nor a mode's own program.
- `name`: its name for people; `programs`, `install`: as a program row's
  [`decoder`](../modes/adsb_decode.json) has them.
- `modes`: by mode id, a program block as a program row's (`args`, `input`, `output`,
  optional `packets` and `demod`), whose `output.fields` maps the row's subject key, and
  `extra`: fields the handler adds that the row's event schema lacks.

A broken file, or an entry naming no mode or failing validation, is reported at startup
and never stops the node.
