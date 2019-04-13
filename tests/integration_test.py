import os
import re
import subprocess
import sys
import time
import unittest
from collections import namedtuple

Frame = namedtuple("Frame", ("function", "file", "line"))
Thread = namedtuple("Thread", ("id", "status"))


class IntegrationTest(unittest.TestCase):
    def _profile_python_file(self, filename):
        # Run the python command in a subprocess
        python_process = subprocess.Popen(
            [sys.executable, os.path.join("scripts", filename)]
        )
        try:
            # hack: give it some time to get running
            time.sleep(0.2)

            # Run py-spy on the pid of the process we just created
            # TODO: get built py-spy here (rather than globally installed)
            output = subprocess.check_output(
                ["py-spy", "--pid", str(python_process.pid), "--dump"]
            )

            if sys.version_info[0] >= 3:
                output = output.decode("utf8")

            traces = []
            for thread in output.split("\nThread")[1:]:
                lines = thread.split("\n")
                traces.append(
                    (parse_thread(lines[0]), [parse_frame(l) for l in lines[1:] if l])
                )
            return traces

        finally:
            python_process.kill()
            python_process.wait()

    def test_basic(self):
        traces = self._profile_python_file("longsleep.py")
        self.assertEqual(len(traces), 1)

        thread, frames = traces[0]
        self.assertEqual(
            frames,
            [
                Frame(function="longsleep", file="longsleep.py", line=5),
                Frame(function="<module>", file="longsleep.py", line=9),
            ],
        )

    def test_gil(self):
        traces = self._profile_python_file("busyloop.py")
        self.assertEqual(len(traces), 1)
        thread, frames = traces[0]
        assert "gil" in thread.status

        traces = self._profile_python_file("longsleep.py")
        self.assertEqual(len(traces), 1)
        thread, frames = traces[0]
        assert "gil" not in thread.status


def parse_frame(frame_line):
    matches = re.match(
        r"\s+(?P<function>\S+) .(?P<file>\S+):(?P<line>\d+).", frame_line
    )
    if not matches:
        return None
    frame = matches.groupdict()
    frame["line"] = int(frame["line"])
    return Frame(**frame)


def parse_thread(thread_line):
    matches = re.match(r"\s*(?P<id>0[xX][0-9a-fA-f]+) \((?P<status>\S+)\)", thread_line)
    if not matches:
        return None
    return Thread(**matches.groupdict())


if __name__ == "__main__":
    unittest.main()
