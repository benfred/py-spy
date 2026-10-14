import sys
import threading
import time


class Name1(str):
    pass


class Name2(Name1):
    pass


class Name3(Name2):
    pass


def main():
    depth = int(sys.argv[1])
    name_type = [str, Name1, Name2, Name3][depth]
    th = threading.Thread(target=lambda: time.sleep(10000))
    # Thread.name converts with str(), so set _name to keep the subclass
    th._name = name_type("SubclassName-%s" % depth)
    th.start()
    time.sleep(10000)


if __name__ == "__main__":
    main()
