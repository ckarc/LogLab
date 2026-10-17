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

## ADIF tools

`loglab-adif` reads and writes ADI files (ADIF 3.1.7) losslessly and generates
ghost QSOs, synthetic test contacts that can never pass for real ones.

```sh
uv run loglab-adif ghosts -o ghosts.adi          # 1000 ghost QSOs (-n to change)
uv run loglab-adif check ghosts.adi              # validate and summarize
uv run loglab-adif roundtrip ghosts.adi -o copy.adi   # read, rewrite, prove nothing changed
```

Ghost files are test data. Never import them into a real log or upload them to
any service.
