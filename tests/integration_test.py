from __future__ import print_function

import json
import os
import subprocess
import sys
import re
import tempfile
import unittest
from collections import defaultdict, namedtuple
from shutil import which

Frame = namedtuple("Frame", ["file", "name", "line", "col"])

# disable gil checks on windows - just rely on active
# (doesn't seem to be working quite right - TODO: investigate)
GIL = ["--gil"] if not sys.platform.startswith("win") else []

PYSPY = which("py-spy")


class TestPyspy(unittest.TestCase):
    """Basic tests of using py-spy as a commandline application"""

    def _sample_process(self, script_name, options=None, include_profile_name=False):
        if not PYSPY:
            raise ValueError("Failed to find py-spy on the path")

        # for permissions reasons, we really want to run the sampled python process as a
        # subprocess of the py-spy (works best on linux etc). So we're running the
        # record option, and setting different flags. To get the profile output
        # we're using the speedscope format (since we can read that in as json)
        with tempfile.NamedTemporaryFile() as profile_file:
            filename = profile_file.name
            if sys.platform.startswith("win"):
                filename = "profile.json"

            cmdline = [
                PYSPY,
                "record",
                "-o",
                filename,
                "--format",
                "speedscope",
                "-d",
                "2",
            ]
            cmdline.extend(options or [])
            cmdline.extend(["--", sys.executable, script_name])
            env = dict(os.environ, RUST_LOG="info")
            subprocess.check_output(cmdline, env=env)
            with open(filename) as f:
                profiles = json.load(f)

        frames = profiles["shared"]["frames"]
        samples = defaultdict(int)
        for p in profiles["profiles"]:
            for sample in p["samples"]:
                if include_profile_name:
                    samples[
                        tuple(
                            [p["name"]] + [Frame(**frames[frame]) for frame in sample]
                        )
                    ] += 1
                else:
                    samples[tuple(Frame(**frames[frame]) for frame in sample)] += 1
        return samples

    def test_longsleep(self):
        # running with the gil flag should have ~ no samples returned
        if GIL:
            profile = self._sample_process(_get_script("longsleep.py"), GIL)
            print(profile)
            assert sum(profile.values()) <= 10

        # running with the idle flag should have > 95%  of samples in the sleep call
        profile = self._sample_process(_get_script("longsleep.py"), ["--idle"])
        sample, count = _most_frequent_sample(profile)
        assert count >= 95
        assert len(sample) == 2
        assert sample[0].name == "<module>"
        assert sample[0].line == 9
        assert sample[1].name == "longsleep"
        assert sample[1].line == 5

    def test_busyloop(self):
        # can't be sure what line we're on, but we should have ~ all samples holding the gil
        profile = self._sample_process(_get_script("busyloop.py"), GIL)
        assert sum(profile.values()) >= 95

    def test_thread_names(self):
        # we don't support getting thread names on python < 3.6
        v = sys.version_info
        if v.major < 3 or v.minor < 6:
            return

        for _ in range(3):
            profile = self._sample_process(
                _get_script("thread_names.py"),
                ["--threads", "--idle"],
                include_profile_name=True,
            )
            expected_thread_names = set("CustomThreadName-" + str(i) for i in range(10))
            expected_thread_names.add("MainThread")
            name_re = re.compile(r"\"(.*)\"")
            actual_thread_names = {name_re.search(p[0]).groups()[0] for p in profile}
            if expected_thread_names == actual_thread_names:
                break
        if expected_thread_names != actual_thread_names:
            print(
                "failed to get thread names",
                expected_thread_names,
                actual_thread_names,
            )

        assert expected_thread_names == actual_thread_names

    def test_shell_completions(self):
        cmdline = [PYSPY, "completions", "bash"]
        subprocess.check_output(cmdline)


def _get_script(name):
    base_dir = os.path.dirname(__file__)
    return os.path.join(base_dir, "scripts", name)


def _most_frequent_sample(samples):
    frames, count = max(samples.items(), key=lambda x: x[1])
    # lets normalize as a percentage here, rather than raw number of samples
    return frames, int(100 * count / sum(samples.values()))


if __name__ == "__main__":
    print("Testing py-spy @", PYSPY)
    unittest.main()
