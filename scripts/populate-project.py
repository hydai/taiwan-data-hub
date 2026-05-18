#!/usr/bin/env python3
"""
Populate the "Taiwan Data Hub Roadmap" Project v2 with all 80 issues and set
Estimate / Component / Priority custom fields based on issue labels + milestone.

Idempotent enough: gh project item-add silently no-ops if the issue is already
in the project. Re-running just refreshes field values.

Usage:
    python3 scripts/populate-project.py
"""

import json
import subprocess
import sys
import time
from typing import Optional

OWNER = "hydai"
REPO = "hydai/taiwan-data-hub"
PROJECT_NUMBER = "2"
PROJECT_ID = "PVT_kwHOACpetM4BYDsI"

ESTIMATE_FIELD = "PVTSSF_lAHOACpetM4BYDsIzhTMJfA"
ESTIMATE_OPTS = {
    "est:xs": "ca61c403",
    "est:s":  "24e9a787",
    "est:m":  "70836a6b",
    "est:l":  "b84655e3",
    "est:xl": "bf1f30e3",
}

COMPONENT_FIELD = "PVTSSF_lAHOACpetM4BYDsIzhTMJfE"
COMPONENT_OPTS = {
    "backend":   "29cd200e",
    "frontend":  "39e2273e",
    "mcp":       "4f529fd6",
    "etl":       "b4b0951e",
    "infra":     "45fdb44f",
    "docs":      "cf8d8bca",
    "i18n":      "0f08f621",
    "community": "4bc02d3a",
    "security":  "012dc8a6",
}
# When an issue has multiple component labels, pick the most specific.
COMPONENT_PRIORITY = ["mcp", "etl", "i18n", "community", "security",
                      "backend", "frontend", "infra", "docs"]

PRIORITY_FIELD = "PVTSSF_lAHOACpetM4BYDsIzhTMJfM"
PRIORITY_OPTS = {
    "P0": "6d4f8355",
    "P1": "74512fd4",
    "P2": "3290e0b5",
}

# Milestone → priority assignment based on MVP definition (v0.1 = M0+M1+M2).
def priority_for_milestone(title: str) -> Optional[str]:
    if title.startswith(("M0", "M1", "M2")):
        return "P0"
    if title.startswith(("M3", "M4", "M5")):
        return "P1"
    if title.startswith(("M6", "M7")):
        return "P2"
    return None


def run(cmd: list[str]) -> str:
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(f"FAIL: {' '.join(cmd)}\n{result.stderr}\n")
        result.check_returncode()
    return result.stdout


def fetch_issues() -> list[dict]:
    raw = run([
        "gh", "issue", "list",
        "-R", REPO,
        "--limit", "200",
        "--state", "all",
        "--json", "number,title,labels,milestone,url",
    ])
    return json.loads(raw)


def add_to_project(issue_url: str) -> str:
    raw = run([
        "gh", "project", "item-add", PROJECT_NUMBER,
        "--owner", OWNER,
        "--url", issue_url,
        "--format", "json",
    ])
    return json.loads(raw)["id"]


def set_field(item_id: str, field_id: str, option_id: str) -> None:
    run([
        "gh", "project", "item-edit",
        "--id", item_id,
        "--project-id", PROJECT_ID,
        "--field-id", field_id,
        "--single-select-option-id", option_id,
    ])


def main() -> int:
    issues = fetch_issues()
    print(f"Found {len(issues)} issues. Adding to project + setting fields…\n")

    success = 0
    for issue in issues:
        num = issue["number"]
        labels = {l["name"] for l in issue["labels"]}
        ms = issue["milestone"]["title"] if issue["milestone"] else ""

        # Pick estimate
        est_opt = None
        for tag, oid in ESTIMATE_OPTS.items():
            if tag in labels:
                est_opt = oid
                break

        # Pick component (most specific first)
        comp_opt = None
        for comp in COMPONENT_PRIORITY:
            if comp in labels:
                comp_opt = COMPONENT_OPTS[comp]
                break

        # Pick priority from milestone
        prio_key = priority_for_milestone(ms)
        prio_opt = PRIORITY_OPTS[prio_key] if prio_key else None

        try:
            item_id = add_to_project(issue["url"])
            if est_opt:
                set_field(item_id, ESTIMATE_FIELD, est_opt)
            if comp_opt:
                set_field(item_id, COMPONENT_FIELD, comp_opt)
            if prio_opt:
                set_field(item_id, PRIORITY_FIELD, prio_opt)
            print(f"  ✓ #{num:<3}  {issue['title'][:55]:<55}  "
                  f"[ms={ms[:6]:<6} est={prio_key or '?'}/{[k for k,v in ESTIMATE_OPTS.items() if v==est_opt][0].split(':')[1] if est_opt else '?'}]")
            success += 1
        except subprocess.CalledProcessError as e:
            print(f"  ✗ #{num}  {e}")

        # Gentle pacing to avoid secondary rate limit
        time.sleep(0.15)

    print(f"\n✅ {success} / {len(issues)} issues processed.")
    return 0 if success == len(issues) else 1


if __name__ == "__main__":
    sys.exit(main())
