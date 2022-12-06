import gc


_GRAPH = None

def fill_global_graph():
    # create a moderately large self-referential struct
    global _GRAPH
    _GRAPH = {}
    for i in range(100000):
        _GRAPH[i] = _GRAPH
    _GRAPH = None
    gc.collect()


def busy_loop():
    while True:
        fill_global_graph()

if __name__ == "__main__":
    busy_loop()
