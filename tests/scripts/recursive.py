def recursive(x):
    if x == 0:
        return
    recursive(x-1)


while True:
    recursive(20)
