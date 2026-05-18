#!/usr/bin/env python3
"""
Regenerate migrations/0002_seed_domains.sql from config/domains.yaml.

Run after editing the YAML; commit both files together so the seed
migration and its source of truth stay in lockstep.

Requires PyYAML (`pip install pyyaml` or use a venv).
"""

from __future__ import annotations

import json
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


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    src = repo / "config" / "domains.yaml"
    out = repo / "migrations" / "0002_seed_domains.sql"

    domains = yaml.safe_load(src.read_text())
    if not isinstance(domains, list) or not domains:
        sys.stderr.write(f"{src} produced no domains\n")
        return 1

    domains.sort(key=lambda d: d["sort_order"])

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
        slug = d["slug"]
        kind = d["kind"]
        sort_order = d["sort_order"]
        name_lit = "$json$" + json.dumps(d["name"], ensure_ascii=False) + "$json$::jsonb"
        desc = d.get("description")
        desc_lit = (
            "NULL"
            if desc is None
            else "$json$" + json.dumps(desc, ensure_ascii=False) + "$json$::jsonb"
        )
        rows.append(
            f"    ('{slug}', '{kind}', {sort_order}, {name_lit}, {desc_lit})"
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

    out.write_text("\n".join(lines))
    print(f"✓ {out.relative_to(repo)} regenerated with {len(domains)} domains")
    return 0


if __name__ == "__main__":
    sys.exit(main())
