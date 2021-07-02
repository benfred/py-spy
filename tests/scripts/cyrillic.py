import time


def f(seconds):
    time.sleep(seconds)


def кириллица(seconds):
    f(seconds)


if __name__ == "__main__":
    f(3)
    кириллица(3)
    f(3)
