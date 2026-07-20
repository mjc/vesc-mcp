# VESCM-197 local planner bakeoff

This bounded run compares deterministic Rust planning with three exact Q4_K_M
model artifacts on the RX 5700 XT. Each model sees the same three locked cases
twice at temperature 0 and seed 42. Outputs are capped at 400 tokens and pass
the same bounded structural checks as the Rust planner boundary.

| Candidate | Schema valid | Query quality | Repeatable | Warm p50 |
|---|---:|---:|---:|---:|
| hard-coded contract | 100% | 100% | 100% | no model call |
| Granite 4.1 3B | 100% | 33% | 100% | 0.87 s |
| Nanbeige 4.1 3B | 0% | 0% | 100% | 3.69 s |
| Qwen 3.5 4B | 100% | 100% | 100% | 2.27 s |

All model logs identify `Vulkan0` as the RX 5700 XT 50th Anniversary and show
every model layer offloaded to it. Nanbeige reaches the 400-token ceiling while
leaving the response content empty. Qwen ties rather than improves the
deterministic baseline. Consequently, no model becomes the default.

Run `python scripts/verify-planner-bakeoff.py` to re-score every saved output,
verify prompt and model pins, prove GPU offload from the logs, and enforce the
selection decision.
