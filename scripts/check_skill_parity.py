#!/usr/bin/env python3
"""Read-only comparison of public Skill definitions against the Python project."""

from __future__ import annotations

import argparse
import difflib
from pathlib import Path


PRIVATE_CORPUS = "语料/3. 学生案例/*.md"
PUBLIC_CORPUS = "corpus/examples/*.md"
PYTHON_CHOICE_RULE = "学生选择后，必须追问为什么选这个，以及没选的方向哪里不合适。"
RUST_CHOICE_RULE = (
    "学生选择后，只追问一个最有信息价值的理由；候选方向有重叠时允许组合或开放回答，"
    "不强迫解释所有未选项。"
)
PYTHON_CHOICE_FLOW = "先确认学生选择了什么，再追问为什么选它，以及没选的路径哪里不合适。"
RUST_CHOICE_FLOW = "先确认学生选择了什么，再只追问一个选择理由；允许组合方向或用自己的方式回答。"


def normalized(text: str, *, python_side: bool) -> str:
    if python_side:
        text = text.replace(PRIVATE_CORPUS, PUBLIC_CORPUS)
        text = text.replace(PYTHON_CHOICE_RULE, RUST_CHOICE_RULE)
        text = text.replace(PYTHON_CHOICE_FLOW, RUST_CHOICE_FLOW)
    lines = text.rstrip().splitlines()
    seen_public_corpus = False
    result: list[str] = []
    for line in lines:
        if line.strip() == f"- {PUBLIC_CORPUS}":
            if seen_public_corpus:
                continue
            seen_public_corpus = True
        result.append(line)
    return "\n".join(result) + "\n"


def yaml_files(root: Path) -> dict[str, Path]:
    return {str(path.relative_to(root)): path for path in root.rglob("*.yaml")}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--python-root", required=True, type=Path)
    parser.add_argument(
        "--rust-root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    args = parser.parse_args()
    python_skills = args.python_root.resolve() / "skills"
    rust_skills = args.rust_root.resolve() / "skills"
    left = yaml_files(python_skills)
    right = yaml_files(rust_skills)
    failures: list[str] = []

    if left.keys() != right.keys():
        failures.append(
            f"Skill file sets differ: Python-only={sorted(left.keys() - right.keys())}, "
            f"Rust-only={sorted(right.keys() - left.keys())}"
        )
    for relative in sorted(left.keys() & right.keys()):
        python_text = normalized(left[relative].read_text(encoding="utf-8"), python_side=True)
        rust_text = normalized(right[relative].read_text(encoding="utf-8"), python_side=False)
        if python_text != rust_text:
            failures.extend(
                difflib.unified_diff(
                    python_text.splitlines(),
                    rust_text.splitlines(),
                    fromfile=f"python/{relative}",
                    tofile=f"rust/{relative}",
                    lineterm="",
                )
            )
    if failures:
        print("\n".join(failures))
        return 1
    print(f"Skill parity OK: {len(left)} public YAML definitions checked (read-only).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
