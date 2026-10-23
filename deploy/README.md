<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Automatic deploys to a development box

`autodeploy.sh` keeps a development box (loglab-dev) running the newest
`main` without anyone asking for each deploy. It runs as the box's own user,
needs no sudo, and is quiet unless something happens.

Every two minutes it checks GitHub. When `main` has moved, it pulls and then
does only what the change needs:

| What changed                                   | What it does                                     |
| ---------------------------------------------- | ------------------------------------------------ |
| Web files only (`engine/src/loglab_engine/web`) | Nothing more; a reload shows them                |
| Engine code, dependencies or migrations        | `uv sync --locked`, `loglab-log migrate`, restarts the Engine, checks it answers |
| `agent/`                                       | Builds the Agent, restarts it                    |
| Anything else (docs, tests, CI)                | Nothing more                                     |

If a sync, migration or build fails, nothing is restarted: the checkout goes
back to the last commit that deployed, and that commit of `main` is not tried
again (the next merge is). A migration that has run is never undone; a failed
one needs a person. A checkout with local edits, or not on `main`, is left
alone. If the Engine is not running at all, it is started again.

The Engine runs in tmux session `engine` and the Agent in `agent`, as before,
so `tmux attach -t engine` still shows it.

## Install (once, as the box's user)

```sh
mkdir -p ~/.config/loglab
cat > ~/.config/loglab/autodeploy.env <<'EOF'
# The commands that start the Engine (run from engine/) and the Agent (run
# from agent/). Leave AGENT_CMD empty if the box runs no Agent.
ENGINE_CMD="uv run loglab-engine serve --host 127.0.0.1 --port 8073 >> $HOME/loglab-engine.log 2>&1"
AGENT_BUILD="cargo build --release --locked"
AGENT_CMD=""
EOF
( crontab -l 2>/dev/null; echo '*/2 * * * * $HOME/LogLab/deploy/autodeploy.sh' ) | crontab -
```

Without cron, run it in its own tmux session instead:
`tmux new -d -s autodeploy '~/LogLab/deploy/autodeploy.sh --loop'`.

## See what it did

```sh
tail ~/.local/state/loglab/autodeploy.log     # one line per step
cat ~/.local/state/loglab/deployed            # the commit now live
```

To stop it, remove the line with `crontab -e`. To retry a commit it skipped,
`rm ~/.local/state/loglab/failed`.
