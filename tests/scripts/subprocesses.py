import time
import multiprocessing

def target():
    multiprocessing.freeze_support()
    time.sleep(1000)

def main():
    # Use 'fork' start method for consistent behavior across Python versions
    # (Python 3.14+ defaults to 'forkserver' which creates an extra process)
    ctx = multiprocessing.get_context('fork')
    child1 = ctx.Process(target=target)
    child1.start()
    child2 = ctx.Process(target=target)
    child2.start()
    time.sleep(10000)
    child1.join()
    child2.join()

if __name__ == "__main__":
    multiprocessing.freeze_support()
    main()