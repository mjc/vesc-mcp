#!/usr/bin/env python3
"""Check that the VESCM-179 design retains every acceptance decision."""

from pathlib import Path


text = " ".join(
    (Path(__file__).parents[1] / "docs/codegraphrag-design.md").read_text().split()
)

required_edges = (
    "declaration → definition", "caller → callee", "include/import → imported file",
    "command ID → handler", "protocol → implementation", "QML → backend",
    "package → firmware API", "configuration → runtime consumer",
    "parser → produced artifact", "generated → source file",
)
for edge in required_edges:
    assert edge in text, edge

for requirement in (
    "LightRAG findings", "Deterministic local and global semantics", "Immutable artifact",
    "Memory and cache locality", "Candidate generation, fusion, and ordering",
    "Result provenance", "Evaluation and go/no-go gates", "Architecture comparison",
    "Explicit rejection list", "Phased implementation", "one graph hop",
    "four outgoing edges", "24 graph-derived candidates", "FrontierShortcutRate",
    "byte-identical rebuilds", "The no-LLM graph", "Runtime graph mutation",
):
    assert requirement in text, requirement

assert "HashMap<NodeId, Vec<Edge>>" in text
assert "graph database" in text
assert "p95 below 2 ms" in text
assert "32 core bytes per edge" in text
print("VESCM-179 deterministic CodeGraphRAG design verified")
