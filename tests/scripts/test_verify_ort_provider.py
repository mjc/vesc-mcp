import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[2] / "scripts" / "verify-ort-provider.py"


class VerifyOrtProviderTests(unittest.TestCase):
    def run_log(self, log: str):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".log") as file:
            file.write(log)
            file.flush()
            return subprocess.run(
                [sys.executable, SCRIPT, file.name, "--provider", "CoreMLExecutionProvider"],
                text=True,
                capture_output=True,
                check=False,
            )

    def test_accepts_proven_accelerator_placement(self):
        result = self.run_log(
            'semantic-runtime: {"selected_provider":"CoreMLExecutionProvider",'
            '"provider_availability":["CoreMLExecutionProvider=true"]}\n'
            "All nodes placed on CoreMLExecutionProvider\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_cpu_fallback(self):
        result = self.run_log(
            'semantic-runtime: {"selected_provider":"CoreMLExecutionProvider",'
            '"provider_availability":["CoreMLExecutionProvider=true"]}\n'
            "node assigned to CoreMLExecutionProvider\n"
            "node assigned to CPUExecutionProvider\n"
        )
        self.assertNotEqual(result.returncode, 0)

    def test_rejects_registration_without_placement(self):
        result = self.run_log(
            'semantic-runtime: {"selected_provider":"CoreMLExecutionProvider",'
            '"provider_availability":["CoreMLExecutionProvider=true"]}\n'
        )
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
