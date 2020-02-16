import time


def local_variable_lookup(arg1="foo", arg2=None, arg3=True):
    local1 = [-1234, 5678]
    local2 = ("a", "b", "c")
    local3 = 123456789123456789
    local4 = 3.1415
    local5 = {"a": False, "b": (1, 2, 3)}
    # https://github.com/benfred/py-spy/issues/224
    local6 = ("-" * 115, {"key": {"key": {"key": "value"}}})
    time.sleep(100000)


if __name__ == "__main__":
    local_variable_lookup()
