#!/usr/bin/env python3
"""
Regenerate migrations/0002_seed_domains.sql from config/domains.yaml.

Run after editing the YAML; commit both files together so the seed
migration and its source of truth stay in lockstep.

Validates every row before emitting SQL — refuses to write a broken
migration if `slug` / `kind` / `name` / `description` don't match the
expected shape. Every SQL literal (slug, kind, JSON for the i18n
columns) is written as a single-quoted string with embedded
apostrophes doubled, so arbitrary translation content stays safe
regardless of punctuation.

Requires PyYAML (`pip install pyyaml` or use a venv).
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError:
    sys.stderr.write(
        "PyYAML not installed. Run via a venv:\n"
        "  python3 -m venv /tmp/yaml-venv && /tmp/yaml-venv/bin/pip install pyyaml\n"
        "  /tmp/yaml-venv/bin/python scripts/regen-domain-seed.py\n"
    )
    sys.exit(1)


ALLOWED_KINDS = {"topical", "meta", "horizontal"}
# Slug must be kebab-case: 2+ chars, starts with a letter, ends with an
# alphanumeric, only lowercase letters / digits / hyphens in between.
# The 2-char minimum is intentional — every realistic domain slug is at
# least a word — and lets the regex enforce the trailing-char rule
# without an alternation for the single-char case.
SLUG_RE = re.compile(r"^[a-z][a-z0-9-]*[a-z0-9]$")


def sql_quote(s: str) -> str:
    """Escape a single-quoted SQL literal. Doubles embedded apostrophes."""
    return "'" + s.replace("'", "''") + "'"


def validate(domain: object, idx: int) -> dict:
    """Return the domain dict if every field passes; raise SystemExit otherwise."""
    if not isinstance(domain, dict):
        raise SystemExit(
            f"domains[{idx}]: expected a mapping, got {type(domain).__name__}"
        )
    slug = domain.get("slug")
    if not isinstance(slug, str) or not SLUG_RE.match(slug):
        raise SystemExit(
            f"domains[{idx}]: slug must be kebab-case (2+ chars, "
            f"[a-z][a-z0-9-]*[a-z0-9]), got {slug!r}"
        )
    kind = domain.get("kind")
    if kind not in ALLOWED_KINDS:
        raise SystemExit(
            f"domains[{idx} {slug}]: kind must be one of {sorted(ALLOWED_KINDS)}, "
            f"got {kind!r}"
        )
    so = domain.get("sort_order")
    if not isinstance(so, int) or isinstance(so, bool):
        raise SystemExit(
            f"domains[{idx} {slug}]: sort_order must be int, got {so!r}"
        )
    name = domain.get("name")
    if not isinstance(name, dict) or "zh-TW" not in name:
        raise SystemExit(
            f"domains[{idx} {slug}]: name must be a mapping with a zh-TW key"
        )
    if not isinstance(name["zh-TW"], str) or not name["zh-TW"]:
        raise SystemExit(
            f"domains[{idx} {slug}]: name.zh-TW must be a non-empty string"
        )
    description = domain.get("description")
    if description is not None:
        if not isinstance(description, dict):
            raise SystemExit(
                f"domains[{idx} {slug}]: description must be null or a mapping, "
                f"got {type(description).__name__}"
            )
        zh = description.get("zh-TW")
        if not isinstance(zh, str) or not zh:
            raise SystemExit(
                f"domains[{idx} {slug}]: description.zh-TW must be a non-empty string"
            )
    return domain


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    src = repo / "config" / "domains.yaml"
    out = repo / "migrations" / "0002_seed_domains.sql"

    raw = yaml.safe_load(src.read_text(encoding="utf-8"))
    if not isinstance(raw, list) or not raw:
        sys.stderr.write(f"{src} produced no domains\n")
        return 1

    domains = [validate(d, i) for i, d in enumerate(raw)]

    seen: set[str] = set()
    for d in domains:
        if d["slug"] in seen:
            raise SystemExit(f"duplicate slug: {d['slug']!r}")
        seen.add(d["slug"])

    domains.sort(key=lambda d: (d["sort_order"], d["slug"]))

    lines: list[str] = [
        "-- M0 #0.8 — seed the 20 marketplace domains.",
        "--",
        "-- GENERATED from config/domains.yaml — do not edit by hand.",
        "-- Re-run scripts/regen-domain-seed.py after editing the YAML.",
        "--",
        "-- Inserts are idempotent (ON CONFLICT … DO UPDATE) so re-running",
        "-- this migration won't duplicate rows. If you actually need to",
        "-- delete a domain, write a follow-up migration that does so explicitly.",
        "",
        "INSERT INTO domains (slug, kind, sort_order, name_i18n, description_i18n) VALUES",
    ]

    rows: list[str] = []
    for d in domains:
        slug_lit = sql_quote(d["slug"])
        kind_lit = sql_quote(d["kind"])
        so = d["sort_order"]
        # Use a single-quoted SQL literal with apostrophe doubling instead of
        # dollar quoting — a translation containing the substring '$json$'
        # would otherwise terminate the literal early. JSON strings never
        # contain bare apostrophes, only escaped Unicode + quoted strings,
        # so the sql_quote() pass is straightforward.
        name_lit = sql_quote(json.dumps(d["name"], ensure_ascii=False)) + "::jsonb"
        desc = d.get("description")
        desc_lit = (
            "NULL"
            if desc is None
            else sql_quote(json.dumps(desc, ensure_ascii=False)) + "::jsonb"
        )
        rows.append(
            f"    ({slug_lit}, {kind_lit}, {so}, {name_lit}, {desc_lit})"
        )
    lines.append(",\n".join(rows))

    lines.extend(
        [
            "ON CONFLICT (slug) DO UPDATE SET",
            "    kind             = EXCLUDED.kind,",
            "    sort_order       = EXCLUDED.sort_order,",
            "    name_i18n        = EXCLUDED.name_i18n,",
            "    description_i18n = EXCLUDED.description_i18n;",
            "",
        ]
    )

    out.write_text("\n".join(lines), encoding="utf-8")
    print(f"OK: regenerated {out.relative_to(repo)} ({len(domains)} domains)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
