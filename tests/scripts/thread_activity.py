import hashlib
import threading
import time


def hash_value():
    while True:
        # A long C call releases the GIL but keeps this thread on CPU.
        hashlib.pbkdf2_hmac("sha256", b"password", b"salt", 10000000)


def sleep_forever():
    # No threading.py frame: only the OS activity lookup can detect this sleep.
    time.sleep(600)


def main():
    threading.Thread(target=hash_value, daemon=True).start()
    threading.Thread(target=sleep_forever, daemon=True).start()
    threading.Event().wait()


main()
