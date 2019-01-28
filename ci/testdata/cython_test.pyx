""" simple test file for cython source mapping: defines a function
that uses newtowns method to compute the square root of a number """

from cython cimport floating

cpdef sqrt(floating value):
    # solve for the square root of value by finding the zeros of
    #   'x * x - value = 0' using newtons meethod
    cdef double x = value / 2
    for _ in range(8):
        x -= (x * x - value) / (2 * x)
    return x
