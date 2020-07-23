import time


def f():
    [
        # Must be split over multiple lines to see the error.
        # https://github.com/benfred/py-spy/pull/208
        time.sleep(1)
        for _ in range(1000)
    ]


f()
