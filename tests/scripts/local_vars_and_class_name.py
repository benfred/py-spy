"""Combining local variable and class name lookup testing."""
import time


class ClassName:
    def __str__(self):
        return "a"

    def local_variable_lookup(self, arg1="foo"):
        local1 = "a"
        local2 = 2.71828
        local3 = {}
        time.sleep(100000)


if __name__ == "__main__":
    ClassName().local_variable_lookup()
