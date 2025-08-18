"""
Test program demonstrating a call stack with a normal method, a staticmethod, and a classmethod.

It also includes functions where the first argument is named `self` but isn't an instance or `cls`
but isn't a class.
"""
import sys
import time


def normal_function():
    normal_function_with_arg(object())


def normal_function_with_arg(something):
    normal_function_with_non_arg_local_called_self()


def normal_function_with_non_arg_local_called_self():
    self = object()
    normal_function_with_non_arg_local_called_cls()


def normal_function_with_non_arg_local_called_cls():
    cls = object
    normal_function_with_a_confusing_self_arg(object())


def normal_function_with_a_confusing_self_arg(self):
    normal_function_with_a_confusing_cls_arg(object)


def normal_function_with_a_confusing_cls_arg(cls):
    SomeClass.class_method()


class SomeClass:
    @classmethod
    def class_method(cls):
        cls.class_method_confusing_first_arg()

    @classmethod
    def class_method_confusing_first_arg(self):
        assert isinstance(
            self, type
        ), "test precondition: `self` should confusingly be a class"
        actual_instance = self()
        actual_instance.normal_method()

    def normal_method(self):
        self.normal_method_confusing_first_arg()

    def normal_method_confusing_first_arg(cls):
        time.sleep(100000)


if __name__ == "__main__":
    normal_function()
