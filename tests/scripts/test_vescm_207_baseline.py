import importlib.util
import json
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[2] / "scripts" / "benchmark-vescm-207-baseline.py"
SPEC = importlib.util.spec_from_file_location("vescm207", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class BaselineHelpersTest(unittest.TestCase):
    def test_parse_sse_ignores_keepalive_and_malformed_events(self):
        body = b"data: \nid: 0\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n\ndata: not-json\n"
        self.assertEqual(MODULE.parse_sse(body), [{"jsonrpc": "2.0", "id": 3, "result": {}}])

    def test_completeness_requires_reviewed_path_and_terms(self):
        query = {
            "gold": [
                {"id": "one", "repository": "refloat", "path": "doc/commands/REALTIME_DATA.md", "required_terms": ["REALTIME_DATA"]}
            ]
        }
        result = {
            "id": "hit",
            "name": "REALTIME_DATA",
            "summary": "command",
            "source": {"repository": "refloat", "path": "doc/commands/REALTIME_DATA.md", "revision": "r1"},
            "passage": "REALTIME_DATA command",
        }
        self.assertTrue(MODULE.completeness(query, [result])["complete"])
        result["source"]["path"] = "doc/commands/OTHER.md"
        self.assertFalse(MODULE.completeness(query, [result])["complete"])

    def test_digest_excludes_transport_metrics(self):
        searches = [{"id": "q", "mode": "lexical", "detail": "full", "snapshot_id": "s", "ordered_results": [{"id": "r"}], "warning_codes": []}]
        left = MODULE.normalized_digest(searches)
        searches[0]["latency_us"] = 12345
        searches[0]["response_bytes"] = 99
        self.assertEqual(left, MODULE.normalized_digest(searches))

    def test_observed_trace_is_distinct_from_bounded_evaluation(self):
        query = {"observed_manual_reads": ["motor.rs", "raw.rs"]}
        judged = {"complete": True}
        observed = MODULE.trace_fallback(query, judged)
        self.assertEqual(observed["kind"], "observed_trace")
        self.assertEqual(observed["manual_file_reads"], 2)
        evaluated = MODULE.trace_fallback({}, judged)
        self.assertEqual(evaluated["kind"], "bounded_response_eval")
        self.assertEqual(evaluated["manual_file_reads_required_by_evaluation"], 0)


if __name__ == "__main__":
    unittest.main()
