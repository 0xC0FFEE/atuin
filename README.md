# Atuin (History-Only Fork)

Atuin replaces your shell history file with a local SQLite database and provides fast interactive search in your terminal.

This fork is intentionally history-only:
- no sync
- no server
- no daemon
- no AI
- no KV/scripts/dotfiles stores

But:
- blazingly fast history search
- fzf-like interactive search via nucleo

## Features

- Capture command history with context (cwd, exit code, duration, host/session)
- Interactive and non-interactive history search
- History import from supported shells
- History stats
- Shell integrations for zsh, bash, fish, nushell, xonsh, and powershell

## Install

Use your package manager or build from source:

```sh
cargo install --path crates/atuin
```

## Quickstart

Add shell init and restart your shell:

```sh
eval "$(atuin init zsh)"
```

Import existing shell history:

```sh
atuin import auto
```

## Common Commands

```sh
atuin search -i
atuin history list
atuin history last
atuin stats
```
