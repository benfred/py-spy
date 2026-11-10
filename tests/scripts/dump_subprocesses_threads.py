import os
import subprocess
import sys
import threading
import time


def _worker():
    while True:
        time.sleep(0.05)


def main():
    for _ in range(4):
        threading.Thread(target=_worker, daemon=True).start()

    child = subprocess.Popen(
        [sys.executable, "-c", "import time\nwhile True: time.sleep(1)"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    print("PID_PARENT=%d" % os.getpid(), flush=True)
    print("PID_CHILD=%d" % child.pid, flush=True)
    print("READY", flush=True)

    try:
        child.wait()
    finally:
        if child.poll() is None:
            child.kill()


if __name__ == "__main__":
    main()
