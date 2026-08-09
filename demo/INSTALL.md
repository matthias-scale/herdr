# Wiring the contract into a session

Verified live end-to-end on 2026-08-06 against real `claude` and `codex` turns.

## Claude Code

Hooks load from **user** settings (`~/.claude/settings.json`), **project**
settings (`.claude/settings.json`), or **project-local** settings
(`.claude/settings.local.json`, gitignored). They do **not** load from
`--settings <file>` — that flag ignores `hooks` entirely, which costs an hour if
you assume otherwise.

Project and project-local hooks additionally require the folder to be trusted.
Until `~/.claude.json` has `projects["<abs cwd>"].hasTrustDialogAccepted: true`,
they are silently skipped — no warning, no debug line.

```json
{"hooks":{"Stop":[{"hooks":[
  {"type":"command","command":"python3 /abs/path/demo/herdr-closing-block.py"}
]}]}}
```

Install it *beside* herdr's managed `herdr-agent-state.sh`, never inside it —
herdr overwrites that file on integration update.

`Stop` does **not** fire in `claude -p` headless mode, so verify interactively.

## Codex

`~/.codex/config.toml` already carries a `notify` entry on this machine, so
covering codex means **chaining**, not replacing. For a throwaway session, use
the CLI override instead of editing the file at all:

```sh
codex -c 'notify=["/abs/path/to/wrapper.sh"]'
```

where the wrapper forwards `$1` to `demo/herdr-codex-notify.py`. Real payload:

```json
{"type":"agent-turn-complete","thread-id":"...","turn-id":"...",
 "cwd":"...","client":"codex-tui","input-messages":["..."],
 "last-assistant-message":"ok\n\n**Critical action points (2 blocking)**\n..."}
```

`last-assistant-message` carries the closing block directly, so codex needs no
transcript parsing — one reason the codex path is simpler than claude's.
