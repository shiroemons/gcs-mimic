import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("freshness", Path(__file__).with_name("check-dependencies.py"))
freshness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(freshness)


class FreshnessTests(unittest.TestCase):
    def test_stable_including_major_updates_ignores_yanked_and_prerelease(self):
        index = '\n'.join([
            '{"vers":"1.9.0","yanked":false}',
            '{"vers":"2.0.0","yanked":false}',
            '{"vers":"3.0.0-beta.1","yanked":false}',
            '{"vers":"2.1.0","yanked":true}',
        ])
        self.assertEqual(freshness.latest_crate(index), "2.0.0")
        with self.assertRaises(ValueError):
            freshness.latest_crate('{"vers":"1.0.0","yanked":true}')

    def test_official_sparse_index_paths(self):
        for name, expected in [("a", "1/a"), ("ab", "2/ab"), ("abc", "3/a/abc"), ("serde_json", "se/rd/serde_json")]:
            self.assertEqual(freshness.index_path(name), expected)

    def test_drift_does_not_claim_current(self):
        self.assertFalse(freshness.compare("test", "1.0.0", "1.0.0"))
        self.assertTrue(freshness.compare("test", "1.0.0", "2.0.0"))

    def test_full_check_current_drift_and_network_error(self):
        import json
        root = freshness.ROOT
        status = json.loads((root / "compatibility/dependency-status.json").read_text())
        action = status["github_actions"][0]
        pin = status["toolchain"]["version"]
        urls = {
            "https://static.rust-lang.org/dist/channel-rust-stable.toml": f'[pkg.rust]\nversion = "{pin} (hash date)"',
            "https://index.crates.io/se/rd/serde_json": '{"vers":"1.0.154","yanked":false}',
            f'https://api.github.com/repos/{action["repository"]}/releases/latest': '{"tag_name":"v7.0.1"}',
            f'https://api.github.com/repos/{action["repository"]}/git/ref/tags/v7.0.1': json.dumps({"object": {"type": "commit", "sha": action["sha"]}}),
        }
        metadata = json.dumps({"packages": [{"dependencies": [{"name": "serde_json", "source": "registry+https://github.com/rust-lang/crates.io-index"}]}]})
        with patch.object(freshness.subprocess, "check_output", return_value=metadata):
            self.assertEqual(freshness.check(get=urls.__getitem__), 0)
            urls["https://index.crates.io/se/rd/serde_json"] = '{"vers":"2.0.0","yanked":false}'
            self.assertEqual(freshness.check(get=urls.__getitem__), 1)
        def failure(url):
            raise OSError("network unavailable")
        with self.assertRaises(OSError):
            freshness.check(get=failure)


if __name__ == "__main__":
    unittest.main()
