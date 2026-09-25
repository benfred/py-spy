import _thread
import threading
import time


def idle(started):
    started.release()
    time.sleep(100000)


def main():
    # each thread releases the lock from its target, which runs after the
    # thread is added to threading._active
    started = _thread.allocate_lock()
    for i in range(3):
        started.acquire()
        threading.Thread(target=idle, args=(started,), name="NamedThread-%d" % i).start()

    # threads started with _thread are not in threading._active, so they have no name
    for _ in range(2):
        started.acquire()
        _thread.start_new_thread(idle, (started,))

    started.acquire()
    print("READY", flush=True)
    time.sleep(100000)


if __name__ == "__main__":
    main()
