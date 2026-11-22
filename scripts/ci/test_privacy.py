"""Tests for privacy.gate's refusal to gate on patterns alone in CI. Run: python -m unittest discover -s scripts/ci"""
import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

import privacy

CI_PUSH = {"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "push"}
CI_PULL_REQUEST = {"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "pull_request", "KIKU_FORK_PR": "false"}
CI_FORK_PULL_REQUEST = {"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "pull_request", "KIKU_FORK_PR": "true"}


class GateTest(unittest.TestCase):
    def gate(self, env):
        """Run privacy.gate with only `env` set, no `.private-words` and the gate stubbed."""
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as folder:
            patches = [mock.patch.dict("os.environ", env, clear=True), mock.patch.object(privacy, "words_file", return_value=None),
                       mock.patch.object(privacy.subprocess, "run", return_value=mock.Mock(returncode=0, stdout="ok\n", stderr=""))]
            run = [patch.start() for patch in patches][2]
            try:
                with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
                    code = privacy.gate("a", "b")
            finally:
                for patch in patches:
                    patch.stop()
        return code, err.getvalue(), run

    def test_a_ci_push_without_words_fails_and_runs_nothing(self):
        for words in ({}, {"KIKU_PRIVATE_WORDS": ""}, {"KIKU_PRIVATE_WORDS": " , "}):
            code, err, run = self.gate({**CI_PUSH, **words})
            self.assertEqual(code, 1)
            self.assertIn("privacy: no private words (KIKU_PRIVATE_WORDS is empty and there is no .private-words): refusing to gate with patterns only", err)
            run.assert_not_called()

    def test_a_same_repository_pull_request_without_words_fails_too(self):
        for env in (CI_PULL_REQUEST, {"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "pull_request"}):
            code, err, run = self.gate(env)
            self.assertEqual(code, 1)
            self.assertIn("no private words", err)
            run.assert_not_called()

    def test_a_fork_pull_request_without_words_gates_the_patterns(self):
        code, _, run = self.gate(CI_FORK_PULL_REQUEST)
        self.assertEqual(code, 0)
        run.assert_called_once()

    def test_with_words_it_gates_every_path_with_them(self):
        code, _, run = self.gate({**CI_PUSH, "KIKU_PRIVATE_WORDS": "alpha,beta"})
        self.assertEqual(code, 0)
        self.assertEqual(run.call_args.args[0][-2:], ["a", "b"])
        self.assertEqual(run.call_args.kwargs["env"]["KIKU_PRIVATE_WORDS"], "alpha,beta")

    def test_a_local_run_without_words_gates_the_patterns(self):
        self.assertEqual(self.gate({})[0], 0)

    def test_workflow_dispatch_and_schedule_need_words_too(self):
        self.assertTrue(privacy.needs_words({"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "workflow_dispatch"}))
        self.assertFalse(privacy.needs_words({"GITHUB_EVENT_NAME": "push"}))
        self.assertFalse(privacy.needs_words(CI_FORK_PULL_REQUEST))
        self.assertTrue(privacy.needs_words(CI_PUSH))


if __name__ == "__main__":
    unittest.main()
