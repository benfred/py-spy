import sys
import time


def idle(started):
    started.set()
    time.sleep(100000)


def main():
    print("READY", flush=True)

    # py-spy usually starts sampling before the program imports threading
    sys.stdin.readline()
    import threading

    print("IMPORTED", flush=True)

    sys.stdin.readline()
    started = threading.Event()
    threading.Thread(target=idle, args=(started,), name="LateThread").start()
    started.wait()
    print("STARTED", flush=True)

    time.sleep(100000)


if __name__ == "__main__":
    main()
