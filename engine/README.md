# LogLab Engine

The Engine is LogLab's server: it owns the log, contest templates, reference
data, operator identities, and the event stream. It is strictly typed Python
on PostgreSQL. See [`../CONSTITUTION.md`](../CONSTITUTION.md).

## Run the gates locally

Requires [uv](https://docs.astral.sh/uv/).

```sh
cd engine
uv sync --locked
uv run ruff format --check
uv run ruff check
uv run mypy
uv run pytest
```
