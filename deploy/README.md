# Deploying a node

`deploy/pi` is the supported example: a systemd unit, an environment file, a build
script and a smoke test for a Raspberry Pi at the antenna. See
[deploy/pi/README.md](pi/README.md).

A host's own scripts (where its node runs, its secrets, how it is updated) are the
operator's and are not published. Copy `deploy/pi` as a starting point.

To register an MCP server with Claude Code, copy `.mcp.json.example` to `.mcp.json` in
the repository root and change the URL.
