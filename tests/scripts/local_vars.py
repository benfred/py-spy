import time
import numpy as np


def local_variable_lookup(arg1="foo", arg2=None, arg3=True):
    local1 = [-1234, 5678]
    local2 = ("a", "b", "c")
    local3 = 123456789123456789
    local4 = 3.1415
    local5 = {"a": False, "b": (1, 2, 3)}
    # https://github.com/benfred/py-spy/issues/224
    local6 = ("-" * 115, {"key": {"key": {"key": "value"}}})

    # Numpy scalars
    # integers
    local7 = np.bool(True)
    local8 = np.byte(2)

    local9 = np.int8(3)
    local10 = np.int16(42)
    local11 = np.int32(43)
    local12 = np.int64(44)

    local13 = np.uint8(45)
    local14 = np.uint16(46)
    local15 = np.uint32(7)
    local16 = np.uint64(8)

    local17 = np.ulonglong(11)

    # Floats
    local18 = np.float16(0.3)
    local19 = np.float32(0.5)
    local20 = np.float64(0.7)
    local21 = np.longdouble(0.9)

    # Complex
    local22 = np.complex64(0.3+5j)
    local23 = np.complex128(0.3+5j)
    local24 = np.clongdouble(0.3+5j)

    # https://github.com/benfred/py-spy/issues/766
    local25 = "测试1" * 500

    # Empty strings should not be ignored
    local26 = ""

    time.sleep(100000)


if __name__ == "__main__":
    local_variable_lookup()
