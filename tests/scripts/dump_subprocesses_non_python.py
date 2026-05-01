import os
import subprocess
import sys
import time


def main():
    # A non-Python child (a long-running shell sleep). py-spy should skip
    # this rather than aborting the whole dump.
    non_python = subprocess.Popen(
        ["sh", "-c", "sleep 3600"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    # A Python child so we can verify it still gets dumped after the
    # non-Python sibling.
    python_child = subprocess.Popen(
        [sys.executable, "-c", "import time\nwhile True: time.sleep(1)"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    print("PID_PARENT=%d" % os.getpid(), flush=True)
    print("PID_NON_PYTHON=%d" % non_python.pid, flush=True)
    print("PID_PYTHON_CHILD=%d" % python_child.pid, flush=True)
    print("READY", flush=True)

    try:
        while True:
            time.sleep(1)
    finally:
        for child in (non_python, python_child):
            if child.poll() is None:
                child.kill()


if __name__ == "__main__":
    main()
