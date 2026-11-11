import threading
import time


def sleep_forever():
    print("READY", flush=True)
    print(threading.get_native_id(), flush=True)
    time.sleep(600)


sleep_forever()
