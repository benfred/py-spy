import threading
import time

while True:
    th = threading.Thread(target = lambda: time.sleep(.5))
    th.start()
    th.join()
