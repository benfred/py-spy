import os
import time
import multiprocessing

def child():
    time.sleep(5000)

def child_with_subchild():
    subchild = multiprocessing.Process(target=child)
    subchild.start()
    time.sleep(5000)
    subchild.join()

if __name__ == "__main__":
    first = multiprocessing.Process(target=child)
    second = multiprocessing.Process(target=child_with_subchild)

    first.start()
    second.start()

    first.join()
    second.join()
