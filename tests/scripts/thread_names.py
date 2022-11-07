import threading
import time


def main():
    for i in range(10):
        th = threading.Thread(target = lambda: time.sleep(10000))
        th.name = f"CustomThreadName-{i}"
        th.start()
    time.sleep(10000)

if __name__ == "__main__":
    main()
