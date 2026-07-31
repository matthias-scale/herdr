import json
import unittest

from scripts.watch_pr_checks import CheckState, parse_checks, transitions


class WatchPrChecksTests(unittest.TestCase):
    def test_empty_payload_has_no_checks_yet(self):
        self.assertEqual(parse_checks(""), [])

    def test_parses_check_payload(self):
        checks = parse_checks(
            json.dumps(
                [
                    {
                        "name": "check",
                        "state": "PENDING",
                        "workflow": "CI",
                        "link": "https://example.test/check",
                    }
                ]
            )
        )

        self.assertEqual(
            checks,
            [
                CheckState(
                    name="check",
                    workflow="CI",
                    state="PENDING",
                    link="https://example.test/check",
                )
            ],
        )

    def test_emits_only_new_or_changed_states(self):
        previous = {
            ("CI", "linux"): "SUCCESS",
            ("CI", "windows"): "PENDING",
        }
        checks = [
            CheckState("linux", "CI", "SUCCESS", ""),
            CheckState("windows", "CI", "SUCCESS", ""),
            CheckState("docs", "Website", "PENDING", ""),
        ]

        self.assertEqual(transitions(previous, checks), checks[1:])


if __name__ == "__main__":
    unittest.main()
