#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

# Self-deploy for a development box (loglab-dev). Run it every two minutes
# from the loglab user's crontab, or with --loop in a tmux session. It needs
# no sudo and touches nothing outside the checkout, its state folder and the
# tmux sessions "engine" and "agent".
#
# Each run, when origin/main has moved past the last commit it deployed:
#   - pulls main (fast-forward only; a checkout with local edits is left alone)
#   - Engine code, dependencies or migrations changed: uv sync --locked,
#     loglab-log migrate, restart the Engine, check it answers
#   - agent/ changed: build the Agent, restart it
#   - web files only (engine/src/loglab_engine/web): the pull is enough
# A failed sync, migration or build restarts nothing: the checkout goes back
# to the last good commit and that commit of main is not tried again.
# The migration itself is never undone (QSO history is append-only,
# Principle VII), so a bad migration needs a person.
#
# Between deploys, if the Engine is down and its tmux session is gone, it is
# started again.
#
# Settings (optional) in ~/.config/loglab/autodeploy.env:
#   LOGLAB_REPO   checkout (default ~/LogLab)
#   ENGINE_CMD    command run in tmux "engine" from engine/
#   ENGINE_URL    where the Engine answers (default http://127.0.0.1:8073)
#   AGENT_BUILD   command run in agent/ to build the Agent
#   AGENT_CMD     command run in tmux "agent" from agent/; empty = no Agent
#
# State in ~/.local/state/loglab/: deployed (last good commit), failed
# (a commit that failed and is skipped), autodeploy.log.

main() {
    set -u -o pipefail
    export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

    local config="$HOME/.config/loglab/autodeploy.env"
    LOGLAB_REPO="$HOME/LogLab"
    ENGINE_CMD="uv run loglab-engine serve --host 127.0.0.1 --port 8073 >> $HOME/loglab-engine.log 2>&1"
    ENGINE_URL="http://127.0.0.1:8073"
    AGENT_BUILD="cargo build --release --locked"
    AGENT_CMD=""
    # shellcheck source=/dev/null
    [[ -f "$config" ]] && . "$config"

    STATE="$HOME/.local/state/loglab"
    LOG="$STATE/autodeploy.log"
    mkdir -p "$STATE"
    trim_log

    if [[ "${1:-}" == "--loop" ]]; then
        while true; do
            bash "$0" || true
            sleep 120
        done
    fi

    exec 9>"$STATE/autodeploy.lock"
    flock -n 9 || exit 0

    cd "$LOGLAB_REPO" || { say "no checkout at $LOGLAB_REPO"; exit 1; }

    if ! git fetch -q origin main 2>>"$LOG"; then
        # No network is normal in FieldLab; try again next run.
        keep_engine_alive
        exit 0
    fi

    local target head good
    target=$(git rev-parse origin/main)
    head=$(git rev-parse HEAD)
    good=$(cat "$STATE/deployed" 2>/dev/null || true)

    if [[ "$head" == "$target" && "$good" == "$target" ]]; then
        keep_engine_alive
        exit 0
    fi
    if [[ "$(cat "$STATE/failed" 2>/dev/null || true)" == "$target" ]]; then
        keep_engine_alive
        exit 0
    fi
    if [[ "$(git symbolic-ref -q --short HEAD)" != "main" ]]; then
        say "checkout is not on main; not deploying"
        fail "$target"
        exit 1
    fi
    if ! git diff --quiet HEAD --; then
        say "checkout has local edits; not deploying"
        fail "$target"
        exit 1
    fi

    say "deploying ${target:0:9} (was ${good:0:9}${good:+ }${good:-nothing recorded})"
    if ! git merge -q --ff-only origin/main >>"$LOG" 2>&1; then
        say "main cannot fast-forward; not deploying"
        fail "$target"
        exit 1
    fi

    # What changed since the last good deploy. With none recorded, treat
    # everything as changed.
    local changed engine=0 agent=0
    if [[ -n "$good" ]] && git cat-file -e "$good^{commit}" 2>/dev/null; then
        changed=$(git diff --name-only "$good" "$target")
    else
        changed=$'engine/src/\nagent/'
    fi
    grep -E '^engine/(src/|pyproject\.toml|uv\.lock)' <<<"$changed" \
        | grep -qvE '^engine/src/loglab_engine/web/' && engine=1
    grep -qE '^agent/' <<<"$changed" && agent=1

    if (( engine )); then
        if ! (cd engine && uv sync --locked) >>"$LOG" 2>&1; then
            say "uv sync failed; going back to ${good:0:9}"
            roll_back "$good" "$target"
            exit 1
        fi
        if ! (cd engine && uv run loglab-log migrate) >>"$LOG" 2>&1; then
            say "migrate failed; going back to ${good:0:9} (the Engine was not restarted)"
            roll_back "$good" "$target"
            exit 1
        fi
    fi
    if (( agent )) && [[ -n "$AGENT_CMD" ]]; then
        if ! (cd agent && eval "$AGENT_BUILD") >>"$LOG" 2>&1; then
            say "Agent build failed; going back to ${good:0:9} (nothing restarted)"
            roll_back "$good" "$target"
            exit 1
        fi
    fi

    if (( engine )); then
        restart engine "$LOGLAB_REPO/engine" "$ENGINE_CMD"
        if ! engine_answers 120; then
            say "the Engine did not come back at ${target:0:9}; going back to ${good:0:9}"
            roll_back "$good" "$target"
            restart engine "$LOGLAB_REPO/engine" "$ENGINE_CMD"
            engine_answers 120 || say "the Engine is down; it needs a person"
            exit 1
        fi
    fi
    if (( agent )) && [[ -n "$AGENT_CMD" ]]; then
        restart agent "$LOGLAB_REPO/agent" "$AGENT_CMD"
    fi

    echo "$target" >"$STATE/deployed"
    rm -f "$STATE/failed"
    local did="pulled"
    (( engine )) && did="$did, Engine restarted"
    (( agent )) && [[ -n "$AGENT_CMD" ]] && did="$did, Agent rebuilt and restarted"
    say "deployed ${target:0:9}: $did"
}

say() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG"
}

trim_log() {
    if [[ -f "$LOG" ]] && (( $(stat -c %s "$LOG") > 1000000 )); then
        tail -n 2000 "$LOG" >"$LOG.tmp" && mv "$LOG.tmp" "$LOG"
    fi
}

fail() {
    echo "$1" >"$STATE/failed"
}

# Put the checkout back on the last good commit and skip this one.
roll_back() {
    local good=$1 target=$2
    fail "$target"
    if [[ -n "$good" ]]; then
        git reset -q --hard "$good" >>"$LOG" 2>&1
        (cd engine && uv sync --locked) >>"$LOG" 2>&1
    fi
}

engine_answers() {
    local wait=$1 i
    for ((i = 0; i < wait; i += 3)); do
        if curl -fs -o /dev/null --max-time 3 "$ENGINE_URL/" \
            && curl -fs -o /dev/null --max-time 3 "$ENGINE_URL/static/app.js"; then
            return 0
        fi
        sleep 3
    done
    return 1
}

# Stop what runs in a tmux session (Ctrl+C, as a person would) and start the
# command again in a fresh session, so a person can still attach to it.
restart() {
    local name=$1 dir=$2 cmd=$3 i
    if tmux has-session -t "=$name" 2>/dev/null; then
        tmux send-keys -t "=$name:" C-c
        for ((i = 0; i < 20; i++)); do
            [[ "$(tmux display -p -t "=$name:" '#{pane_current_command}')" =~ ^-?(ba|z|da)?sh$ ]] && break
            sleep 1
        done
        tmux kill-session -t "=$name"
        sleep 2
    fi
    # Close the lock so a tmux server started here does not hold it forever.
    tmux new-session -d -s "$name" -c "$dir" 9>&-
    tmux send-keys -t "=$name:" "$cmd" Enter
    say "started $name: $cmd"
}

keep_engine_alive() {
    if ! tmux has-session -t "=engine" 2>/dev/null && ! engine_answers 3; then
        say "the Engine was not running; starting it"
        restart engine "$LOGLAB_REPO/engine" "$ENGINE_CMD"
        engine_answers 120 || say "the Engine did not come up; it needs a person"
    fi
}

# The whole script is read before anything runs, so a pull that changes this
# file cannot change it under a running deploy.
main "$@"
exit
