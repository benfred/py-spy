import json
import subprocess
import sys
import unittest
import tempfile
import os
from collections import defaultdict, namedtuple
from distutils.spawn import find_executable


Frame = namedtuple("Frame", ["file", "name", "line", "col"])


class TestPyspy(unittest.TestCase):
    """ Basic tests of using py-spy as a commandline application """
    def _sample_process(self, script_name, options=None):
        pyspy = find_executable("py-spy")
        print("Testing py-spy @", pyspy)

        # for permissions reasons, we really want to run the sampled python process as a
        # subprocess of the py-spy (works best on linux etc). So we're running the 
        # record option, and setting different flags. To get the profile output
        # we're using the speedscope format (since we can read that in as json)
        with tempfile.NamedTemporaryFile() as profile_file:
            cmdline = [
                pyspy,
                "record",
                "-o",
                profile_file.name,
                "--format",
                "speedscope",
                "-d",
                "1",
            ]
            cmdline.extend(options or [])
            cmdline.extend(["--", sys.executable, script_name])

            subprocess.check_call(cmdline)
            with open(profile_file.name) as f:
                profiles = json.load(f)
        
        frames = profiles["shared"]["frames"]
        samples = defaultdict(int)
        for p in profiles["profiles"]:
            for sample in p["samples"]:
                samples[tuple(Frame(**frames[frame]) for frame in sample)] += 1
        return samples

    def test_longsleep(self):
        # running with the gil flag should have ~ no samples returned
        profile = self._sample_process(_get_script("longsleep.py"), ["--gil"])
        assert sum(profile.values()) <= 1

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
        profile = self._sample_process(_get_script("busyloop.py"), ["--gil"])
        print(profile)
        assert sum(profile.values()) >= 95



def _get_script(name):
    base_dir = os.path.dirname(__file__)
    return os.path.join(base_dir, "scripts", name)


def _most_frequent_sample(samples):
    return max(samples.items(), key=lambda x: x[1])

if __name__ == "__main__":
    unittest.main()
