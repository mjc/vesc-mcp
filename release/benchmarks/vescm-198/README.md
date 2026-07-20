# VESCM-198 Bonsai planner and critic bakeoff

This bounded RX 5700 XT run tests exact low-bit Bonsai artifacts with exact
runtime commits. It does not use publisher benchmark claims as local evidence.

The llama.cpp mainline control at `6f4f53f` rejects the ticket-named 8B
`Q2_0_g64` tensor type. The publisher's Vulkan fork at
`9fcaed763ccda38ea81068ad9d7f991aaddca451` recognizes that type but rejects the
same verified file because its `output_norm.weight` tensor offset is
inconsistent. The publisher's recommended group-128 fallback loads all 37
layers on Vulkan0; the 27B Q1_0 model loads all 65 layers.

| Operating point | Schema valid | Query quality | Repeatable | Warm p50 |
|---|---:|---:|---:|---:|
| hard-coded contract | 100% | 100% | 100% | no model call |
| Bonsai 27B planner | 0% | 0% | 100% | 3.54 s |
| Ternary Bonsai 8B group-128 fallback | 33% | 33% | 100% | 1.75 s |
| Nanbeige + Bonsai 27B critic | 0% | 0% | 100% | 6.90 s |

The 27B runtime itself is viable: it sustains about 24.3 decode tokens/s, uses
4,250 MiB of Vulkan allocation at the 8K planner context, and does not CPU
offload model layers. Its outputs are not viable: it adds Markdown fences,
uses malformed query objects, and invents a concern for complete evidence.

The critic contract has no completeness-approval field. Since no Bonsai
operating point passes the locked gate, escalation remains disabled and the
observed production model-call rate is zero.

Run `python scripts/verify-bonsai-bakeoff.py` to reconstruct prompts and scores,
check exact pins and load failures, prove full GPU offload, and enforce the
disabled-default decision.
